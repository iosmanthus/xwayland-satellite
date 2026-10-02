//! The focus state machine (design §1.2, §2): where X focus goes, given where the
//! compositor's keyboard focus is, the one open panel and what the user pressed. Pure;
//! the event layer feeds it events and executes its outputs (`XFocus` at `BatchEnd`,
//! the rest at once).

// Wired into the event layer in T5; T5c removes this.
#![allow(dead_code)]

use super::model::{
    Compositor, Event, FocusOnMap, FocusState, InputHint, KbTarget, Method, Output, OutputId,
    PressTarget, Role, RoleTable, XFocusChange,
};
use log::warn;
use xcb::x;

/// The longest classification-link chain followed; links can loop after a window id is
/// reused.
pub const MAX_LINKS: usize = 32;

impl From<KbTarget> for Compositor {
    fn from(target: KbTarget) -> Self {
        match target {
            KbTarget::X(w) => Compositor::X(w),
            KbTarget::Overlay(o) => Compositor::Overlay(o),
        }
    }
}

/// Whether `w` is `root` or links to it through Popup, PanelOf and OverlayPopupOf
/// (Family(root), design §2 terms).
pub fn in_family(roles: &RoleTable, w: x::Window, root: x::Window) -> bool {
    let mut current = w;
    for _ in 0..MAX_LINKS {
        if current == root {
            return true;
        }
        match roles.classification(current).and_then(|c| c.role.link()) {
            Some(next) => current = next,
            None => return false,
        }
    }
    false
}

/// Whether X focus may go to `w` when it is pressed: a toplevel, a panel, or a popup
/// upstream's #494 rule gives focus (design §2 terms, Focusable).
pub fn focusable(roles: &RoleTable, w: x::Window) -> bool {
    let Some(entry) = roles.entry(w) else {
        return false;
    };
    let Some(c) = entry.classification else {
        return false;
    };
    if c.role.is_toplevel() || c.role.is_panel() {
        return true;
    }
    c.role.is_popup()
        && !entry.facts.override_redirect
        && (entry.facts.take_focus || entry.facts.input == InputHint::True)
}

/// How X is told to focus `w`. Toplevels always get SetInputFocus, as they always have
/// from a keyboard enter; the rest are offered WM_TAKE_FOCUS when they advertise it (Q1).
pub fn method(roles: &RoleTable, w: x::Window) -> Method {
    match roles.entry(w) {
        Some(entry) if entry.classification.is_some_and(|c| c.role.is_toplevel()) => {
            Method::SetInput
        }
        Some(entry) if entry.facts.take_focus => Method::TakeFocus,
        _ => Method::SetInput,
    }
}

/// The output whose RandR primary goes with focusing `w`: a toplevel's own, an overlay
/// window's overlay, else that of what it is a popup or panel of (rule 13).
pub fn primary_output(roles: &RoleTable, w: x::Window) -> Option<OutputId> {
    let mut current = w;
    for _ in 0..MAX_LINKS {
        let entry = roles.entry(current)?;
        match entry.classification?.role {
            Role::OverlayWindow { output, .. } => return Some(output),
            Role::Toplevel { .. } | Role::FullscreenToplevel { .. } => return entry.output,
            role => current = role.link()?,
        }
    }
    None
}

impl FocusState {
    /// Handles one event; returns what the event layer must do now (`XFocus` only at
    /// `BatchEnd`).
    pub fn handle(&mut self, event: Event, roles: &RoleTable) -> Vec<Output> {
        let mut out = Vec::new();
        match event {
            Event::KeyboardEnter(target) => self.keyboard_enter(target, roles),
            Event::KeyboardLeave(target) => {
                // A leave of a surface the compositor's focus is not on (reordered or
                // duplicated) says nothing. Otherwise only what follows in this batch tells a
                // move from a loss (rule 2); the route drops at once.
                if self.compositor == Compositor::from(target) {
                    self.compositor = Compositor::Other;
                    self.leave_pending = true;
                }
            }
            Event::Key { pressed } => {
                if pressed {
                    self.pending_press = None;
                }
            }
            Event::Press(target) => self.press(target, roles),
            Event::Mapped {
                window,
                classification,
            } => match classification.focus_on_map {
                FocusOnMap::Panel => self.open_panel(window),
                FocusOnMap::SetInput | FocusOnMap::TakeFocus => self.assign(Some(window)),
                FocusOnMap::None => {}
            },
            Event::Gone(window) => self.gone(window, roles),
            Event::ActivationRequested(window) => self.activation(window, roles, &mut out),
            Event::OverlayGone(output) => {
                if self.compositor == Compositor::Overlay(output) {
                    self.compositor = Compositor::Nothing;
                }
                if self.pending_press.is_some_and(|(_, o)| o == output) {
                    self.pending_press = None;
                }
            }
            Event::OutputChanged(window, _) => {
                // Rule 13: the X-focused toplevel moved output; set the primary again.
                if self.desired == Some(window)
                    && self.applied_window() == Some(window)
                    && roles
                        .classification(window)
                        .is_some_and(|c| c.role.is_toplevel())
                {
                    self.assign(Some(window));
                }
            }
            Event::BatchEnd => self.batch_end(roles, &mut out),
        }
        self.update_route(&mut out);
        out
    }

    /// The window X was last told to focus.
    pub fn applied_window(&self) -> Option<x::Window> {
        match self.applied {
            Some(XFocusChange::Window { window, .. }) => Some(window),
            _ => None,
        }
    }

    fn compositor_window(&self) -> Option<x::Window> {
        match self.compositor {
            Compositor::X(w) => Some(w),
            _ => None,
        }
    }

    fn assign(&mut self, desired: Option<x::Window>) {
        self.desired = desired;
        self.assigned = true;
    }

    fn keyboard_enter(&mut self, target: KbTarget, roles: &RoleTable) {
        self.leave_pending = false;
        match target {
            KbTarget::X(w) => {
                self.compositor = Compositor::X(w);
                self.pending_press = None;
                // Rule 12: the activation this Enter answers is the user's intent.
                if self.pending_activation.take() == Some(w) {
                    self.intent(w, roles);
                    return;
                }
                if self.panel.is_some() {
                    // Rule 4: hover never steals from the panel.
                    self.remembered = Some(w);
                } else {
                    self.follow(w, roles);
                }
            }
            KbTarget::Overlay(o) => {
                self.compositor = Compositor::Overlay(o);
                self.pending_activation = None;
                // Rule 7: the press this Enter answers; rules 4 and 8: otherwise nothing.
                if let Some((pressed, output)) = self.pending_press.take()
                    && output == o
                {
                    self.overlay_press(pressed, roles);
                }
            }
        }
    }

    /// Rule 1: X focus follows the compositor's, but a popup of `w` that has X focus
    /// keeps it.
    fn follow(&mut self, w: x::Window, roles: &RoleTable) {
        if let Some(d) = self.desired
            && d != w
            && roles.classification(d).is_some_and(|c| c.role.is_popup())
            && in_family(roles, d, w)
        {
            return;
        }
        self.assign(Some(w));
    }

    fn press(&mut self, target: PressTarget, roles: &RoleTable) {
        self.pending_press = None;
        self.pending_activation = None;
        match target {
            PressTarget::Window(w) => self.intent(w, roles),
            PressTarget::OverlayWindow { window, output } => {
                // Rule 7: niri sends no Enter for a click on the overlay it already focused.
                if self.compositor == Compositor::Overlay(output) {
                    self.overlay_press(window, roles);
                } else {
                    self.pending_press = Some((window, output));
                }
            }
            PressTarget::Other => {}
        }
    }

    /// Rules 5 and 6: the user asked for `w` (a press on it, or its activation).
    fn intent(&mut self, w: x::Window, roles: &RoleTable) {
        if let Some(panel) = self.panel
            && in_family(roles, w, panel)
        {
            return;
        }
        self.panel = None;
        self.remembered = None;
        if roles.classification(w).is_some_and(|c| c.role.is_panel()) {
            self.open_panel(w);
        } else if focusable(roles, w) {
            self.assign(Some(w));
        } else if let Some(v) = self.compositor_window() {
            self.follow(v, roles);
        }
    }

    /// Rule 3: `w` is the one open panel now; the window the compositor is on is where
    /// focus goes back to.
    fn open_panel(&mut self, w: x::Window) {
        self.panel = Some(w);
        if let Some(v) = self.compositor_window() {
            self.remembered = Some(v);
        }
        self.assign(Some(w));
    }

    /// Rule 7: a press on overlay window `w`, once the compositor's focus is on its overlay.
    fn overlay_press(&mut self, w: x::Window, roles: &RoleTable) {
        if self.panel == Some(w) {
            return;
        }
        if roles.classification(w).is_some_and(|c| c.role.is_panel()) {
            self.open_panel(w);
        } else if self.panel.is_none() {
            self.assign(Some(w));
        }
        // A bar under an open panel only gets the click: X focus stays on the panel.
    }

    /// Rules 9 and 10.
    fn gone(&mut self, w: x::Window, roles: &RoleTable) {
        let was_panel = self.panel == Some(w);
        if was_panel {
            self.panel = None;
        }
        if self.desired == Some(w) || self.applied_window() == Some(w) {
            let next = [
                self.panel,
                self.remembered,
                self.compositor_window(),
                self.last_toplevel,
            ]
            .into_iter()
            .flatten()
            .find(|&c| c != w && roles.is_mapped(c));
            self.assign(next);
        }
        if was_panel {
            self.remembered = None;
        }
        if self.remembered == Some(w) {
            self.remembered = None;
        }
        if self.last_toplevel == Some(w) {
            self.last_toplevel = None;
        }
        if self.pending_press.is_some_and(|(p, _)| p == w) {
            self.pending_press = None;
        }
        if self.pending_activation == Some(w) {
            self.pending_activation = None;
        }
        if self.compositor == Compositor::X(w) {
            // The compositor's leave for a dead surface never arrives.
            self.compositor = Compositor::Nothing;
        }
    }

    /// Rule 12.
    fn activation(&mut self, w: x::Window, roles: &RoleTable, out: &mut Vec<Output>) {
        match self.compositor {
            Compositor::X(v) => out.push(Output::ActivationToken {
                window: w,
                surface: KbTarget::X(v),
            }),
            Compositor::Overlay(o) => out.push(Output::ActivationToken {
                window: w,
                surface: KbTarget::Overlay(o),
            }),
            Compositor::Other | Compositor::Nothing => {
                warn!("no surface with keyboard focus to activate {w:?} from");
            }
        }
        if self.compositor == Compositor::X(w) {
            self.pending_activation = None;
            self.intent(w, roles);
        } else {
            self.pending_activation = Some(w);
        }
    }

    fn batch_end(&mut self, roles: &RoleTable, out: &mut Vec<Output>) {
        if self.leave_pending {
            // Rule 2: focus went to another client.
            self.leave_pending = false;
            self.compositor = Compositor::Other;
            self.panel = None;
            self.remembered = None;
            self.pending_press = None;
            self.pending_activation = None;
            self.assign(None);
        }
        if !self.assigned {
            return;
        }
        self.assigned = false;
        let change = match self.desired {
            None => XFocusChange::None,
            Some(window) => XFocusChange::Window {
                window,
                method: method(roles, window),
                primary_output: primary_output(roles, window),
            },
        };
        if let XFocusChange::Window { window, .. } = change
            && roles
                .classification(window)
                .is_some_and(|c| c.role.is_toplevel())
        {
            self.last_toplevel = Some(window);
        }
        self.applied = Some(change);
        out.push(Output::XFocus(change));
    }

    /// §2.0: while the compositor's focus is on an overlay, keys go to the window in
    /// `desired`; otherwise nowhere through satellite.
    fn update_route(&mut self, out: &mut Vec<Output>) {
        let route = match self.compositor {
            Compositor::Overlay(_) => self.desired,
            _ => None,
        };
        if route != self.route {
            self.route = route;
            out.push(Output::KeyboardRoute(route));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::model::testkit::*;
    use crate::server::model::*;
    use crate::xstate::WindowRole;

    const MAIN: u32 = A | 1;
    const MEETING: u32 = A | 2;
    const PANEL: u32 = A | 3;
    const BAR: u32 = A | 4;
    const POPUP: u32 = A | 5;
    const BUBBLE: u32 = A | 6;
    const OVERLAY_PANEL: u32 = A | 7;
    const CANDIDATES: u32 = B | 1;
    const OTHER: u32 = B | 2;

    struct T {
        fs: FocusState,
        roles: RoleTable,
    }

    impl T {
        fn ev(&mut self, events: &[Event]) -> Vec<Output> {
            events
                .iter()
                .flat_map(|e| self.fs.handle(*e, &self.roles))
                .collect()
        }

        /// X focus and the compositor on `w`, as after it was focused.
        fn focused(&mut self, w: u32) {
            self.fs.compositor = Compositor::X(win(w));
            self.fs.desired = Some(win(w));
            self.fs.applied = Some(XFocusChange::Window {
                window: win(w),
                method: Method::SetInput,
                primary_output: None,
            });
            self.fs.last_toplevel = Some(win(w));
        }

        /// `p` is the open panel and holds X focus; the compositor is on `compositor`.
        fn panel_open(&mut self, p: u32, compositor: Compositor, remembered: Option<u32>) {
            self.fs.panel = Some(win(p));
            self.fs.remembered = remembered.map(win);
            self.fs.desired = Some(win(p));
            self.fs.applied = Some(XFocusChange::Window {
                window: win(p),
                method: Method::SetInput,
                primary_output: None,
            });
            self.fs.compositor = compositor;
        }
    }

    /// Two toplevels (main, meeting), a panel of the meeting, a bar and an overlay panel on
    /// O1, an input-true popup of main, WeChat's no-input bubble of main, IME candidates
    /// over the panel, and another client's toplevel.
    fn world() -> T {
        let mut roles = RoleTable::default();
        output(&mut roles, O1, 0);
        output(&mut roles, O2, 3840);
        add(&mut roles, facts(MAIN), TOPLEVEL);
        add(&mut roles, facts(MEETING), TOPLEVEL);
        add(
            &mut roles,
            facts(PANEL),
            Role::PanelOf {
                parent: win(MEETING),
            },
        );
        add(
            &mut roles,
            facts(BAR),
            Role::OverlayWindow {
                output: O1,
                panel: false,
            },
        );
        add(
            &mut roles,
            facts(OVERLAY_PANEL),
            Role::OverlayWindow {
                output: O1,
                panel: true,
            },
        );
        add(
            &mut roles,
            facts(POPUP)
                .input(InputHint::True)
                .guessed(WindowRole::Popup),
            Role::Popup { parent: win(MAIN) },
        );
        add(
            &mut roles,
            facts(BUBBLE)
                .input(InputHint::False)
                .guessed(WindowRole::Popup),
            Role::Popup { parent: win(MAIN) },
        );
        add(
            &mut roles,
            facts(CANDIDATES).or(),
            Role::OverlayPopupOf { panel: win(PANEL) },
        );
        add(&mut roles, facts(OTHER), TOPLEVEL);
        T {
            fs: FocusState::default(),
            roles,
        }
    }

    fn xf(w: u32) -> Output {
        Output::XFocus(XFocusChange::Window {
            window: win(w),
            method: Method::SetInput,
            primary_output: None,
        })
    }

    fn xf_on(w: u32, o: OutputId) -> Output {
        Output::XFocus(XFocusChange::Window {
            window: win(w),
            method: Method::SetInput,
            primary_output: Some(o),
        })
    }

    fn xf_none() -> Output {
        Output::XFocus(XFocusChange::None)
    }

    fn route(w: u32) -> Output {
        Output::KeyboardRoute(Some(win(w)))
    }

    fn enter_x(w: u32) -> Event {
        Event::KeyboardEnter(KbTarget::X(win(w)))
    }

    fn leave_x(w: u32) -> Event {
        Event::KeyboardLeave(KbTarget::X(win(w)))
    }

    fn enter_overlay(o: OutputId) -> Event {
        Event::KeyboardEnter(KbTarget::Overlay(o))
    }

    fn leave_overlay(o: OutputId) -> Event {
        Event::KeyboardLeave(KbTarget::Overlay(o))
    }

    fn press(w: u32) -> Event {
        Event::Press(PressTarget::Window(win(w)))
    }

    fn press_overlay(w: u32, o: OutputId) -> Event {
        Event::Press(PressTarget::OverlayWindow {
            window: win(w),
            output: o,
        })
    }

    use crate::server::model::Event::BatchEnd;

    // --- Predicates ---

    #[test]
    fn method_for_toplevels_is_set_input() {
        let mut t = world();
        t.roles
            .windows
            .get_mut(&win(MAIN))
            .unwrap()
            .facts
            .take_focus = true;
        t.roles
            .windows
            .get_mut(&win(PANEL))
            .unwrap()
            .facts
            .take_focus = true;
        assert_eq!(method(&t.roles, win(MAIN)), Method::SetInput);
        assert_eq!(method(&t.roles, win(PANEL)), Method::TakeFocus);
        assert_eq!(method(&t.roles, win(POPUP)), Method::SetInput);
    }

    #[test]
    fn focusable_windows() {
        let t = world();
        assert!(focusable(&t.roles, win(MAIN)));
        assert!(focusable(&t.roles, win(PANEL)));
        assert!(focusable(&t.roles, win(OVERLAY_PANEL)));
        assert!(focusable(&t.roles, win(POPUP)));
        assert!(!focusable(&t.roles, win(BUBBLE)));
        assert!(!focusable(&t.roles, win(CANDIDATES)));
        assert!(!focusable(&t.roles, win(BAR)));
        assert!(!focusable(&t.roles, win(A | 99)));
    }

    #[test]
    fn family_and_primary_output() {
        let mut t = world();
        assert!(in_family(&t.roles, win(CANDIDATES), win(PANEL)));
        assert!(in_family(&t.roles, win(CANDIDATES), win(MEETING)));
        assert!(in_family(&t.roles, win(PANEL), win(PANEL)));
        assert!(!in_family(&t.roles, win(MEETING), win(PANEL)));
        assert!(!in_family(&t.roles, win(POPUP), win(MEETING)));
        t.roles.windows.get_mut(&win(MEETING)).unwrap().output = Some(O2);
        assert_eq!(primary_output(&t.roles, win(CANDIDATES)), Some(O2));
        assert_eq!(primary_output(&t.roles, win(BAR)), Some(O1));
        assert_eq!(primary_output(&t.roles, win(MAIN)), None);
    }

    // Review Focus 2: links that loop (window id reuse) end the walk.
    #[test]
    fn link_walks_terminate_on_cycles() {
        let mut roles = RoleTable::default();
        add(&mut roles, facts(A | 1), Role::Popup { parent: win(A | 2) });
        add(&mut roles, facts(A | 2), Role::Popup { parent: win(A | 1) });
        assert!(!in_family(&roles, win(A | 1), win(A | 3)));
        assert_eq!(primary_output(&roles, win(A | 1)), None);
    }

    // --- Rule 1 ---

    // "With no panel, KeyboardEnter(X(w)) → desired := w. Toplevels … become last_toplevel."
    #[test]
    fn rule1_follow_without_panel() {
        let mut t = world();
        assert_eq!(t.ev(&[enter_x(MAIN)]), vec![]);
        assert_eq!(t.ev(&[BatchEnd]), vec![xf(MAIN)]);
        assert_eq!(t.fs.last_toplevel, Some(win(MAIN)));
        assert_eq!(t.fs.applied_window(), Some(win(MAIN)));
    }

    // Rule 1 exception: an Enter on the parent keeps X focus on its focused popup.
    #[test]
    fn rule1_keeps_a_focused_popup_of_the_entered_window() {
        let mut t = world();
        t.focused(MAIN);
        t.fs.desired = Some(win(POPUP));
        assert_eq!(t.ev(&[leave_x(MAIN), enter_x(MAIN), BatchEnd]), vec![]);
        // Not for a popup of another window.
        assert_eq!(
            t.ev(&[leave_x(MAIN), enter_x(OTHER), BatchEnd]),
            vec![xf(OTHER)]
        );
    }

    // --- Rule 2 ---

    #[test]
    fn rule2_leave_alone_is_focus_to_another_client() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MAIN)), Some(MAIN));
        t.fs.pending_activation = Some(win(OTHER));
        assert_eq!(t.ev(&[leave_x(MAIN), BatchEnd]), vec![xf_none()]);
        assert_eq!(t.fs.panel, None);
        assert_eq!(t.fs.remembered, None);
        assert_eq!(t.fs.pending_activation, None);
        assert_eq!(t.fs.compositor, Compositor::Other);
    }

    // Race: Leave then Enter in one batch is a move, not a loss.
    #[test]
    fn rule2_leave_then_enter_in_one_batch() {
        let mut t = world();
        t.focused(MAIN);
        assert_eq!(
            t.ev(&[leave_x(MAIN), enter_x(MEETING), BatchEnd]),
            vec![xf(MEETING)]
        );
    }

    // Race (deliberate change): Enter(A) then Leave(A) in one batch: focus left to another client.
    #[test]
    fn rule2_enter_then_leave_in_one_batch() {
        let mut t = world();
        t.focused(MAIN);
        assert_eq!(
            t.ev(&[enter_x(MEETING), leave_x(MEETING), BatchEnd]),
            vec![xf_none()]
        );
    }

    // A leave of a surface the compositor's focus is not on is ignored: it neither resolves
    // rule 2 nor drops the panel (review r1 M4).
    #[test]
    fn rule2_leave_of_another_surface_is_ignored() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MAIN)), Some(MAIN));
        assert_eq!(t.ev(&[leave_x(MEETING), BatchEnd]), vec![]);
        assert_eq!(t.fs.compositor, Compositor::X(win(MAIN)));
        assert_eq!(t.fs.panel, Some(win(PANEL)));
        assert_eq!(t.ev(&[leave_overlay(O1), BatchEnd]), vec![]);
        assert_eq!(t.fs.panel, Some(win(PANEL)));
    }

    // Known limitation: a Leave and Enter split across batches drops the panel.
    #[test]
    fn rule2_split_batches_drop_the_panel() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MEETING)), Some(MEETING));
        assert_eq!(t.ev(&[leave_x(MEETING), BatchEnd]), vec![xf_none()]);
        assert_eq!(t.ev(&[enter_x(MAIN), BatchEnd]), vec![xf(MAIN)]);
        assert_eq!(t.fs.panel, None);
    }

    // --- Rule 3 ---

    fn mapped(w: u32, role: Role, focus_on_map: FocusOnMap) -> Event {
        Event::Mapped {
            window: win(w),
            classification: Classification {
                kind: XKind::Notification,
                role,
                focus_on_map,
            },
        }
    }

    // "A window mapped with focus_on_map = Panel becomes panel … desired := it.
    // remembered := the compositor-focus X window if compositor == X(v)."
    #[test]
    fn rule3_panel_opens() {
        let mut t = world();
        t.focused(MEETING);
        let out = t.ev(&[
            mapped(
                PANEL,
                Role::PanelOf {
                    parent: win(MEETING),
                },
                FocusOnMap::Panel,
            ),
            BatchEnd,
        ]);
        assert_eq!(out, vec![xf(PANEL)]);
        assert_eq!(t.fs.panel, Some(win(PANEL)));
        assert_eq!(t.fs.remembered, Some(win(MEETING)));
        assert_eq!(t.fs.last_toplevel, Some(win(MEETING)));
    }

    // Rule 3: TakeFocus when advertised.
    #[test]
    fn rule3_panel_with_take_focus() {
        let mut t = world();
        t.roles
            .windows
            .get_mut(&win(PANEL))
            .unwrap()
            .facts
            .take_focus = true;
        let out = t.ev(&[
            mapped(
                PANEL,
                Role::PanelOf {
                    parent: win(MEETING),
                },
                FocusOnMap::Panel,
            ),
            BatchEnd,
        ]);
        assert_eq!(
            out,
            vec![Output::XFocus(XFocusChange::Window {
                window: win(PANEL),
                method: Method::TakeFocus,
                primary_output: None,
            })]
        );
    }

    // Rule 3: a panel replacing another while the compositor is on the overlay keeps
    // the restore target; the route follows.
    #[test]
    fn rule3_replacing_a_panel_on_the_overlay_keeps_remembered() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::Overlay(O1), Some(MAIN));
        t.fs.route = Some(win(PANEL));
        let out = t.ev(&[
            mapped(
                OVERLAY_PANEL,
                Role::OverlayWindow {
                    output: O1,
                    panel: true,
                },
                FocusOnMap::Panel,
            ),
            BatchEnd,
        ]);
        assert_eq!(out, vec![route(OVERLAY_PANEL), xf_on(OVERLAY_PANEL, O1)]);
        assert_eq!(t.fs.remembered, Some(win(MAIN)));
    }

    // --- Rule 4 ---

    // "While a panel is open, KeyboardEnter(X(w)) … and KeyboardEnter(Overlay(o)) with no
    // matching pending_press only update compositor and remembered."
    #[test]
    fn rule4_hover_never_steals_from_the_panel() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MEETING)), Some(MEETING));
        assert_eq!(t.ev(&[leave_x(MEETING), enter_x(MAIN), BatchEnd]), vec![]);
        assert_eq!(t.fs.remembered, Some(win(MAIN)));
        // Onto the overlay (niri focus-follows-mouse on an on-demand layer): keys go to the panel.
        assert_eq!(
            t.ev(&[leave_x(MAIN), enter_overlay(O1), BatchEnd]),
            vec![route(PANEL)]
        );
        assert_eq!(t.fs.desired, Some(win(PANEL)));
    }

    // --- Rule 5 ---

    // "A Press on window w not in Family(panel): the panel and remembered are cleared;
    // desired := w if Focusable(w)."
    #[test]
    fn rule5_press_outside_the_panel_focuses_the_pressed_window() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MAIN)), Some(MAIN));
        assert_eq!(t.ev(&[press(MAIN), BatchEnd]), vec![xf(MAIN)]);
        assert_eq!(t.fs.panel, None);
        assert_eq!(t.fs.remembered, None);
    }

    // Rule 5: a non-Focusable window (WeChat's bubble): focus follows the compositor.
    #[test]
    fn rule5_non_focusable_press_follows_the_compositor() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MAIN)), Some(MAIN));
        assert_eq!(t.ev(&[press(BUBBLE), BatchEnd]), vec![xf(MAIN)]);
        assert_eq!(t.fs.panel, None);
        // Without a panel (Q9): the same, through rule 1's exception (here: none).
        let mut t = world();
        t.focused(MAIN);
        assert_eq!(t.ev(&[press(BUBBLE), BatchEnd]), vec![xf(MAIN)]);
        // On the overlay there is no X window to follow: nothing.
        let mut t = world();
        t.focused(MAIN);
        t.fs.compositor = Compositor::Overlay(O1);
        t.fs.route = Some(win(MAIN));
        assert_eq!(t.ev(&[press(BUBBLE), BatchEnd]), vec![]);
    }

    // Rule 5 without a panel (deliberate change): a press focuses a Focusable popup, and a
    // press in the focused toplevel re-asserts it (self-healing).
    #[test]
    fn rule5_without_a_panel() {
        let mut t = world();
        t.focused(MAIN);
        assert_eq!(t.ev(&[press(POPUP), BatchEnd]), vec![xf(POPUP)]);
        assert_eq!(t.ev(&[press(MAIN), BatchEnd]), vec![xf(MAIN)]);
        assert_eq!(t.ev(&[press(MAIN), BatchEnd]), vec![xf(MAIN)]);
    }

    // Rule 5 (r6): a PanelOf panel still mapped after the slot was cleared becomes the panel.
    #[test]
    fn rule5_promotes_a_pressed_panel() {
        let mut t = world();
        t.focused(MAIN);
        assert_eq!(t.ev(&[press(PANEL), BatchEnd]), vec![xf(PANEL)]);
        assert_eq!(t.fs.panel, Some(win(PANEL)));
        assert_eq!(t.fs.remembered, Some(win(MAIN)));
    }

    // --- Rule 6 ---

    #[test]
    fn rule6_press_inside_the_family_changes_nothing() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MEETING)), Some(MEETING));
        assert_eq!(t.ev(&[press(PANEL), BatchEnd]), vec![]);
        assert_eq!(t.ev(&[press(CANDIDATES), BatchEnd]), vec![]);
        assert_eq!(t.fs.panel, Some(win(PANEL)));
    }

    // --- Rule 7 ---

    // Consumed immediately when the compositor is already on that overlay (no Enter comes).
    #[test]
    fn rule7_immediate_on_a_focused_overlay() {
        let mut t = world();
        t.focused(MAIN);
        t.fs.compositor = Compositor::Overlay(O1);
        t.fs.route = Some(win(MAIN));
        assert_eq!(t.ev(&[press_overlay(BAR, O1)]), vec![route(BAR)]);
        assert_eq!(t.ev(&[BatchEnd]), vec![xf_on(BAR, O1)]);
    }

    // Race: press → Leave → Enter(overlay), across batches.
    #[test]
    fn rule7_pending_press_survives_batches_and_a_leave() {
        let mut t = world();
        t.focused(MAIN);
        assert_eq!(t.ev(&[press_overlay(BAR, O1), BatchEnd]), vec![]);
        assert_eq!(t.ev(&[leave_x(MAIN)]), vec![]);
        assert_eq!(t.ev(&[enter_overlay(O1)]), vec![route(BAR)]);
        assert_eq!(t.ev(&[BatchEnd]), vec![xf_on(BAR, O1)]);
        assert_eq!(t.fs.pending_press, None);
    }

    // Rule 7: a press on the current panel changes nothing; another panel becomes the panel.
    #[test]
    fn rule7_panels() {
        let mut t = world();
        t.panel_open(OVERLAY_PANEL, Compositor::Overlay(O1), Some(MAIN));
        t.fs.route = Some(win(OVERLAY_PANEL));
        assert_eq!(t.ev(&[press_overlay(OVERLAY_PANEL, O1), BatchEnd]), vec![]);
        let mut t = world();
        t.panel_open(PANEL, Compositor::Overlay(O1), Some(MAIN));
        t.fs.route = Some(win(PANEL));
        assert_eq!(
            t.ev(&[press_overlay(OVERLAY_PANEL, O1), BatchEnd]),
            vec![route(OVERLAY_PANEL), xf_on(OVERLAY_PANEL, O1)]
        );
        assert_eq!(t.fs.panel, Some(win(OVERLAY_PANEL)));
        assert_eq!(t.fs.remembered, Some(win(MAIN)));
    }

    // Rule 7 (deliberate change): a bar pressed under an open panel leaves focus on the panel.
    #[test]
    fn rule7_bar_under_a_panel() {
        let mut t = world();
        t.panel_open(OVERLAY_PANEL, Compositor::Overlay(O1), None);
        t.fs.route = Some(win(OVERLAY_PANEL));
        assert_eq!(t.ev(&[press_overlay(BAR, O1), BatchEnd]), vec![]);
        assert_eq!(t.fs.panel, Some(win(OVERLAY_PANEL)));
    }

    // Rule 7 drop conditions: key press (not release), Enter elsewhere, another press,
    // another output's overlay.
    #[test]
    fn rule7_pending_press_drops() {
        let key = |pressed| Event::Key { pressed };
        let mut t = world();
        t.focused(MAIN);
        t.ev(&[press_overlay(BAR, O1), key(false)]);
        assert_eq!(t.fs.pending_press, Some((win(BAR), O1)));
        t.ev(&[key(true)]);
        assert_eq!(t.fs.pending_press, None);

        let mut t = world();
        t.focused(MAIN);
        assert_eq!(
            t.ev(&[
                press_overlay(BAR, O1),
                leave_x(MAIN),
                enter_x(MEETING),
                BatchEnd
            ]),
            vec![xf(MEETING)]
        );
        assert_eq!(t.fs.pending_press, None);

        let mut t = world();
        t.focused(MAIN);
        t.ev(&[press_overlay(BAR, O1), press(MAIN)]);
        assert_eq!(t.fs.pending_press, None);

        let mut t = world();
        t.focused(MAIN);
        assert_eq!(
            t.ev(&[
                press_overlay(BAR, O1),
                leave_x(MAIN),
                enter_overlay(O2),
                BatchEnd
            ]),
            vec![route(MAIN)]
        );
        assert_eq!(t.fs.pending_press, None);
    }

    // Rule 7 (deliberate change): one-shot; a later hover onto the overlay focuses nothing.
    #[test]
    fn rule7_press_is_one_shot() {
        let mut t = world();
        t.focused(MAIN);
        t.ev(&[
            press_overlay(BAR, O1),
            leave_x(MAIN),
            enter_overlay(O1),
            BatchEnd,
        ]);
        assert_eq!(
            t.ev(&[leave_overlay(O1), enter_x(MAIN), BatchEnd]),
            vec![Output::KeyboardRoute(None), xf(MAIN)]
        );
        assert_eq!(
            t.ev(&[leave_x(MAIN), enter_overlay(O1), BatchEnd]),
            vec![route(MAIN)]
        );
    }

    // --- Rule 8 ---

    // Deliberate change: an overlay hover without a press keeps X focus and routes keys to it.
    #[test]
    fn rule8_overlay_hover_keeps_x_focus() {
        let mut t = world();
        t.focused(MAIN);
        assert_eq!(
            t.ev(&[leave_x(MAIN), enter_overlay(O1), BatchEnd]),
            vec![route(MAIN)]
        );
        assert_eq!(t.fs.desired, Some(win(MAIN)));
    }

    // --- Rule 9 ---

    fn gone(t: &mut T, w: u32) -> Vec<Output> {
        t.roles.windows.get_mut(&win(w)).unwrap().mapped = false;
        t.ev(&[Event::Gone(win(w)), BatchEnd])
    }

    // Race: a popup closing with X focus on it gives focus back (deliberate change, U2 replaced).
    #[test]
    fn rule9_focused_popup_goes() {
        let mut t = world();
        t.focused(MAIN);
        t.fs.desired = Some(win(POPUP));
        t.fs.applied = Some(XFocusChange::Window {
            window: win(POPUP),
            method: Method::SetInput,
            primary_output: None,
        });
        assert_eq!(gone(&mut t, POPUP), vec![xf(MAIN)]);
    }

    // The panel goes: remembered → compositor window → last toplevel → None; remembered cleared.
    #[test]
    fn rule9_panel_goes() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MAIN)), Some(MAIN));
        assert_eq!(gone(&mut t, PANEL), vec![xf(MAIN)]);
        assert_eq!((t.fs.panel, t.fs.remembered), (None, None));

        let mut t = world();
        t.panel_open(PANEL, Compositor::Overlay(O1), Some(MAIN));
        t.fs.route = Some(win(PANEL));
        t.fs.last_toplevel = Some(win(MEETING));
        t.roles.windows.get_mut(&win(MAIN)).unwrap().mapped = false;
        assert_eq!(gone(&mut t, PANEL), vec![route(MEETING), xf(MEETING)]);

        let mut t = world();
        t.panel_open(PANEL, Compositor::Other, None);
        assert_eq!(gone(&mut t, PANEL), vec![xf_none()]);
    }

    // The panel's own popup goes: back to the panel.
    #[test]
    fn rule9_popup_of_the_panel_goes() {
        let mut t = world();
        add(
            &mut t.roles,
            facts(A | 20)
                .input(InputHint::True)
                .guessed(WindowRole::Popup),
            Role::Popup { parent: win(PANEL) },
        );
        t.panel_open(PANEL, Compositor::X(win(MEETING)), Some(MEETING));
        t.fs.desired = Some(win(A | 20));
        t.fs.applied = Some(XFocusChange::Window {
            window: win(A | 20),
            method: Method::SetInput,
            primary_output: None,
        });
        assert_eq!(gone(&mut t, A | 20), vec![xf(PANEL)]);
    }

    // Q10: the panel goes while its popup holds X focus: no X traffic, remembered cleared.
    #[test]
    fn rule9_panel_goes_unfocused() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MEETING)), Some(MEETING));
        t.fs.desired = Some(win(CANDIDATES));
        t.fs.applied = Some(XFocusChange::Window {
            window: win(CANDIDATES),
            method: Method::SetInput,
            primary_output: None,
        });
        assert_eq!(gone(&mut t, PANEL), vec![]);
        assert_eq!((t.fs.panel, t.fs.remembered), (None, None));
    }

    // A window that does not hold X focus going away causes no X traffic.
    #[test]
    fn rule9_unrelated_window_goes() {
        let mut t = world();
        t.focused(MAIN);
        assert_eq!(gone(&mut t, OTHER), vec![]);
    }

    // --- Rule 10 ---

    #[test]
    fn rule10_references() {
        let mut t = world();
        t.focused(MAIN);
        t.fs.pending_activation = Some(win(MAIN));
        assert_eq!(gone(&mut t, MAIN), vec![xf_none()]);
        assert_eq!(t.fs.last_toplevel, None);
        assert_eq!(t.fs.pending_activation, None);
        assert_eq!(t.fs.compositor, Compositor::Nothing);

        let mut t = world();
        t.focused(MAIN);
        t.ev(&[press_overlay(BAR, O1)]);
        t.roles.windows.get_mut(&win(BAR)).unwrap().mapped = false;
        t.ev(&[Event::Gone(win(BAR))]);
        assert_eq!(t.fs.pending_press, None);

        // OverlayGone: compositor Nothing, the route dropped, a pending press on it dropped.
        let mut t = world();
        t.focused(MAIN);
        t.fs.compositor = Compositor::Overlay(O1);
        t.fs.route = Some(win(MAIN));
        t.fs.pending_press = Some((win(BAR), O1));
        assert_eq!(
            t.ev(&[Event::OverlayGone(O1), BatchEnd]),
            vec![Output::KeyboardRoute(None)]
        );
        assert_eq!(t.fs.compositor, Compositor::Nothing);
        assert_eq!(t.fs.pending_press, None);
    }

    // --- Rule 11 ---

    #[test]
    fn rule11_popups_at_first_configure() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MEETING)), Some(MEETING));
        let popup_mapped = |focus_on_map| Event::Mapped {
            window: win(POPUP),
            classification: Classification {
                kind: XKind::Popup,
                role: Role::Popup { parent: win(MAIN) },
                focus_on_map,
            },
        };
        assert_eq!(t.ev(&[popup_mapped(FocusOnMap::None), BatchEnd]), vec![]);
        assert_eq!(
            t.ev(&[popup_mapped(FocusOnMap::SetInput), BatchEnd]),
            vec![xf(POPUP)]
        );
        // Never touches the panel slot.
        assert_eq!(t.fs.panel, Some(win(PANEL)));
    }

    // --- Rule 12 ---

    fn token(w: u32, surface: KbTarget) -> Output {
        Output::ActivationToken {
            window: win(w),
            surface,
        }
    }

    // Race: activation of the already-focused window resolves at once (rule 5).
    #[test]
    fn rule12_activation_of_the_compositor_focus_window() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(OTHER)), Some(OTHER));
        let out = t.ev(&[Event::ActivationRequested(win(OTHER)), BatchEnd]);
        assert_eq!(out, vec![token(OTHER, KbTarget::X(win(OTHER))), xf(OTHER)]);
        assert_eq!((t.fs.panel, t.fs.pending_activation), (None, None));
    }

    // Rule 12 → rule 6: activating a window of the panel's family keeps the panel.
    #[test]
    fn rule12_activation_inside_the_family() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(PANEL)), None);
        let out = t.ev(&[Event::ActivationRequested(win(PANEL)), BatchEnd]);
        assert_eq!(out, vec![token(PANEL, KbTarget::X(win(PANEL)))]);
        assert_eq!(t.fs.panel, Some(win(PANEL)));
    }

    // Race: keyboard-triggered activation (Return released before the compositor's Enter).
    #[test]
    fn rule12_key_release_does_not_drop_the_activation() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MEETING)), Some(MEETING));
        assert_eq!(
            t.ev(&[Event::ActivationRequested(win(OTHER)), BatchEnd]),
            vec![token(OTHER, KbTarget::X(win(MEETING)))]
        );
        assert_eq!(t.ev(&[Event::Key { pressed: false }, BatchEnd]), vec![]);
        assert_eq!(
            t.ev(&[leave_x(MEETING), enter_x(OTHER), BatchEnd]),
            vec![xf(OTHER)]
        );
        assert_eq!(t.fs.panel, None);
    }

    // Race (deliberate change): a new toplevel while a panel is open releases the panel.
    #[test]
    fn rule12_new_toplevel_releases_the_panel() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MEETING)), Some(MEETING));
        add(&mut t.roles, facts(A | 30), TOPLEVEL);
        let new_toplevel = Event::Mapped {
            window: win(A | 30),
            classification: Classification {
                kind: XKind::Toplevel,
                role: TOPLEVEL,
                focus_on_map: FocusOnMap::None,
            },
        };
        assert_eq!(
            t.ev(&[
                new_toplevel,
                Event::ActivationRequested(win(A | 30)),
                BatchEnd
            ]),
            vec![token(A | 30, KbTarget::X(win(MEETING)))]
        );
        assert_eq!(
            t.ev(&[leave_x(MEETING), enter_x(A | 30), BatchEnd]),
            vec![xf(A | 30)]
        );
        assert_eq!(t.fs.panel, None);
    }

    // Rule 12 drops: an Enter elsewhere drops it (then a later Enter on w is a hover),
    // a press drops it; no surface (Other/Nothing) means no token, the intent stays (Q20).
    #[test]
    fn rule12_drops_and_no_surface() {
        let mut t = world();
        t.panel_open(PANEL, Compositor::X(win(MEETING)), Some(MEETING));
        t.ev(&[
            Event::ActivationRequested(win(OTHER)),
            leave_x(MEETING),
            enter_x(MAIN),
        ]);
        assert_eq!(t.fs.pending_activation, None);
        assert_eq!(t.ev(&[leave_x(MAIN), enter_x(OTHER), BatchEnd]), vec![]);
        assert_eq!(t.fs.panel, Some(win(PANEL)));

        let mut t = world();
        t.focused(MAIN);
        t.ev(&[Event::ActivationRequested(win(OTHER)), press(MAIN)]);
        assert_eq!(t.fs.pending_activation, None);

        let mut t = world();
        t.fs.compositor = Compositor::Other;
        assert_eq!(t.ev(&[Event::ActivationRequested(win(OTHER))]), vec![]);
        assert_eq!(t.fs.pending_activation, Some(win(OTHER)));
    }

    // --- Rule 13 ---

    #[test]
    fn rule13_primary_output() {
        let mut t = world();
        t.roles.windows.get_mut(&win(MAIN)).unwrap().output = Some(O2);
        t.focused(MAIN);
        assert_eq!(t.ev(&[press(POPUP), BatchEnd]), vec![xf_on(POPUP, O2)]);
        // The X-focused toplevel moves output: the primary is set again.
        let mut t = world();
        t.focused(MAIN);
        t.roles.windows.get_mut(&win(MAIN)).unwrap().output = Some(O2);
        assert_eq!(
            t.ev(&[Event::OutputChanged(win(MAIN), O2), BatchEnd]),
            vec![xf_on(MAIN, O2)]
        );
        // Not for a window without X focus.
        assert_eq!(
            t.ev(&[Event::OutputChanged(win(MEETING), O2), BatchEnd]),
            vec![]
        );
    }

    // --- Routing (§2.0) ---

    #[test]
    fn routing_follows_desired_immediately() {
        let mut t = world();
        t.focused(MAIN);
        assert_eq!(t.ev(&[leave_x(MAIN), enter_overlay(O1)]), vec![route(MAIN)]);
        // A panel mapping in the same batch: the route moves before the batch ends.
        let out = t.ev(&[mapped(
            OVERLAY_PANEL,
            Role::OverlayWindow {
                output: O1,
                panel: true,
            },
            FocusOnMap::Panel,
        )]);
        assert_eq!(out, vec![route(OVERLAY_PANEL)]);
        assert_eq!(t.ev(&[BatchEnd]), vec![xf_on(OVERLAY_PANEL, O1)]);
        assert_eq!(
            t.ev(&[leave_overlay(O1)]),
            vec![Output::KeyboardRoute(None)]
        );
    }

    // --- Review Focus 3 ---

    #[test]
    fn unknown_windows_are_inert() {
        let mut t = world();
        t.focused(MAIN);
        let ghost = A | 99;
        assert_eq!(t.ev(&[Event::Gone(win(ghost)), BatchEnd]), vec![]);
        assert_eq!(t.ev(&[press(ghost), BatchEnd]), vec![xf(MAIN)]);
        assert_eq!(
            t.ev(&[Event::OutputChanged(win(ghost), O1), BatchEnd]),
            vec![]
        );
        assert_eq!(
            t.ev(&[Event::ActivationRequested(win(ghost)), BatchEnd]),
            vec![token(ghost, KbTarget::X(win(MAIN)))]
        );
        assert_eq!(t.ev(&[Event::Gone(win(ghost)), BatchEnd]), vec![]);
        assert_eq!(t.fs.pending_activation, None);
    }
}
