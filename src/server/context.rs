//! The model's one pipeline (design §3.3): what the model keeps of windows and outputs,
//! the press that may open a panel, the context classify reads, and the translation of
//! raw events into classify requests and focus events. The event layer, the scenario
//! runner and trace replay all go through `Model::feed`.

use super::classify::classify;
use super::focus::MAX_LINKS;
use super::model::{
    ClassifyContext, Event, FocusState, KbTarget, MachineInput, Millis, Model, Output, OutputId,
    PanelContext, PanelPlacement, ParentRole, Press, PressLog, PressRef, PressTarget, RawEvent,
    RecentPress, Role, RoleTable, SurfaceRef, WindowEntry, WindowFacts,
};
use crate::xstate::WindowDims;
use xcb::x;

/// How long after a press a notification window of the same client counts as opened by it.
pub const PRESS_OPENS_FOR: Millis = 1500;

impl RoleTable {
    /// Keeps what the model needs of `raw`; runs before `translate` sees it.
    pub fn apply(&mut self, raw: &RawEvent) {
        match raw {
            RawEvent::MapFacts(facts) => {
                let (output, classification) = match self.windows.get(&facts.window) {
                    Some(e) if e.mapped => (e.output, e.classification),
                    Some(e) => (e.output, None),
                    None => (None, None),
                };
                self.windows.insert(
                    facts.window,
                    WindowEntry {
                        facts: facts.clone(),
                        mapped: true,
                        classification,
                        output,
                    },
                );
            }
            RawEvent::RoleCreate { window, dims } => {
                if let Some(e) = self.windows.get_mut(window) {
                    e.facts.dims = *dims;
                }
            }
            RawEvent::Unmap { window } => {
                if let Some(e) = self.windows.get_mut(window) {
                    e.mapped = false;
                    e.classification = None;
                }
                if self.last_hovered == Some(*window) {
                    self.last_hovered = None;
                }
            }
            RawEvent::Destroy { window } => {
                self.windows.remove(window);
                if self.last_hovered == Some(*window) {
                    self.last_hovered = None;
                }
            }
            RawEvent::PointerEnter { window } => {
                // Popups are not where the user is (today's `last_hovered`).
                if self
                    .classification(*window)
                    .is_some_and(|c| c.role.is_toplevel())
                {
                    self.last_hovered = Some(*window);
                }
            }
            RawEvent::SurfaceEnterOutput { window, output } => {
                if let Some(e) = self.windows.get_mut(window) {
                    e.output = Some(*output);
                }
            }
            RawEvent::OutputGeometry {
                output,
                x_rect,
                mode,
            } => {
                let info = self.outputs.entry(*output).or_default();
                info.x_rect = *x_rect;
                info.mode = *mode;
            }
            RawEvent::OutputRemoved { output } => {
                self.outputs.remove(output);
                for e in self.windows.values_mut() {
                    if e.output == Some(*output) {
                        e.output = None;
                    }
                }
            }
            RawEvent::OverlayMapped { output } => {
                self.outputs.entry(*output).or_default().overlay_mapped = true;
            }
            RawEvent::OverlayClosed { output } => {
                if let Some(info) = self.outputs.get_mut(output) {
                    info.overlay_mapped = false;
                }
            }
            RawEvent::OverlayRehome { window, output } => {
                if let Some(c) = self
                    .windows
                    .get_mut(window)
                    .and_then(|e| e.classification.as_mut())
                    && let Role::OverlayWindow { output: o, .. } = &mut c.role
                {
                    *o = *output;
                }
            }
            RawEvent::HintsChanged { .. }
            | RawEvent::ActiveWindowRequest { .. }
            | RawEvent::KeyboardEnter { .. }
            | RawEvent::KeyboardLeave { .. }
            | RawEvent::Key { .. }
            | RawEvent::Press { .. }
            | RawEvent::PopupFirstConfigure { .. }
            | RawEvent::PopupDone { .. }
            | RawEvent::ActivationTokenDone { .. }
            | RawEvent::BatchEnd => {}
        }
    }

    /// `w` as an anchor (design §1.1-bis): a toplevel, what its links lead to, or nothing
    /// usable.
    pub fn parent_role(&self, w: x::Window) -> ParentRole {
        let mut current = w;
        let mut linked = false;
        for _ in 0..MAX_LINKS {
            let Some(entry) = self.windows.get(&current).filter(|e| e.mapped) else {
                return ParentRole::Unknown;
            };
            let Some(c) = entry.classification else {
                return if linked {
                    ParentRole::Unknown
                } else {
                    ParentRole::MappedNoRole
                };
            };
            match c.role {
                Role::Toplevel { .. } | Role::FullscreenToplevel { .. } => {
                    return if linked {
                        ParentRole::OfToplevel(current)
                    } else {
                        ParentRole::Toplevel(current)
                    };
                }
                Role::OverlayWindow { .. } => return ParentRole::Overlay,
                Role::PanelOf { parent } => {
                    return if self
                        .classification(parent)
                        .is_some_and(|c| c.role.is_toplevel())
                    {
                        ParentRole::OfToplevel(parent)
                    } else {
                        ParentRole::Unknown
                    };
                }
                Role::Popup { parent } | Role::OverlayPopupOf { panel: parent } => {
                    current = parent;
                    linked = true;
                }
            }
        }
        ParentRole::Unknown
    }

    /// The output whose X rect holds the centre of a window at `dims`, if its overlay is
    /// mapped.
    pub fn overlay_under(&self, dims: WindowDims) -> Option<OutputId> {
        let x = i32::from(dims.x) + i32::from(dims.width) / 2;
        let y = i32::from(dims.y) + i32::from(dims.height) / 2;
        self.outputs
            .iter()
            .find(|(_, info)| info.overlay_mapped && info.x_rect.contains(x, y))
            .map(|(output, _)| *output)
    }
}

impl PressLog {
    /// Records a pointer press on an X window; nothing else replaces it (Q12).
    pub fn apply(&mut self, raw: &RawEvent, roles: &RoleTable, now: Millis) {
        if let RawEvent::Press {
            target: PressRef::X(window),
            touch: false,
            ..
        } = raw
            && let Some(entry) = roles.entry(*window)
        {
            self.last = Some(Press {
                window: *window,
                client: entry.facts.client,
                at: now,
            });
        }
    }

    /// The latest press, if it may still have opened a window.
    pub fn recent(&self, now: Millis) -> Option<Press> {
        self.last
            .filter(|press| now.saturating_sub(press.at) <= PRESS_OPENS_FOR)
    }
}

fn panel_context(roles: &RoleTable, panel: x::Window) -> Option<PanelContext> {
    let entry = roles.entry(panel)?;
    let placement = match entry.classification?.role {
        Role::PanelOf { parent } => PanelPlacement::OfToplevel(parent),
        Role::OverlayWindow {
            output,
            panel: true,
        } => PanelPlacement::Overlay(output),
        _ => return None,
    };
    Some(PanelContext {
        window: panel,
        client: entry.facts.client,
        placement,
    })
}

/// What classify reads about the world when `facts`' window gets its role.
pub fn derive_context(
    facts: &WindowFacts,
    roles: &RoleTable,
    focus: &FocusState,
    presses: &PressLog,
    now: Millis,
) -> ClassifyContext {
    let has_role = |w: &x::Window| roles.classification(*w).is_some();
    ClassifyContext {
        transient_parent: facts.transient_for.map(|p| roles.parent_role(p)),
        recent_press: presses.recent(now).map(|p| RecentPress {
            window: p.window,
            client: p.client,
            role: roles.parent_role(p.window),
        }),
        panel: focus.panel.and_then(|p| panel_context(roles, p)),
        last_hovered: roles.last_hovered.filter(has_role),
        last_toplevel: focus.last_toplevel.filter(has_role),
        centre_overlay: roles.overlay_under(facts.dims),
        fallback_overlay: fallback_overlay(roles, focus),
        output_modes: roles.outputs.values().map(|o| o.mode).collect(),
    }
}

/// The overlay a client-placed window goes on when none is mapped under its centre: that of
/// the output the last focused toplevel is on, if mapped (today's fallback, Q4).
pub fn fallback_overlay(roles: &RoleTable, focus: &FocusState) -> Option<OutputId> {
    focus
        .last_toplevel
        .and_then(|t| roles.entry(t)?.output)
        .filter(|o| roles.outputs.get(o).is_some_and(|i| i.overlay_mapped))
}

fn keyboard_target(target: SurfaceRef) -> Option<KbTarget> {
    match target {
        SurfaceRef::X(w) => Some(KbTarget::X(w)),
        SurfaceRef::Overlay(o) => Some(KbTarget::Overlay(o)),
        SurfaceRef::Other => None,
    }
}

fn press_target(target: PressRef, roles: &RoleTable) -> PressTarget {
    match target {
        PressRef::X(w) => match roles.classification(w).map(|c| c.role) {
            Some(Role::OverlayWindow { output, .. }) => {
                PressTarget::OverlayWindow { window: w, output }
            }
            _ => PressTarget::Window(w),
        },
        // A press on satellite's titlebar is a press on its window.
        PressRef::Decoration(w) => PressTarget::Window(w),
        PressRef::Overlay(_) | PressRef::Other => PressTarget::Other,
    }
}

/// What `raw` asks of the model, read against the table `RoleTable::apply` has updated.
pub fn translate(raw: &RawEvent, roles: &RoleTable) -> Vec<MachineInput> {
    use MachineInput::{Classify, Focus};
    match raw {
        RawEvent::RoleCreate { window, .. } if roles.is_mapped(*window) => {
            vec![Classify { window: *window }]
        }
        RawEvent::Unmap { window } | RawEvent::Destroy { window } => {
            vec![Focus(Event::Gone(*window))]
        }
        RawEvent::ActiveWindowRequest { window } => {
            vec![Focus(Event::ActivationRequested(*window))]
        }
        RawEvent::KeyboardEnter { target, .. } => keyboard_target(*target)
            .map(|t| Focus(Event::KeyboardEnter(t)))
            .into_iter()
            .collect(),
        RawEvent::KeyboardLeave { target, .. } => keyboard_target(*target)
            .map(|t| Focus(Event::KeyboardLeave(t)))
            .into_iter()
            .collect(),
        RawEvent::Key { pressed, .. } => vec![Focus(Event::Key { pressed: *pressed })],
        RawEvent::Press { target, .. } => vec![Focus(Event::Press(press_target(*target, roles)))],
        RawEvent::PopupFirstConfigure { window } => match roles.classification(*window) {
            Some(classification) if !classification.role.is_toplevel() => {
                vec![Focus(Event::Mapped {
                    window: *window,
                    classification,
                })]
            }
            _ => vec![],
        },
        RawEvent::SurfaceEnterOutput { window, output } => {
            vec![Focus(Event::OutputChanged(*window, *output))]
        }
        RawEvent::OutputRemoved { output } | RawEvent::OverlayClosed { output } => {
            vec![Focus(Event::OverlayGone(*output))]
        }
        RawEvent::BatchEnd => vec![Focus(Event::BatchEnd)],
        RawEvent::RoleCreate { .. }
        | RawEvent::MapFacts(_)
        | RawEvent::HintsChanged { .. }
        | RawEvent::PointerEnter { .. }
        | RawEvent::PopupDone { .. }
        | RawEvent::OutputGeometry { .. }
        | RawEvent::OverlayMapped { .. }
        | RawEvent::OverlayRehome { .. }
        | RawEvent::ActivationTokenDone { .. } => vec![],
    }
}

impl Model {
    /// Feeds one raw event through the model; returns what the event layer must do.
    pub fn feed(&mut self, raw: &RawEvent, now: Millis) -> Vec<Output> {
        self.roles.apply(raw);
        self.presses.apply(raw, &self.roles, now);
        let mut out = Vec::new();
        for input in translate(raw, &self.roles) {
            match input {
                MachineInput::Classify { window } => out.extend(self.classify_window(window, now)),
                MachineInput::Focus(event) => out.extend(self.focus.handle(event, &self.roles)),
            }
        }
        out
    }

    /// Classifies `window` now; a toplevel is mapped at once and activated (M2), a popup
    /// waits for its first configure.
    fn classify_window(&mut self, window: x::Window, now: Millis) -> Vec<Output> {
        let Some(facts) = self.roles.entry(window).map(|e| e.facts.clone()) else {
            return Vec::new();
        };
        let ctx = derive_context(&facts, &self.roles, &self.focus, &self.presses, now);
        let classification = classify(&facts, &ctx);
        if let Some(entry) = self.roles.windows.get_mut(&window) {
            entry.classification = Some(classification);
        }
        let mut out = Vec::new();
        if classification.role.is_toplevel() {
            let mapped = Event::Mapped {
                window,
                classification,
            };
            out.extend(self.focus.handle(mapped, &self.roles));
            out.extend(
                self.focus
                    .handle(Event::ActivationRequested(window), &self.roles),
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::focus::in_family;
    use crate::server::model::testkit::*;
    use crate::server::model::*;
    use crate::xstate::WindowRole;

    fn geometry(o: OutputId, x: i32) -> RawEvent {
        RawEvent::OutputGeometry {
            output: o,
            x_rect: XRect {
                x,
                y: 0,
                width: 3840,
                height: 2160,
            },
            mode: (3840, 2160),
        }
    }

    #[test]
    fn apply_keeps_what_the_model_needs() {
        let mut roles = RoleTable::default();
        let t = facts(A | 1).types(&[NetWmType::Normal]);
        roles.apply(&RawEvent::MapFacts(t.clone()));
        assert!(roles.is_mapped(win(A | 1)));
        roles.apply(&RawEvent::RoleCreate {
            window: win(A | 1),
            dims: dims(5, 6, 70, 80),
        });
        assert_eq!(
            roles.entry(win(A | 1)).unwrap().facts.dims,
            dims(5, 6, 70, 80)
        );
        roles.windows.get_mut(&win(A | 1)).unwrap().classification = Some(Classification {
            kind: XKind::Toplevel,
            role: TOPLEVEL,
            focus_on_map: FocusOnMap::None,
        });
        roles.apply(&RawEvent::SurfaceEnterOutput {
            window: win(A | 1),
            output: O1,
        });
        roles.apply(&RawEvent::PointerEnter { window: win(A | 1) });
        assert_eq!(roles.last_hovered, Some(win(A | 1)));
        // A second MapNotify for a mapped window keeps its role.
        roles.apply(&RawEvent::MapFacts(t.clone()));
        assert!(roles.classification(win(A | 1)).is_some());
        // Unmap forgets the role and the hover, keeps the output; a remap keeps the output.
        roles.apply(&RawEvent::Unmap { window: win(A | 1) });
        assert_eq!(roles.classification(win(A | 1)), None);
        assert_eq!(roles.last_hovered, None);
        roles.apply(&RawEvent::MapFacts(t));
        assert_eq!(roles.entry(win(A | 1)).unwrap().output, Some(O1));
        roles.apply(&RawEvent::Destroy { window: win(A | 1) });
        assert!(roles.entry(win(A | 1)).is_none());
    }

    #[test]
    fn pointer_enter_tracks_toplevels_only() {
        let mut roles = RoleTable::default();
        add(&mut roles, facts(A | 1), TOPLEVEL);
        add(
            &mut roles,
            facts(A | 2).or(),
            Role::Popup { parent: win(A | 1) },
        );
        roles.apply(&RawEvent::PointerEnter { window: win(A | 1) });
        roles.apply(&RawEvent::PointerEnter { window: win(A | 2) });
        roles.apply(&RawEvent::PointerEnter { window: win(A | 9) });
        assert_eq!(roles.last_hovered, Some(win(A | 1)));
    }

    #[test]
    fn outputs_overlays_and_rehoming() {
        let mut roles = RoleTable::default();
        roles.apply(&geometry(O1, 0));
        roles.apply(&geometry(O2, 3840));
        roles.apply(&RawEvent::OverlayMapped { output: O2 });
        assert_eq!(roles.overlay_under(dims(4000, 10, 100, 100)), Some(O2));
        assert_eq!(roles.overlay_under(dims(10, 10, 100, 100)), None);
        roles.apply(&RawEvent::OverlayMapped { output: O1 });
        // The centre decides, not the corner.
        assert_eq!(roles.overlay_under(dims(3790, 10, 100, 100)), Some(O2));
        add(
            &mut roles,
            facts(A | 4),
            Role::OverlayWindow {
                output: O1,
                panel: false,
            },
        );
        roles.windows.get_mut(&win(A | 4)).unwrap().output = Some(O1);
        roles.apply(&RawEvent::OverlayRehome {
            window: win(A | 4),
            output: O2,
        });
        assert_eq!(
            roles.classification(win(A | 4)).map(|c| c.role),
            Some(Role::OverlayWindow {
                output: O2,
                panel: false
            })
        );
        roles.apply(&RawEvent::OverlayClosed { output: O2 });
        assert_eq!(roles.overlay_under(dims(4000, 10, 100, 100)), None);
        roles.apply(&RawEvent::OutputRemoved { output: O1 });
        assert!(!roles.outputs.contains_key(&O1));
        assert_eq!(roles.entry(win(A | 4)).unwrap().output, None);
    }

    #[test]
    fn parent_roles() {
        let mut roles = RoleTable::default();
        add(&mut roles, facts(A | 1), TOPLEVEL);
        add(
            &mut roles,
            facts(A | 2),
            Role::FullscreenToplevel {
                parent: None,
                fixed_size: false,
            },
        );
        add(
            &mut roles,
            facts(A | 3),
            Role::PanelOf { parent: win(A | 1) },
        );
        add(&mut roles, facts(A | 4), Role::Popup { parent: win(A | 3) });
        add(
            &mut roles,
            facts(A | 5),
            Role::OverlayWindow {
                output: O1,
                panel: true,
            },
        );
        add(
            &mut roles,
            facts(A | 6).or(),
            Role::OverlayPopupOf { panel: win(A | 5) },
        );
        add_unclassified(&mut roles, facts(A | 7));
        add(
            &mut roles,
            facts(A | 8),
            Role::PanelOf {
                parent: win(A | 99),
            },
        );
        assert_eq!(
            roles.parent_role(win(A | 1)),
            ParentRole::Toplevel(win(A | 1))
        );
        assert_eq!(
            roles.parent_role(win(A | 2)),
            ParentRole::Toplevel(win(A | 2))
        );
        assert_eq!(
            roles.parent_role(win(A | 3)),
            ParentRole::OfToplevel(win(A | 1))
        );
        assert_eq!(
            roles.parent_role(win(A | 4)),
            ParentRole::OfToplevel(win(A | 1))
        );
        assert_eq!(roles.parent_role(win(A | 5)), ParentRole::Overlay);
        assert_eq!(roles.parent_role(win(A | 6)), ParentRole::Overlay);
        assert_eq!(roles.parent_role(win(A | 7)), ParentRole::MappedNoRole);
        assert_eq!(roles.parent_role(win(A | 8)), ParentRole::Unknown);
        assert_eq!(roles.parent_role(win(A | 9)), ParentRole::Unknown);
        roles.windows.get_mut(&win(A | 1)).unwrap().mapped = false;
        assert_eq!(roles.parent_role(win(A | 1)), ParentRole::Unknown);
    }

    // Review Focus 2.
    #[test]
    fn parent_role_terminates_on_cycle() {
        let mut roles = RoleTable::default();
        add(&mut roles, facts(A | 1), Role::Popup { parent: win(A | 2) });
        add(&mut roles, facts(A | 2), Role::Popup { parent: win(A | 1) });
        assert_eq!(roles.parent_role(win(A | 1)), ParentRole::Unknown);
    }

    #[test]
    fn press_log_keeps_todays_last_press() {
        let mut roles = RoleTable::default();
        add(&mut roles, facts(A | 1), TOPLEVEL);
        let mut log = PressLog::default();
        let press = |target, touch| RawEvent::Press {
            target,
            serial: 1,
            touch,
        };
        log.apply(&press(PressRef::X(win(A | 1)), false), &roles, 1_000);
        assert_eq!(
            log.last,
            Some(Press {
                window: win(A | 1),
                client: A,
                at: 1_000
            })
        );
        // Decorations, touch, other surfaces and unknown windows do not replace it.
        log.apply(
            &press(PressRef::Decoration(win(A | 1)), false),
            &roles,
            1_100,
        );
        log.apply(&press(PressRef::X(win(A | 1)), true), &roles, 1_200);
        log.apply(&press(PressRef::Other, false), &roles, 1_300);
        log.apply(&press(PressRef::X(win(A | 9)), false), &roles, 1_400);
        assert_eq!(log.last.map(|p| p.at), Some(1_000));
        assert!(log.recent(2_500).is_some());
        assert!(log.recent(2_501).is_none());
    }

    // Review Focus 4.
    #[test]
    fn press_age_saturates() {
        let log = PressLog {
            last: Some(Press {
                window: win(A | 1),
                client: A,
                at: 2_000,
            }),
        };
        assert_eq!(log.recent(1_000).map(|p| p.window), Some(win(A | 1)));
    }

    #[test]
    fn context_for_a_window() {
        let mut roles = RoleTable::default();
        output(&mut roles, O1, 0);
        output(&mut roles, O2, 3840);
        add(&mut roles, facts(A | 1), TOPLEVEL);
        roles.windows.get_mut(&win(A | 1)).unwrap().output = Some(O1);
        add(
            &mut roles,
            facts(A | 3),
            Role::PanelOf { parent: win(A | 1) },
        );
        let focus = FocusState {
            panel: Some(win(A | 3)),
            last_toplevel: Some(win(A | 1)),
            ..FocusState::default()
        };
        let presses = PressLog {
            last: Some(Press {
                window: win(A | 1),
                client: A,
                at: 1_000,
            }),
        };
        let f = facts(A | 4).transient(A | 1).at(4000, 100, 200, 100);
        assert_eq!(
            derive_context(&f, &roles, &focus, &presses, 2_000),
            ClassifyContext {
                transient_parent: Some(ParentRole::Toplevel(win(A | 1))),
                recent_press: Some(RecentPress {
                    window: win(A | 1),
                    client: A,
                    role: ParentRole::Toplevel(win(A | 1)),
                }),
                panel: Some(PanelContext {
                    window: win(A | 3),
                    client: A,
                    placement: PanelPlacement::OfToplevel(win(A | 1)),
                }),
                last_hovered: None,
                last_toplevel: Some(win(A | 1)),
                centre_overlay: Some(O2),
                fallback_overlay: Some(O1),
                output_modes: vec![(3840, 2160), (3840, 2160)],
            }
        );
        assert_eq!(
            derive_context(&f, &roles, &focus, &presses, 2_600).recent_press,
            None
        );
    }

    #[test]
    fn translate_rows() {
        let mut roles = RoleTable::default();
        add(&mut roles, facts(A | 1), TOPLEVEL);
        add(
            &mut roles,
            facts(A | 2).or(),
            Role::Popup { parent: win(A | 1) },
        );
        add(
            &mut roles,
            facts(A | 4),
            Role::OverlayWindow {
                output: O1,
                panel: false,
            },
        );
        roles.windows.insert(
            win(A | 5),
            WindowEntry {
                facts: facts(A | 5),
                mapped: false,
                classification: None,
                output: None,
            },
        );
        use MachineInput::{Classify, Focus};
        let w = win;
        let rows: Vec<(RawEvent, Vec<MachineInput>)> = vec![
            (RawEvent::MapFacts(facts(A | 1)), vec![]),
            (
                RawEvent::RoleCreate {
                    window: w(A | 1),
                    dims: dims(0, 0, 1, 1),
                },
                vec![Classify { window: w(A | 1) }],
            ),
            (
                RawEvent::RoleCreate {
                    window: w(A | 5),
                    dims: dims(0, 0, 1, 1),
                },
                vec![],
            ),
            (
                RawEvent::Unmap { window: w(A | 1) },
                vec![Focus(Event::Gone(w(A | 1)))],
            ),
            (
                RawEvent::Destroy { window: w(A | 1) },
                vec![Focus(Event::Gone(w(A | 1)))],
            ),
            (
                RawEvent::ActiveWindowRequest { window: w(A | 1) },
                vec![Focus(Event::ActivationRequested(w(A | 1)))],
            ),
            (
                RawEvent::KeyboardEnter {
                    target: SurfaceRef::X(w(A | 1)),
                    serial: 1,
                },
                vec![Focus(Event::KeyboardEnter(KbTarget::X(w(A | 1))))],
            ),
            (
                RawEvent::KeyboardLeave {
                    target: SurfaceRef::Overlay(O1),
                    serial: 1,
                },
                vec![Focus(Event::KeyboardLeave(KbTarget::Overlay(O1)))],
            ),
            (
                RawEvent::KeyboardEnter {
                    target: SurfaceRef::Other,
                    serial: 1,
                },
                vec![],
            ),
            (
                RawEvent::Key {
                    pressed: true,
                    serial: 1,
                },
                vec![Focus(Event::Key { pressed: true })],
            ),
            (
                RawEvent::Press {
                    target: PressRef::X(w(A | 4)),
                    serial: 1,
                    touch: false,
                },
                vec![Focus(Event::Press(PressTarget::OverlayWindow {
                    window: w(A | 4),
                    output: O1,
                }))],
            ),
            (
                RawEvent::Press {
                    target: PressRef::X(w(A | 2)),
                    serial: 1,
                    touch: true,
                },
                vec![Focus(Event::Press(PressTarget::Window(w(A | 2))))],
            ),
            (
                RawEvent::Press {
                    target: PressRef::Decoration(w(A | 1)),
                    serial: 1,
                    touch: false,
                },
                vec![Focus(Event::Press(PressTarget::Window(w(A | 1))))],
            ),
            (
                RawEvent::Press {
                    target: PressRef::Overlay(O1),
                    serial: 1,
                    touch: false,
                },
                vec![Focus(Event::Press(PressTarget::Other))],
            ),
            (
                RawEvent::PopupFirstConfigure { window: w(A | 2) },
                vec![Focus(Event::Mapped {
                    window: w(A | 2),
                    classification: roles.classification(w(A | 2)).unwrap(),
                })],
            ),
            (RawEvent::PopupFirstConfigure { window: w(A | 1) }, vec![]),
            (
                RawEvent::SurfaceEnterOutput {
                    window: w(A | 1),
                    output: O2,
                },
                vec![Focus(Event::OutputChanged(w(A | 1), O2))],
            ),
            (
                RawEvent::OutputRemoved { output: O1 },
                vec![Focus(Event::OverlayGone(O1))],
            ),
            (
                RawEvent::OverlayClosed { output: O1 },
                vec![Focus(Event::OverlayGone(O1))],
            ),
            (RawEvent::BatchEnd, vec![Focus(Event::BatchEnd)]),
            (RawEvent::PointerEnter { window: w(A | 1) }, vec![]),
            (RawEvent::PopupDone { window: w(A | 2) }, vec![]),
            (
                RawEvent::HintsChanged {
                    window: w(A | 2),
                    input: InputHint::True,
                },
                vec![],
            ),
            (RawEvent::OverlayMapped { output: O1 }, vec![]),
            (
                RawEvent::OverlayRehome {
                    window: w(A | 4),
                    output: O2,
                },
                vec![],
            ),
            (RawEvent::ActivationTokenDone { window: w(A | 1) }, vec![]),
        ];
        for (raw, want) in rows {
            assert_eq!(translate(&raw, &roles), want, "{raw:?}");
        }
        // Review Focus 3: a first configure arriving after the window's unmap does nothing.
        roles.apply(&RawEvent::Unmap { window: w(A | 2) });
        assert_eq!(
            translate(&RawEvent::PopupFirstConfigure { window: w(A | 2) }, &roles),
            vec![]
        );
    }

    #[test]
    fn model_classifies_at_role_creation_and_focuses_at_first_configure() {
        let mut m = Model::default();
        for raw in [
            geometry(O1, 0),
            RawEvent::OverlayMapped { output: O1 },
            RawEvent::BatchEnd,
        ] {
            assert_eq!(m.feed(&raw, 1_000), vec![]);
        }
        let main = facts(A | 1)
            .types(&[NetWmType::Normal])
            .at(0, 0, 1600, 1200);
        assert_eq!(m.feed(&RawEvent::MapFacts(main), 1_000), vec![]);
        // No surface has the compositor's focus yet: no token; the activation is pending.
        assert_eq!(
            m.feed(
                &RawEvent::RoleCreate {
                    window: win(A | 1),
                    dims: dims(0, 0, 1600, 1200)
                },
                1_000
            ),
            vec![]
        );
        assert_eq!(
            m.roles.classification(win(A | 1)).map(|c| c.role),
            Some(TOPLEVEL)
        );
        assert_eq!(m.focus.pending_activation, Some(win(A | 1)));
        m.feed(
            &RawEvent::KeyboardEnter {
                target: SurfaceRef::X(win(A | 1)),
                serial: 1,
            },
            1_000,
        );
        let set_input = |w: u32| {
            Output::XFocus(XFocusChange::Window {
                window: win(w),
                method: Method::SetInput,
                primary_output: None,
            })
        };
        assert_eq!(m.feed(&RawEvent::BatchEnd, 1_000), vec![set_input(A | 1)]);

        let menu = facts(A | 5)
            .types(&[NetWmType::PopupMenu])
            .guessed(WindowRole::Popup)
            .input(InputHint::True)
            .at(10, 10, 50, 50);
        m.feed(&RawEvent::MapFacts(menu), 1_000);
        assert_eq!(
            m.feed(
                &RawEvent::RoleCreate {
                    window: win(A | 5),
                    dims: dims(10, 10, 50, 50)
                },
                1_000
            ),
            vec![]
        );
        assert_eq!(m.feed(&RawEvent::BatchEnd, 1_000), vec![]);
        assert_eq!(
            m.roles.classification(win(A | 5)).map(|c| c.role),
            Some(Role::Popup { parent: win(A | 1) })
        );
        m.feed(&RawEvent::PopupFirstConfigure { window: win(A | 5) }, 1_000);
        assert_eq!(m.feed(&RawEvent::BatchEnd, 1_000), vec![set_input(A | 5)]);

        // A new toplevel: activation from the surface the compositor is on (M2).
        m.feed(&RawEvent::MapFacts(facts(A | 6).at(0, 0, 640, 480)), 1_000);
        assert_eq!(
            m.feed(
                &RawEvent::RoleCreate {
                    window: win(A | 6),
                    dims: dims(0, 0, 640, 480)
                },
                1_000
            ),
            vec![Output::ActivationToken {
                window: win(A | 6),
                surface: KbTarget::X(win(A | 1))
            }]
        );
    }

    // Review Focus 1: a window remapped under the same id as another kind.
    #[test]
    fn remap_same_id_as_other_kind() {
        let mut m = Model::default();
        let feed = |m: &mut Model, raw: RawEvent| m.feed(&raw, 1_000);
        feed(&mut m, geometry(O1, 0));
        feed(&mut m, RawEvent::OverlayMapped { output: O1 });
        let meeting = facts(A | 2)
            .types(&[NetWmType::Normal])
            .at(0, 0, 1600, 1200);
        feed(&mut m, RawEvent::MapFacts(meeting));
        feed(
            &mut m,
            RawEvent::RoleCreate {
                window: win(A | 2),
                dims: dims(0, 0, 1600, 1200),
            },
        );
        let panel = facts(A | 3)
            .types(&[NetWmType::Notification])
            .transient(A | 2)
            .at(1600, 1760, 904, 256);
        feed(&mut m, RawEvent::MapFacts(panel));
        feed(
            &mut m,
            RawEvent::RoleCreate {
                window: win(A | 3),
                dims: dims(1600, 1760, 904, 256),
            },
        );
        feed(&mut m, RawEvent::PopupFirstConfigure { window: win(A | 3) });
        feed(&mut m, RawEvent::BatchEnd);
        assert_eq!(m.focus.panel, Some(win(A | 3)));
        feed(&mut m, RawEvent::Unmap { window: win(A | 3) });
        feed(&mut m, RawEvent::BatchEnd);
        assert_eq!(m.focus.panel, None);
        feed(
            &mut m,
            RawEvent::MapFacts(facts(A | 3).types(&[NetWmType::Normal]).at(0, 0, 800, 600)),
        );
        feed(
            &mut m,
            RawEvent::RoleCreate {
                window: win(A | 3),
                dims: dims(0, 0, 800, 600),
            },
        );
        assert_eq!(
            m.roles.classification(win(A | 3)).map(|c| c.role),
            Some(TOPLEVEL)
        );
        assert!(!in_family(&m.roles, win(A | 3), win(A | 2)));
        assert_eq!(m.focus.panel, None);
    }
}
