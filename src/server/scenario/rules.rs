//! One fixture per focus rule (design §2.1 rules 1-14).

use super::*;
use crate::server::model::testkit::{A, B, O1, O2, win};
use crate::server::model::{InputHint, KbTarget, RawEvent, Role, SurfaceRef};

const MAIN: u32 = A | 1;
const MEETING: u32 = A | 2;
const PANEL: u32 = A | 3;
const BAR: u32 = A | 4;
const MENU: u32 = A | 5;
const PANEL2: u32 = A | 6;
const CAND: u32 = 0x0060_0001;
const OTHER_MENU: u32 = B | 5;

/// Rule 1: X focus follows the compositor's; a focused popup of the entered window keeps it
/// (deliberate change, design §2.2); a press in the parent takes it back (rule 5).
#[test]
fn rule01_follow_and_the_popup_exception() {
    let mut s = Scenario::new("design §2.1 rule 1, §2.2");
    s.output(O1, 0).focused_toplevel(MAIN).hover(win(MAIN));
    s.map(menu(MENU))
        .expect(&[xf(win(MENU))])
        .expect_role(win(MENU), Role::Popup { parent: win(MAIN) });
    s.leave(x(MAIN)).enter(x(MAIN)).batch().expect(&[]);
    s.press(on(MAIN)).batch().expect(&[xf(win(MAIN))]);
}

/// Rule 2: a Leave with nothing after it in the batch is focus going to another client.
#[test]
fn rule02_focus_to_another_client() {
    let mut s = Scenario::new("design §2.1 rule 2");
    s.output(O1, 0).focused_toplevel(MEETING);
    s.map(panel_for(PANEL, MEETING))
        .expect(&[xf(win(PANEL))])
        .expect_panel(Some(win(PANEL)));
    s.leave(x(MEETING))
        .batch()
        .expect(&[xf_none()])
        .expect_panel(None);
}

/// Rule 3: a panel takes focus as it shows; the window the compositor is on is where focus
/// goes back; a panel replacing another while the compositor is on the overlay keeps that.
#[test]
fn rule03_panel_opens_and_replaces() {
    let mut s = Scenario::new("design §2.1 rule 3");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(bar(BAR)).expect(&[]).expect_role(
        win(BAR),
        Role::OverlayWindow {
            output: O1,
            panel: false,
        },
    );
    s.map(panel_for(PANEL, BAR))
        .expect(&[xf_on(win(PANEL), O1)])
        .expect_role(
            win(PANEL),
            Role::OverlayWindow {
                output: O1,
                panel: true,
            },
        );
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(PANEL))]);
    s.map(panel_for(PANEL2, BAR))
        .expect(&[route(win(PANEL2)), xf_on(win(PANEL2), O1)])
        .expect_panel(Some(win(PANEL2)));
    s.unmap(win(PANEL2))
        .expect(&[route(win(MAIN)), xf(win(MAIN))])
        .expect_panel(None);
}

/// Rule 4: hover never steals from the panel, onto an X window or onto the overlay.
#[test]
fn rule04_hover_never_steals_from_the_panel() {
    let mut s = Scenario::new("design §2.1 rule 4");
    s.output(O1, 0)
        .focused_toplevel(MAIN)
        .focused_toplevel(MEETING);
    s.map(panel_for(PANEL, MEETING)).expect(&[xf(win(PANEL))]);
    s.leave(x(MEETING)).enter(x(MAIN)).batch().expect(&[]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(PANEL))]);
    s.key(true).key(false).expect(&[]);
}

/// Rule 5: a press outside the panel's family focuses the pressed window.
#[test]
fn rule05_press_outside_the_panel() {
    let mut s = Scenario::new("design §2.1 rule 5");
    s.output(O1, 0)
        .focused_toplevel(MAIN)
        .focused_toplevel(MEETING);
    s.map(panel_for(PANEL, MEETING)).expect(&[xf(win(PANEL))]);
    s.leave(x(MEETING)).enter(x(MAIN)).batch().expect(&[]);
    s.press(on(MAIN))
        .batch()
        .expect(&[xf(win(MAIN))])
        .expect_panel(None);
}

/// Rule 5 (r6 fixture): a press in a PanelOf panel still mapped after the slot was cleared
/// makes it the panel again.
#[test]
fn rule05_pressed_panel_becomes_the_panel_again() {
    let mut s = Scenario::new("design §2.1 rule 5 (Q1 of round 4), §3.2 r6 fixtures");
    s.output(O1, 0)
        .focused_toplevel(MAIN)
        .focused_toplevel(MEETING);
    s.map(panel_for(PANEL, MEETING)).expect(&[xf(win(PANEL))]);
    s.leave(x(MEETING)).enter(x(MAIN)).batch().expect(&[]);
    s.press(on(MAIN)).batch().expect(&[xf(win(MAIN))]);
    s.press(on(PANEL))
        .batch()
        .expect(&[xf(win(PANEL))])
        .expect_panel(Some(win(PANEL)));
}

/// Rule 6: a press inside the panel's family changes nothing.
#[test]
fn rule06_press_inside_the_family() {
    let mut s = Scenario::new("design §2.1 rule 6");
    s.output(O1, 0).focused_toplevel(MEETING);
    s.map(panel_for(PANEL, MEETING)).expect(&[xf(win(PANEL))]);
    s.map(candidates(CAND))
        .expect(&[])
        .expect_role(win(CAND), Role::OverlayPopupOf { panel: win(PANEL) });
    s.press(on(PANEL)).batch().expect(&[]);
    s.press(on(CAND))
        .batch()
        .expect(&[])
        .expect_panel(Some(win(PANEL)));
}

/// Rule 7: a press on an overlay window waits for the compositor's focus on that overlay,
/// across batches and a Leave; it is one-shot; on an overlay already focused it acts at once.
#[test]
fn rule07_press_on_an_overlay_window() {
    let mut s = Scenario::new("design §2.1 rule 7, §2.2 one-shot press");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(bar(BAR)).expect(&[]);
    s.press(on(BAR)).batch().expect(&[]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .expect(&[route(win(BAR))])
        .batch()
        .expect(&[xf_on(win(BAR), O1)]);
    s.leave(SurfaceRef::Overlay(O1))
        .enter(x(MAIN))
        .batch()
        .expect(&[unroute(), xf(win(MAIN))]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(MAIN))]);
    s.press(on(BAR))
        .expect(&[route(win(BAR))])
        .batch()
        .expect(&[xf_on(win(BAR), O1)]);
}

/// Rule 7 (deliberate change): with a panel open, a press on a bar only clicks the bar.
#[test]
fn rule07_bar_under_a_panel() {
    let mut s = Scenario::new("design §2.1 rule 7, §2.2");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(bar(BAR)).expect(&[]);
    s.map(panel_for(PANEL, BAR))
        .expect(&[xf_on(win(PANEL), O1)]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(PANEL))]);
    s.press(on(BAR))
        .batch()
        .expect(&[])
        .expect_panel(Some(win(PANEL)));
}

/// Rule 8 (deliberate change): the overlay focused without a press keeps X focus, and keys
/// go to the X-focused window.
#[test]
fn rule08_overlay_hover_keeps_x_focus() {
    let mut s = Scenario::new("design §2.1 rule 8, §2.2");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(MAIN))]);
}

/// Rule 9 / race "popup closing with X focus on it": focus goes to the window the
/// compositor is on.
#[test]
fn rule09_focused_popup_closes() {
    let mut s = Scenario::new("design §2.1 rule 9, §3.2 races");
    s.output(O1, 0).focused_toplevel(MAIN).hover(win(MAIN));
    s.map(menu(MENU)).expect(&[xf(win(MENU))]);
    s.unmap(win(MENU)).expect(&[xf(win(MAIN))]);
}

/// Rule 10: the focused toplevel going away takes X focus to None and forgets it; the
/// compositor's focus on it is gone too, so a new toplevel gets no token.
#[test]
fn rule10_references() {
    let mut s = Scenario::new("design §2.1 rule 10");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.unmap(win(MAIN)).expect(&[xf_none()]);
    assert_eq!(s.model.focus.last_toplevel, None);
    s.map(toplevel(MEETING)).expect(&[]);
}

/// Rule 11 (upstream #494): override-redirect popups never take focus; WM_TAKE_FOCUS ones
/// are offered it; input-true ones get it.
#[test]
fn rule11_popups_at_first_configure() {
    let mut s = Scenario::new("design §2.1 rule 11");
    s.output(O1, 0).focused_toplevel(MAIN).hover(win(MAIN));
    s.map(facts(A | 10).or().at(10, 10, 50, 50)).expect(&[]);
    s.map(
        facts(A | 11)
            .guessed(WindowRole::Popup)
            .take_focus()
            .at(10, 10, 50, 50),
    )
    .expect(&[xf_take(win(A | 11))]);
    s.map(menu(MENU)).expect(&[xf(win(MENU))]);
}

/// Rule 11: under an open panel, its client's popups are its popups; other clients' are not.
#[test]
fn rule11_popups_under_an_open_panel() {
    let mut s = Scenario::new("design §2.1 rule 11, §2.2");
    s.output(O1, 0)
        .focused_toplevel(MEETING)
        .hover(win(MEETING));
    s.map(panel_for(PANEL, MEETING)).expect(&[xf(win(PANEL))]);
    s.map(menu(MENU))
        .expect(&[xf(win(MENU))])
        .expect_role(win(MENU), Role::Popup { parent: win(PANEL) });
    s.map(menu(OTHER_MENU))
        .expect(&[xf(win(OTHER_MENU))])
        .expect_role(
            win(OTHER_MENU),
            Role::Popup {
                parent: win(MEETING),
            },
        );
}

/// Rule 12 / race "keyboard-triggered activation": the key release before the
/// compositor's Enter does not drop the activation; it releases the panel.
#[test]
fn rule12_keyboard_triggered_activation() {
    let mut s = Scenario::new("design §2.1 rule 12, §3.2 races");
    s.output(O1, 0)
        .focused_toplevel(MAIN)
        .focused_toplevel(MEETING);
    s.map(panel_for(PANEL, MEETING)).expect(&[xf(win(PANEL))]);
    s.activate(win(MAIN))
        .expect(&[token(win(MAIN), KbTarget::X(win(MEETING)))]);
    s.key(false).batch().expect(&[]);
    s.leave(x(MEETING))
        .enter(x(MAIN))
        .batch()
        .expect(&[xf(win(MAIN))])
        .expect_panel(None);
}

/// Rule 12 / race "activation of an already-focused window": resolved at once.
#[test]
fn rule12_activation_of_the_window_the_compositor_is_on() {
    let mut s = Scenario::new("design §2.1 rule 12, §3.2 races");
    s.output(O1, 0)
        .focused_toplevel(MAIN)
        .focused_toplevel(MEETING);
    s.map(panel_for(PANEL, MEETING)).expect(&[xf(win(PANEL))]);
    s.leave(x(MEETING)).enter(x(MAIN)).batch().expect(&[]);
    s.activate(win(MAIN))
        .expect(&[token(win(MAIN), KbTarget::X(win(MAIN))), xf(win(MAIN))])
        .expect_panel(None);
}

/// Rule 12 / race "new toplevel while a panel is open" (deliberate change): it takes focus
/// from the panel when the compositor focuses it.
#[test]
fn rule12_new_toplevel_while_a_panel_is_open() {
    let mut s = Scenario::new("design §2.1 rule 12, §2.2, §3.2 races");
    s.output(O1, 0).focused_toplevel(MEETING);
    s.map(panel_for(PANEL, MEETING)).expect(&[xf(win(PANEL))]);
    s.map(toplevel(MAIN))
        .expect(&[token(win(MAIN), KbTarget::X(win(MEETING)))]);
    s.leave(x(MEETING))
        .enter(x(MAIN))
        .batch()
        .expect(&[xf(win(MAIN))])
        .expect_panel(None);
}

/// Rule 13: the primary output goes with the focused window, a popup's parent's output; the
/// focused toplevel moving output sets it again.
#[test]
fn rule13_primary_output() {
    let mut s = Scenario::new("design §2.1 rule 13, §2.2");
    s.output(O1, 0).output(O2, 3840).focused_toplevel(MAIN);
    s.raw(RawEvent::SurfaceEnterOutput {
        window: win(MAIN),
        output: O2,
    })
    .batch()
    .expect(&[xf_on(win(MAIN), O2)]);
    s.hover(win(MAIN))
        .map(menu(MENU))
        .expect(&[xf_on(win(MENU), O2)]);
}

/// Rule 14 / design §1.1-bis: the panel anchor table through the pipeline, with the r6
/// fixture "a panel transient for a bar whose role is not created yet".
#[test]
fn rule14_panel_placement() {
    const P1: u32 = A | 11;
    const P2: u32 = A | 12;
    const P3: u32 = A | 13;
    const P4: u32 = A | 14;
    const P5: u32 = A | 15;
    const P6: u32 = A | 16;
    const P7: u32 = A | 17;
    const FS: u32 = A | 18;
    const CALL: u32 = A | 19;
    let mut s = Scenario::new("design §1.1-bis anchor table, §3.2 r6 fixtures");
    s.output(O1, 0)
        .focused_toplevel(MEETING)
        .hover(win(MEETING));
    s.map(panel_for(P1, MEETING)).skip().expect_role(
        win(P1),
        Role::PanelOf {
            parent: win(MEETING),
        },
    );
    s.map(panel_for(P2, P1)).skip().expect_role(
        win(P2),
        Role::PanelOf {
            parent: win(MEETING),
        },
    );
    s.map(menu(OTHER_MENU)).skip().expect_role(
        win(OTHER_MENU),
        Role::Popup {
            parent: win(MEETING),
        },
    );
    s.map(panel_for(P3, OTHER_MENU)).skip().expect_role(
        win(P3),
        Role::PanelOf {
            parent: win(MEETING),
        },
    );
    s.map(fullscreen_toplevel(FS)).skip().expect_role(
        win(FS),
        Role::FullscreenToplevel {
            parent: None,
            fixed_size: false,
        },
    );
    s.map(panel_for(P4, FS))
        .skip()
        .expect_role(win(P4), Role::PanelOf { parent: win(FS) });
    s.map(panel_for(P5, A | 60)).skip().expect_role(
        win(P5),
        Role::OverlayWindow {
            output: O1,
            panel: false,
        },
    );
    s.facts_only(bar(BAR))
        .map(panel_for(P6, BAR))
        .skip()
        .expect_role(
            win(P6),
            Role::OverlayWindow {
                output: O1,
                panel: true,
            },
        );
    s.map(panel_for(P7, MEETING).input(InputHint::False))
        .skip()
        .expect_role(
            win(P7),
            Role::OverlayWindow {
                output: O1,
                panel: false,
            },
        );
    s.map(call_bar(CALL)).skip().expect_role(
        win(CALL),
        Role::OverlayWindow {
            output: O1,
            panel: false,
        },
    );
}
