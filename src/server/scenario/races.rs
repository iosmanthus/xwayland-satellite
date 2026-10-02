//! One fixture per race of design §3.2 not already pinned by a rule fixture (the rest:
//! rule09_focused_popup_closes, rule12_* in rules.rs).

use super::*;
use crate::server::model::testkit::{A, O1, win};
use crate::server::model::{Compositor, RawEvent, Role, SurfaceRef};

const MAIN: u32 = A | 1;
const MEETING: u32 = A | 2;
const PANEL: u32 = A | 3;
const BAR: u32 = A | 4;
const PANEL2: u32 = A | 6;
const VIEW: u32 = A | 7;
const TOAST: u32 = A | 8;

/// Race: Leave and Enter in one batch is a move; the panel holds.
#[test]
fn race_leave_enter_in_one_batch() {
    let mut s = Scenario::new("design §3.2 races; inventory §2c");
    s.output(O1, 0)
        .focused_toplevel(MAIN)
        .focused_toplevel(MEETING);
    s.map(panel_for(PANEL, MEETING)).expect(&[xf(win(PANEL))]);
    s.leave(x(MEETING))
        .enter(x(MAIN))
        .batch()
        .expect(&[])
        .expect_panel(Some(win(PANEL)));
}

/// Race (deliberate change): Enter(A) then Leave(A) in one batch is focus leaving to
/// another client.
#[test]
fn race_enter_then_leave_in_one_batch() {
    let mut s = Scenario::new("design §3.2 races, §2.2 rule 2");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(toplevel(MEETING)).expect(&[token(
        win(MEETING),
        crate::server::model::KbTarget::X(win(MAIN)),
    )]);
    s.enter(x(MEETING))
        .leave(x(MEETING))
        .batch()
        .expect(&[xf_none()]);
}

/// Race: a press on the overlay the compositor already focused (niri sends no Enter):
/// another panel there becomes the panel at once.
#[test]
fn race_press_on_the_focused_overlay() {
    let mut s = Scenario::new("design §3.2 races, §2.1 rule 7");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(bar(BAR)).expect(&[]);
    s.map(panel_for(PANEL, BAR))
        .expect(&[xf_on(win(PANEL), O1)]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(PANEL))]);
    s.map(panel_for(PANEL2, BAR))
        .expect(&[route(win(PANEL2)), xf_on(win(PANEL2), O1)]);
    s.press(on(PANEL))
        .expect(&[route(win(PANEL))])
        .batch()
        .expect(&[xf_on(win(PANEL), O1)])
        .expect_panel(Some(win(PANEL)));
}

/// Race: press, then Leave and Enter(overlay) in a later batch.
#[test]
fn race_press_then_overlay_enter_across_batches() {
    let mut s = Scenario::new("design §3.2 races, §2 Pending state");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(bar(BAR)).expect(&[]);
    s.press(on(BAR)).batch().expect(&[]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(BAR)), xf_on(win(BAR), O1)]);
}

/// Race: a key release keeps a pending overlay press; a key press drops it.
#[test]
fn race_press_then_keys() {
    let mut s = Scenario::new("design §3.2 races, §2 Pending state");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(bar(BAR)).expect(&[]);
    s.press(on(BAR)).key(false).batch().expect(&[]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(BAR)), xf_on(win(BAR), O1)]);
    s.leave(SurfaceRef::Overlay(O1))
        .enter(x(MAIN))
        .batch()
        .expect(&[unroute(), xf(win(MAIN))]);
    s.press(on(BAR)).key(true).batch().expect(&[]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(MAIN))]);
}

/// Race: a pending overlay press is dropped by the compositor focusing an X window instead.
#[test]
fn race_press_then_enter_elsewhere() {
    let mut s = Scenario::new("design §3.2 races, §2 Pending state");
    s.output(O1, 0)
        .focused_toplevel(MAIN)
        .focused_toplevel(MEETING);
    s.map(bar(BAR)).expect(&[]);
    s.press(on(BAR))
        .leave(x(MEETING))
        .enter(x(MAIN))
        .batch()
        .expect(&[xf(win(MAIN))]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(MAIN))]);
}

/// Race: the overlay the compositor's focus is on goes away.
#[test]
fn race_overlay_removed_while_focused() {
    let mut s = Scenario::new("design §3.2 races, §2.1 rule 10");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(bar(BAR)).expect(&[]);
    s.map(panel_for(PANEL, BAR))
        .expect(&[xf_on(win(PANEL), O1)]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(PANEL))]);
    s.raw(RawEvent::OverlayClosed { output: O1 })
        .batch()
        .expect(&[unroute()]);
    assert_eq!(s.model.focus.compositor, Compositor::Nothing);
}

/// Race: a toast shown between a click and the panel it opened: both count as opened by the
/// click; the panel, shown later, replaces the toast (today's ab5fae0 outcome).
#[test]
fn race_toast_before_a_click_opened_panel() {
    let mut s = Scenario::new("design §1.1-bis (b), §3.2 races");
    s.output(O1, 0).focused_toplevel(VIEW);
    s.press(on(VIEW)).batch().expect(&[xf(win(VIEW))]);
    s.advance(300)
        .map(toast(TOAST))
        .expect(&[xf(win(TOAST))])
        .expect_role(win(TOAST), Role::PanelOf { parent: win(VIEW) });
    s.advance(500)
        .map(clicked_panel(PANEL))
        .expect(&[xf(win(PANEL))])
        .expect_role(win(PANEL), Role::PanelOf { parent: win(VIEW) })
        .expect_panel(Some(win(PANEL)));
}

/// Known hazard (design §1.1-bis, review N3): a toast within 1.5 s after the click that
/// opened a panel replaces the panel, and Feishu closes the panel. Documents today's outcome
/// until rollout step 2b decides a discriminator.
#[test]
fn race_toast_after_a_click_opened_panel() {
    let mut s = Scenario::new("design §1.1-bis known hazard, §3.2 races");
    s.output(O1, 0).focused_toplevel(VIEW);
    s.press(on(VIEW)).batch().expect(&[xf(win(VIEW))]);
    s.advance(300)
        .map(clicked_panel(PANEL))
        .expect(&[xf(win(PANEL))])
        .expect_panel(Some(win(PANEL)));
    s.advance(300)
        .map(toast(TOAST))
        .expect(&[xf(win(TOAST))])
        .expect_panel(Some(win(TOAST)));
}
