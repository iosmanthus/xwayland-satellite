//! The window model's shared types: the facts classify reads about a window, the context it
//! reads about the others, what it decides, the focus machine's events, state and outputs,
//! and the raw events every driver (the event layer, the scenario runner, trace replay)
//! feeds through one pipeline, [`Model::feed`]. Plain data: the rules live in `classify`,
//! `focus` and `context`. Trace and fixture schemas: docs/window-model/.

// Wired into the event layer in T5; T5c removes this.
#![allow(dead_code)]

use crate::xstate::{WindowDims, WindowRole};
use std::collections::{BTreeMap, HashMap};
use xcb::x;

/// Milliseconds on satellite's monotonic clock; the tracer stamps the same clock, so press
/// ages replayed from a trace equal the live ones.
pub type Millis = u64;

/// An output, by its wl_output global name (stable for the output's life, recorded in traces).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutputId(pub u32);

/// An entry of `_NET_WM_WINDOW_TYPE`. `Other` is any type upstream's guess does not
/// recognise and that is not NOTIFICATION (DOCK, `_KDE_NET_WM_WINDOW_TYPE_OVERRIDE`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetWmType {
    Normal,
    Dialog,
    Utility,
    Splash,
    Menu,
    PopupMenu,
    DropdownMenu,
    Tooltip,
    Dnd,
    Combo,
    Notification,
    Other,
}

/// WM_HINTS' input field. ICCCM: WM_HINTS without the Input flag means input is true, so
/// only a window with no WM_HINTS at all is `Absent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputHint {
    #[default]
    Absent,
    True,
    False,
}

/// `_MOTIF_WM_HINTS` functions and decorations as their known bits (unknown bits dropped, as
/// xstate's bitflags drop them); `None` when the property or the field's flag is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MotifHints {
    pub functions: Option<u32>,
    pub decorations: Option<u32>,
}

impl MotifHints {
    /// Functions given and none of them: the user can do nothing to the window (A2).
    pub fn functions_empty(&self) -> bool {
        self.functions == Some(0)
    }

    /// Decorations given and none of them (upstream's `motif_no_decor`).
    pub fn no_decorations(&self) -> bool {
        self.decorations == Some(0)
    }
}

/// WM_NORMAL_HINTS as classify reads them, in X pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SizeHints {
    pub min: Option<(i32, i32)>,
    pub max: Option<(i32, i32)>,
    /// USPosition or PPosition: the window asks for a position of its own.
    pub position: bool,
}

impl SizeHints {
    /// Minimum and maximum given and equal (upstream's `forced_size`).
    pub fn forced_size(&self) -> bool {
        matches!((self.min, self.max), (Some(min), Some(max)) if min == max)
    }
}

/// What X says about a window, read afresh at every map (spec §1.1 Input A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFacts {
    pub window: x::Window,
    pub override_redirect: bool,
    /// In `_NET_WM_WINDOW_TYPE` order; empty when the property is missing.
    pub types: Vec<NetWmType>,
    pub motif: MotifHints,
    pub class: Option<String>,
    pub size_hints: Option<SizeHints>,
    pub input: InputHint,
    /// `_NET_WM_STATE` holds `_NET_WM_STATE_ABOVE`.
    pub keep_above: bool,
    pub transient_for: Option<x::Window>,
    /// WM_PROTOCOLS holds WM_TAKE_FOCUS.
    pub take_focus: bool,
    /// WM_PROTOCOLS holds WM_DELETE_WINDOW.
    pub delete: bool,
    /// X geometry; at role creation, the geometry at that instant.
    pub dims: WindowDims,
    /// The window's resource id masked with the connection's resource-id mask: equal for
    /// windows of one X client.
    pub client: u32,
    /// Upstream's `guess_window_role`.
    pub guessed: WindowRole,
}

/// What the X-side layer makes of a window: upstream's guess, or the fork arm that fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XKind {
    /// Upstream guessed Toplevel.
    Toplevel,
    /// Upstream guessed Popup.
    Popup,
    /// Upstream guessed Splash.
    Splash,
    /// A9: the first recognised window type is NOTIFICATION.
    Notification,
    /// A4: a frameless keep-above NORMAL window that places itself (Feishu's call bar).
    CallBar,
    /// A7's no-input clause: a frameless transient UTILITY window with WM_HINTS input false
    /// (WeChat's Moments bubble).
    NoInputHelper,
}

impl XKind {
    /// Today's `role.is_popup()`: what may move itself after map, among others.
    pub fn is_popup(self) -> bool {
        matches!(self, XKind::Popup | XKind::NoInputHelper)
    }

    /// Today's `role.is_fixed_size()`: a toplevel of this kind keeps its client's size.
    pub fn is_fixed_size(self) -> bool {
        matches!(self, XKind::Splash | XKind::Notification | XKind::CallBar)
    }
}

/// How a window is shown (spec §1.1 Output).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A window the size of an output (B1). `parent`: its transient parent, if that is a
    /// toplevel and the window is not popup-kind; `fixed_size`: min = max = its size.
    FullscreenToplevel {
        parent: Option<x::Window>,
        fixed_size: bool,
    },
    Toplevel {
        parent: Option<x::Window>,
        fixed_size: bool,
    },
    /// An xdg_popup of `parent`, placed from its X geometry.
    Popup { parent: x::Window },
    /// A focused panel shown as an xdg_popup of toplevel `parent`.
    PanelOf { parent: x::Window },
    /// A popup of `output`'s overlay at the window's own X position.
    OverlayWindow { output: OutputId, panel: bool },
    /// An override-redirect popup (IME candidates) over the focused panel: on the panel's
    /// overlay at its own X position, or an xdg_popup of a `PanelOf` panel.
    OverlayPopupOf { panel: x::Window },
}

impl Role {
    pub fn is_toplevel(self) -> bool {
        matches!(
            self,
            Role::Toplevel { .. } | Role::FullscreenToplevel { .. }
        )
    }

    pub fn is_panel(self) -> bool {
        matches!(
            self,
            Role::PanelOf { .. } | Role::OverlayWindow { panel: true, .. }
        )
    }

    /// The roles upstream's #494 rule decides focus for.
    pub fn is_popup(self) -> bool {
        matches!(self, Role::Popup { .. } | Role::OverlayPopupOf { .. })
    }

    /// The window this one's classification links to (Family, spec §2 terms).
    pub fn link(self) -> Option<x::Window> {
        match self {
            Role::Popup { parent } | Role::PanelOf { parent } => Some(parent),
            Role::OverlayPopupOf { panel } => Some(panel),
            _ => None,
        }
    }
}

/// What the window does to X focus when it is mapped (spec §1.1 Output).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusOnMap {
    None,
    SetInput,
    TakeFocus,
    /// It becomes the single open panel (rule 3).
    Panel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classification {
    pub kind: XKind,
    pub role: Role,
    pub focus_on_map: FocusOnMap,
}

/// A window classify places another relative to (a transient parent, a pressed window), as
/// the anchor table of spec §1.1-bis reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentRole {
    /// Itself a Toplevel or FullscreenToplevel.
    Toplevel(x::Window),
    /// A `PanelOf{T}` panel, or a window whose links lead to one or to toplevel T.
    OfToplevel(x::Window),
    /// An overlay window, or a window whose links lead to one.
    Overlay,
    /// X-mapped, but its role is not created yet.
    MappedNoRole,
    /// Unmapped, unknown, or links that lead nowhere usable.
    Unknown,
}

/// The latest press, if it is at most 1.5 s old.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecentPress {
    pub window: x::Window,
    pub client: u32,
    pub role: ParentRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelPlacement {
    Overlay(OutputId),
    OfToplevel(x::Window),
}

/// The open focused panel, as classify reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelContext {
    pub window: x::Window,
    pub client: u32,
    pub placement: PanelPlacement,
}

/// What classify reads about the rest of the world (spec §1.1 Input B).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClassifyContext {
    /// `None` without WM_TRANSIENT_FOR.
    pub transient_parent: Option<ParentRole>,
    pub recent_press: Option<RecentPress>,
    pub panel: Option<PanelContext>,
    /// The last hovered toplevel, only if it has a role.
    pub last_hovered: Option<x::Window>,
    /// The focus machine's `last_toplevel`, only if it has a role.
    pub last_toplevel: Option<x::Window>,
    /// The output whose X rect holds the window's centre, if its overlay is mapped.
    pub centre_overlay: Option<OutputId>,
    /// `last_toplevel`'s output, if its overlay is mapped (Q4).
    pub fallback_overlay: Option<OutputId>,
    /// Every output's mode size (unrotated), for the fullscreen heuristic (Q5).
    pub output_modes: Vec<(i32, i32)>,
}

/// Where the compositor's keyboard focus is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compositor {
    X(x::Window),
    Overlay(OutputId),
    /// A surface that is not satellite's (a Leave with no Enter after it).
    Other,
    #[default]
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbTarget {
    X(x::Window),
    Overlay(OutputId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressTarget {
    /// An X surface or satellite's decoration of window w.
    Window(x::Window),
    /// The surface of an `OverlayWindow` (never override-redirect).
    OverlayWindow {
        window: x::Window,
        output: OutputId,
    },
    Other,
}

/// The focus machine's inputs (spec §1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    KeyboardEnter(KbTarget),
    KeyboardLeave(KbTarget),
    Key {
        pressed: bool,
    },
    Press(PressTarget),
    /// Popups and panels at their first xdg configure; toplevels at role creation.
    Mapped {
        window: x::Window,
        classification: Classification,
    },
    /// Unmap, destroy, reparent away.
    Gone(x::Window),
    /// `_NET_ACTIVE_WINDOW` (A1) or a new toplevel (M2).
    ActivationRequested(x::Window),
    OverlayGone(OutputId),
    OutputChanged(x::Window, OutputId),
    BatchEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `focus_window`: SetInputFocus, `_NET_ACTIVE_WINDOW`, WM_STATE, RandR primary.
    SetInput,
    /// WM_TAKE_FOCUS only.
    TakeFocus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XFocusChange {
    None,
    Window {
        window: x::Window,
        method: Method,
        primary_output: Option<OutputId>,
    },
}

/// The focus machine's outputs: `XFocus` only at `BatchEnd`, the rest when they happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    XFocus(XFocusChange),
    /// The Xwayland surface that holds the forwarded keyboard focus while the compositor's
    /// focus is on an overlay (§2.0 routing invariant); `None`: nothing.
    KeyboardRoute(Option<x::Window>),
    /// Request an xdg-activation token for `window` from the compositor-focus surface.
    ActivationToken {
        window: x::Window,
        surface: KbTarget,
    },
}

/// The focus machine's state (spec §1.2). Fields are public for tests and for
/// `derive_context`; only `FocusState::handle` changes them.
#[derive(Debug, Clone, Default)]
pub struct FocusState {
    pub compositor: Compositor,
    pub panel: Option<x::Window>,
    pub remembered: Option<x::Window>,
    /// What X was last told.
    pub applied: Option<XFocusChange>,
    pub last_toplevel: Option<x::Window>,
    pub pending_press: Option<(x::Window, OutputId)>,
    pub pending_activation: Option<x::Window>,
    /// The X focus decision; `None` is X focus None.
    pub desired: Option<x::Window>,
    /// Some rule assigned `desired` during the current batch.
    pub assigned: bool,
    /// A KeyboardLeave with no KeyboardEnter after it in the current batch.
    pub leave_pending: bool,
    /// The last `KeyboardRoute` emitted.
    pub route: Option<x::Window>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceRef {
    X(x::Window),
    Overlay(OutputId),
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressRef {
    X(x::Window),
    Decoration(x::Window),
    Overlay(OutputId),
    Other,
}

/// A rectangle in X's coordinates (physical pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl XRect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        (self.x..self.x + self.width).contains(&x) && (self.y..self.y + self.height).contains(&y)
    }
}

/// What satellite sees, at the boundaries the old and new code share (spec §3.1 streams 1-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawEvent {
    /// MapNotify, after the window's properties are read.
    MapFacts(WindowFacts),
    /// WM_HINTS changed after map (recorded; the model ignores it).
    HintsChanged {
        window: x::Window,
        input: InputHint,
    },
    /// set_serial of a mapped window: the role is created now, with the X geometry now.
    RoleCreate {
        window: x::Window,
        dims: WindowDims,
    },
    Unmap {
        window: x::Window,
    },
    /// DestroyNotify, or reparented away from the root.
    Destroy {
        window: x::Window,
    },
    /// `_NET_ACTIVE_WINDOW` client message.
    ActiveWindowRequest {
        window: x::Window,
    },
    KeyboardEnter {
        target: SurfaceRef,
        serial: u32,
    },
    KeyboardLeave {
        target: SurfaceRef,
        serial: u32,
    },
    Key {
        pressed: bool,
        serial: u32,
    },
    /// A pointer button press (any button) or touch down.
    Press {
        target: PressRef,
        serial: u32,
        touch: bool,
    },
    /// The pointer entered an X window's surface.
    PointerEnter {
        window: x::Window,
    },
    PopupFirstConfigure {
        window: x::Window,
    },
    PopupDone {
        window: x::Window,
    },
    SurfaceEnterOutput {
        window: x::Window,
        output: OutputId,
    },
    /// An output appeared, or its X rect or mode changed.
    OutputGeometry {
        output: OutputId,
        x_rect: XRect,
        mode: (i32, i32),
    },
    OutputRemoved {
        output: OutputId,
    },
    /// The output's overlay got its first configure.
    OverlayMapped {
        output: OutputId,
    },
    OverlayClosed {
        output: OutputId,
    },
    /// An overlay window was made again on another output's overlay (its X position moved).
    OverlayRehome {
        window: x::Window,
        output: OutputId,
    },
    ActivationTokenDone {
        window: x::Window,
    },
    /// End of a `handle_clientside_events` run (also after every X event).
    BatchEnd,
}

/// What `translate` makes of a raw event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineInput {
    Classify { window: x::Window },
    Focus(Event),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowEntry {
    pub facts: WindowFacts,
    pub mapped: bool,
    /// Set at role creation, cleared at unmap.
    pub classification: Option<Classification>,
    /// The output its surface last entered.
    pub output: Option<OutputId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputInfo {
    pub x_rect: XRect,
    /// Mode size, unrotated (fullscreen heuristic).
    pub mode: (i32, i32),
    pub overlay_mapped: bool,
}

/// What the model knows of windows and outputs; kept by `RoleTable::apply` from raw events.
#[derive(Debug, Clone, Default)]
pub struct RoleTable {
    pub windows: HashMap<x::Window, WindowEntry>,
    pub outputs: BTreeMap<OutputId, OutputInfo>,
    /// The last toplevel the pointer entered (popups excluded, as today).
    pub last_hovered: Option<x::Window>,
}

impl RoleTable {
    pub fn entry(&self, w: x::Window) -> Option<&WindowEntry> {
        self.windows.get(&w)
    }

    pub fn is_mapped(&self, w: x::Window) -> bool {
        self.windows.get(&w).is_some_and(|e| e.mapped)
    }

    pub fn classification(&self, w: x::Window) -> Option<Classification> {
        self.windows.get(&w).and_then(|e| e.classification)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    pub window: x::Window,
    pub client: u32,
    pub at: Millis,
}

/// The latest pointer press on an X surface (Q12).
#[derive(Debug, Clone, Default)]
pub struct PressLog {
    pub last: Option<Press>,
}

/// The whole pure model; `Model::feed` (T4b) is the one pipeline every driver uses.
#[derive(Debug, Clone, Default)]
pub struct Model {
    pub roles: RoleTable,
    pub focus: FocusState,
    pub presses: PressLog,
}

#[cfg(test)]
pub mod testkit {
    //! Builders shared by the model's tests.
    use super::*;
    use xcb::XidNew;

    pub const MASK: u32 = 0x1f_ffff;
    /// Client bases: window `A | n` belongs to client A, `B | n` to client B, `C | n` to C.
    pub const A: u32 = 0x0020_0000;
    pub const B: u32 = 0x0040_0000;
    pub const C: u32 = 0x0060_0000;
    pub const O1: OutputId = OutputId(1);
    pub const O2: OutputId = OutputId(2);

    pub fn win(id: u32) -> x::Window {
        x::Window::new(id)
    }

    pub fn dims(x: i16, y: i16, width: u16, height: u16) -> WindowDims {
        WindowDims {
            x,
            y,
            width,
            height,
        }
    }

    /// A window with no properties: upstream guesses Toplevel.
    pub fn facts(id: u32) -> WindowFacts {
        WindowFacts {
            window: win(id),
            override_redirect: false,
            types: Vec::new(),
            motif: MotifHints::default(),
            class: None,
            size_hints: None,
            input: InputHint::Absent,
            keep_above: false,
            transient_for: None,
            take_focus: false,
            delete: false,
            dims: dims(0, 0, 100, 100),
            client: id & !MASK,
            guessed: WindowRole::Toplevel,
        }
    }

    impl WindowFacts {
        pub fn types(mut self, types: &[NetWmType]) -> Self {
            self.types = types.to_vec();
            self
        }

        /// Override-redirect; upstream guesses Popup for these.
        pub fn or(mut self) -> Self {
            self.override_redirect = true;
            self.guessed = WindowRole::Popup;
            self
        }

        pub fn transient(mut self, parent: u32) -> Self {
            self.transient_for = Some(win(parent));
            self
        }

        pub fn input(mut self, input: InputHint) -> Self {
            self.input = input;
            self
        }

        pub fn take_focus(mut self) -> Self {
            self.take_focus = true;
            self
        }

        pub fn keep_above(mut self) -> Self {
            self.keep_above = true;
            self
        }

        pub fn no_decor(mut self) -> Self {
            self.motif.decorations = Some(0);
            self
        }

        pub fn functions_none(mut self) -> Self {
            self.motif.functions = Some(0);
            self
        }

        pub fn position(mut self) -> Self {
            self.size_hints
                .get_or_insert_with(SizeHints::default)
                .position = true;
            self
        }

        pub fn min(mut self, width: i32, height: i32) -> Self {
            self.size_hints.get_or_insert_with(SizeHints::default).min = Some((width, height));
            self
        }

        pub fn max(mut self, width: i32, height: i32) -> Self {
            self.size_hints.get_or_insert_with(SizeHints::default).max = Some((width, height));
            self
        }

        pub fn class(mut self, class: &str) -> Self {
            self.class = Some(class.to_string());
            self
        }

        pub fn at(mut self, x: i16, y: i16, width: u16, height: u16) -> Self {
            self.dims = dims(x, y, width, height);
            self
        }

        pub fn guessed(mut self, role: WindowRole) -> Self {
            self.guessed = role;
            self
        }
    }

    /// A 3840x2160 output at X `(x, 0)` whose overlay is mapped.
    pub fn output(roles: &mut RoleTable, id: OutputId, x: i32) {
        roles.outputs.insert(
            id,
            OutputInfo {
                x_rect: XRect {
                    x,
                    y: 0,
                    width: 3840,
                    height: 2160,
                },
                mode: (3840, 2160),
                overlay_mapped: true,
            },
        );
    }

    /// Inserts a mapped window classified as `role` (focus_on_map None; kind from the role).
    pub fn add(roles: &mut RoleTable, facts: WindowFacts, role: Role) -> x::Window {
        let kind = match role {
            Role::Popup { .. } | Role::OverlayPopupOf { .. } => XKind::Popup,
            Role::PanelOf { .. } | Role::OverlayWindow { .. } => XKind::Notification,
            Role::Toplevel { .. } | Role::FullscreenToplevel { .. } => XKind::Toplevel,
        };
        let window = facts.window;
        roles.windows.insert(
            window,
            WindowEntry {
                facts,
                mapped: true,
                classification: Some(Classification {
                    kind,
                    role,
                    focus_on_map: FocusOnMap::None,
                }),
                output: None,
            },
        );
        window
    }

    /// Inserts a mapped window whose role is not created yet.
    pub fn add_unclassified(roles: &mut RoleTable, facts: WindowFacts) -> x::Window {
        let window = facts.window;
        roles.windows.insert(
            window,
            WindowEntry {
                facts,
                mapped: true,
                classification: None,
                output: None,
            },
        );
        window
    }

    pub const TOPLEVEL: Role = Role::Toplevel {
        parent: None,
        fixed_size: false,
    };
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    #[test]
    fn hint_helpers() {
        assert!(
            MotifHints {
                functions: Some(0),
                decorations: None
            }
            .functions_empty()
        );
        assert!(!MotifHints::default().functions_empty());
        assert!(
            MotifHints {
                functions: None,
                decorations: Some(0)
            }
            .no_decorations()
        );
        assert!(
            !MotifHints {
                functions: None,
                decorations: Some(1)
            }
            .no_decorations()
        );
        let hints = |min, max| SizeHints {
            min,
            max,
            position: false,
        };
        assert!(hints(Some((10, 10)), Some((10, 10))).forced_size());
        assert!(!hints(Some((10, 10)), None).forced_size());
        assert!(!hints(Some((10, 10)), Some((10, 11))).forced_size());
    }

    #[test]
    fn role_predicates_and_links() {
        let w = win(A | 1);
        assert!(TOPLEVEL.is_toplevel());
        assert!(
            Role::FullscreenToplevel {
                parent: None,
                fixed_size: false
            }
            .is_toplevel()
        );
        assert!(Role::PanelOf { parent: w }.is_panel());
        assert!(
            Role::OverlayWindow {
                output: O1,
                panel: true
            }
            .is_panel()
        );
        assert!(
            !Role::OverlayWindow {
                output: O1,
                panel: false
            }
            .is_panel()
        );
        assert!(Role::OverlayPopupOf { panel: w }.is_popup());
        assert_eq!(Role::Popup { parent: w }.link(), Some(w));
        assert_eq!(Role::PanelOf { parent: w }.link(), Some(w));
        assert_eq!(Role::OverlayPopupOf { panel: w }.link(), Some(w));
        assert_eq!(TOPLEVEL.link(), None);
        assert!(XKind::NoInputHelper.is_popup() && !XKind::CallBar.is_popup());
        assert!(XKind::CallBar.is_fixed_size() && !XKind::Toplevel.is_fixed_size());
    }

    #[test]
    fn rect_and_table_getters() {
        let r = XRect {
            x: 3840,
            y: 0,
            width: 3840,
            height: 2160,
        };
        assert!(r.contains(3840, 0) && !r.contains(7680, 0) && !r.contains(3839, 10));
        let mut roles = RoleTable::default();
        let t = add(&mut roles, facts(A | 1), TOPLEVEL);
        assert!(roles.is_mapped(t));
        assert_eq!(roles.classification(t).map(|c| c.role), Some(TOPLEVEL));
        assert!(!roles.is_mapped(win(A | 9)));
        assert_eq!(facts(A | 1).client, A);
        assert_eq!(facts(B | 7).client, B);
    }
}
