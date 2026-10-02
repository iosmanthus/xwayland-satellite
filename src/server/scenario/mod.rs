//! Scenario fixtures (docs/window-model/fixtures.md): raw events scripted through the
//! model's one pipeline, `Model::feed`, with the outputs of each step checked.

use super::model::testkit::{facts, win};
use super::model::{
    Compositor, InputHint, KbTarget, Method, Millis, Model, NetWmType, Output, OutputId, PressRef,
    RawEvent, Role, SurfaceRef, WindowFacts, XFocusChange, XRect,
};
use crate::xstate::WindowRole;
use xcb::x;

mod apps;
mod races;
mod rules;

pub struct Scenario {
    pub model: Model,
    now: Millis,
    serial: u32,
    outputs: Vec<Output>,
    source: &'static str,
}

impl Scenario {
    /// `source` cites what the fixture pins (a log excerpt, a design section).
    pub fn new(source: &'static str) -> Self {
        Self {
            model: Model::default(),
            now: 1_000,
            serial: 0,
            outputs: Vec::new(),
            source,
        }
    }

    pub fn raw(&mut self, raw: RawEvent) -> &mut Self {
        let out = self.model.feed(&raw, self.now);
        self.outputs.extend(out);
        self
    }

    pub fn advance(&mut self, ms: Millis) -> &mut Self {
        self.now += ms;
        self
    }

    pub fn batch(&mut self) -> &mut Self {
        self.raw(RawEvent::BatchEnd)
    }

    fn next_serial(&mut self) -> u32 {
        self.serial += 1;
        self.serial
    }

    /// A 3840x2160 output at X `(x, 0)` with its overlay mapped.
    pub fn output(&mut self, output: OutputId, x: i32) -> &mut Self {
        self.raw(RawEvent::OutputGeometry {
            output,
            x_rect: XRect {
                x,
                y: 0,
                width: 3840,
                height: 2160,
            },
            mode: (3840, 2160),
        })
        .raw(RawEvent::OverlayMapped { output })
        .batch()
    }

    /// MapNotify only: X has the window mapped, its role is not made yet.
    pub fn facts_only(&mut self, f: WindowFacts) -> &mut Self {
        self.raw(RawEvent::MapFacts(f))
    }

    pub fn map_unconfigured(&mut self, f: WindowFacts) -> &mut Self {
        let (window, dims) = (f.window, f.dims);
        self.raw(RawEvent::MapFacts(f))
            .raw(RawEvent::RoleCreate { window, dims })
            .batch()
    }

    pub fn configure(&mut self, window: x::Window) -> &mut Self {
        self.raw(RawEvent::PopupFirstConfigure { window }).batch()
    }

    /// Maps the window, and gives it its first configure if it is not a toplevel.
    pub fn map(&mut self, f: WindowFacts) -> &mut Self {
        let window = f.window;
        self.map_unconfigured(f);
        if self
            .model
            .roles
            .classification(window)
            .is_some_and(|c| !c.role.is_toplevel())
        {
            self.configure(window);
        }
        self
    }

    pub fn enter(&mut self, target: SurfaceRef) -> &mut Self {
        let serial = self.next_serial();
        self.raw(RawEvent::KeyboardEnter { target, serial })
    }

    pub fn leave(&mut self, target: SurfaceRef) -> &mut Self {
        let serial = self.next_serial();
        self.raw(RawEvent::KeyboardLeave { target, serial })
    }

    pub fn key(&mut self, pressed: bool) -> &mut Self {
        let serial = self.next_serial();
        self.raw(RawEvent::Key { pressed, serial })
    }

    pub fn press(&mut self, target: PressRef) -> &mut Self {
        let serial = self.next_serial();
        self.raw(RawEvent::Press {
            target,
            serial,
            touch: false,
        })
    }

    pub fn hover(&mut self, window: x::Window) -> &mut Self {
        self.raw(RawEvent::PointerEnter { window })
    }

    pub fn unmap(&mut self, window: x::Window) -> &mut Self {
        self.raw(RawEvent::Unmap { window }).batch()
    }

    pub fn destroy(&mut self, window: x::Window) -> &mut Self {
        self.raw(RawEvent::Destroy { window }).batch()
    }

    pub fn activate(&mut self, window: x::Window) -> &mut Self {
        self.raw(RawEvent::ActiveWindowRequest { window }).batch()
    }

    /// Maps toplevel `id` and moves the compositor's focus onto it, as niri does after the
    /// activation satellite asks for; X focus must follow.
    #[track_caller]
    pub fn focused_toplevel(&mut self, id: u32) -> &mut Self {
        let w = win(id);
        let before = self.model.focus.compositor;
        self.map(toplevel(id));
        match before {
            Compositor::X(v) => {
                self.expect(&[token(w, KbTarget::X(v))]);
                self.leave(SurfaceRef::X(v));
            }
            Compositor::Other | Compositor::Nothing => {
                self.expect(&[]);
            }
            Compositor::Overlay(_) => panic!("focused_toplevel: leave the overlay first"),
        }
        self.enter(SurfaceRef::X(w)).batch().expect(&[xf(w)])
    }

    /// The outputs since the previous check are exactly `want`.
    #[track_caller]
    pub fn expect(&mut self, want: &[Output]) -> &mut Self {
        assert_eq!(self.outputs, want, "{}", self.source);
        self.outputs.clear();
        self
    }

    /// Forgets the outputs since the previous check, unchecked (fixtures about roles only).
    pub fn skip(&mut self) -> &mut Self {
        self.outputs.clear();
        self
    }

    #[track_caller]
    pub fn expect_role(&mut self, window: x::Window, role: Role) -> &mut Self {
        let got = self.model.roles.classification(window).map(|c| c.role);
        assert_eq!(got, Some(role), "{}", self.source);
        self
    }

    #[track_caller]
    pub fn expect_panel(&mut self, panel: Option<x::Window>) -> &mut Self {
        assert_eq!(self.model.focus.panel, panel, "{}", self.source);
        self
    }
}

pub fn x(id: u32) -> SurfaceRef {
    SurfaceRef::X(win(id))
}

pub fn on(id: u32) -> PressRef {
    PressRef::X(win(id))
}

fn focus(window: x::Window, method: Method, primary_output: Option<OutputId>) -> Output {
    Output::XFocus(XFocusChange::Window {
        window,
        method,
        primary_output,
    })
}

pub fn xf(window: x::Window) -> Output {
    focus(window, Method::SetInput, None)
}

pub fn xf_on(window: x::Window, output: OutputId) -> Output {
    focus(window, Method::SetInput, Some(output))
}

pub fn xf_take(window: x::Window) -> Output {
    focus(window, Method::TakeFocus, None)
}

pub fn xf_take_on(window: x::Window, output: OutputId) -> Output {
    focus(window, Method::TakeFocus, Some(output))
}

pub fn xf_none() -> Output {
    Output::XFocus(XFocusChange::None)
}

pub fn route(window: x::Window) -> Output {
    Output::KeyboardRoute(Some(window))
}

pub fn unroute() -> Output {
    Output::KeyboardRoute(None)
}

pub fn token(window: x::Window, surface: KbTarget) -> Output {
    Output::ActivationToken { window, surface }
}

// Facts of the windows the fixtures use (sizes and positions from the server tests and the
// observer logs; X pixels on a 3840x2160 output at 2x).

pub fn toplevel(id: u32) -> WindowFacts {
    facts(id).types(&[NetWmType::Normal]).at(0, 0, 1600, 1200)
}

pub fn fullscreen_toplevel(id: u32) -> WindowFacts {
    facts(id).types(&[NetWmType::Normal]).at(0, 0, 3840, 2160)
}

/// Feishu's meeting control bar (inventory §5).
pub fn bar(id: u32) -> WindowFacts {
    facts(id)
        .types(&[NetWmType::Notification])
        .class("Meeting")
        .no_decor()
        .min(562, 56)
        .at(1650, 2020, 538, 56)
}

/// Feishu's meeting panel, transient for `parent`, without WM_HINTS (inventory §5).
pub fn panel_for(id: u32, parent: u32) -> WindowFacts {
    facts(id)
        .types(&[NetWmType::Notification])
        .class("Meeting")
        .no_decor()
        .transient(parent)
        .at(1600, 1760, 904, 256)
}

/// Feishu's participant panel: no WM_TRANSIENT_FOR, opened by a click (inventory §5).
pub fn clicked_panel(id: u32) -> WindowFacts {
    facts(id)
        .types(&[NetWmType::Notification])
        .class("Meeting")
        .no_decor()
        .at(224, 20, 672, 1072)
}

pub fn toast(id: u32) -> WindowFacts {
    facts(id)
        .types(&[NetWmType::Notification])
        .at(1656, 96, 528, 144)
}

/// Feishu's incoming call bar (inventory §5, xstate test feishu_incoming_call_bar).
pub fn call_bar(id: u32) -> WindowFacts {
    facts(id)
        .types(&[NetWmType::Normal])
        .class("Meeting")
        .no_decor()
        .keep_above()
        .position()
        .min(736, 172)
        .at(3056, 48, 736, 172)
}

/// fcitx5's candidate window: override-redirect, another client.
pub fn candidates(id: u32) -> WindowFacts {
    facts(id).or().class("fcitx").at(1640, 1860, 180, 560)
}

/// A menu that takes input (WM_HINTS input true).
pub fn menu(id: u32) -> WindowFacts {
    facts(id)
        .types(&[NetWmType::PopupMenu])
        .guessed(WindowRole::Popup)
        .input(InputHint::True)
        .at(100, 100, 200, 300)
}

/// An override-redirect tooltip transient for `parent`.
pub fn tooltip(id: u32, parent: u32) -> WindowFacts {
    facts(id)
        .or()
        .types(&[NetWmType::Tooltip])
        .transient(parent)
        .at(1700, 1700, 120, 40)
}
