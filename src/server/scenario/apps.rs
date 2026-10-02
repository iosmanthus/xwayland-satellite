//! The role and focus scenarios of the real apps (inventory §5), from the observer logs.

use super::*;
use crate::server::model::testkit::{A, B, C, O1, O2, facts, win};
use crate::server::model::{InputHint, KbTarget, NetWmType, RawEvent, Role, SurfaceRef};

const MAIN: u32 = A | 1;
const MEETING: u32 = A | 2;
const BAR: u32 = A | 3;
const PANEL: u32 = A | 4;
const VIEW: u32 = A | 5;
const CONTACT: u32 = A | 6;
const CALL: u32 = A | 7;
const TOAST: u32 = A | 8;
const TIP: u32 = A | 9;
const PREVIEW: u32 = A | 10;
const CAND: u32 = C | 1;
const MOMENTS: u32 = B | 1;
const BUBBLE: u32 = B | 2;
const WECHAT_POPUP: u32 = B | 3;

/// Feishu's meeting control bars: NOTIFICATION, frameless, min size only, no transient:
/// on the overlay at their X position, no focus; dragged onto another output they are
/// re-made there, and a click on them then waits for that output's overlay.
/// Sources: tests/scenarios/sources/meeting-bars.log (obs/xwin.log:557-586); motif and
/// normal hints from inventory §5 and xstate test feishu_meeting_bar.
/// Find: grep -an " map .*class='Meeting' type=NOTIFICATION" obs/xwin.log | head
#[test]
fn feishu_meeting_bars() {
    let mut s = Scenario::new("meeting-bars.log (obs/xwin.log:557-586); inventory §5 row 1");
    s.output(O1, 0).output(O2, 3840).focused_toplevel(MEETING);
    s.map(bar(BAR).at(1222, 2076, 1372, 84))
        .expect(&[])
        .expect_role(
            win(BAR),
            Role::OverlayWindow {
                output: O1,
                panel: false,
            },
        );
    s.raw(RawEvent::OverlayRehome {
        window: win(BAR),
        output: O2,
    })
    .batch()
    .expect(&[])
    .expect_role(
        win(BAR),
        Role::OverlayWindow {
            output: O2,
            panel: false,
        },
    );
    s.press(on(BAR))
        .leave(x(MEETING))
        .enter(SurfaceRef::Overlay(O2))
        .expect(&[route(win(BAR))])
        .batch()
        .expect(&[xf_on(win(BAR), O2)]);
}

/// Feishu's meeting panels over the bar (danmaku input, participant list): NOTIFICATION,
/// transient for the bar, no WM_HINTS: focused as shown; a click gives the overlay the
/// keys; the pointer passing over the main window does not take focus; focus going to
/// another client does.
/// Sources: inventory §5 row 2 (no observed log line of a panel transient for a bar's own
/// window; the capture only has panels transient for the fullscreen meeting toplevel,
/// row 3's case).
#[test]
fn feishu_meeting_panel_over_the_bar() {
    let mut s = Scenario::new("inventory §5 row 2");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(bar(BAR)).expect(&[]);
    s.map(panel_for(PANEL, BAR))
        .expect(&[xf_on(win(PANEL), O1)])
        .expect_panel(Some(win(PANEL)));
    s.press(on(PANEL)).batch().expect(&[]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(PANEL))]);
    s.key(true).key(false).expect(&[]);
    s.leave(SurfaceRef::Overlay(O1))
        .enter(x(MAIN))
        .batch()
        .expect(&[unroute()]);
    s.leave(x(MAIN))
        .batch()
        .expect(&[xf_none()])
        .expect_panel(None);
}

/// The same panels while the meeting window is up: transient for the meeting toplevel, an
/// xdg_popup of it; held against focus-follows-mouse; a click in the main window takes focus.
/// Sources: inventory §5 row 3 (5d2c7f3); server test panel_keeps_focus_the_pointer_moves.
#[test]
fn feishu_panel_over_the_meeting_window() {
    let mut s = Scenario::new("inventory §5 row 3; tests.rs panel_keeps_focus_the_pointer_moves");
    s.output(O1, 0)
        .focused_toplevel(MAIN)
        .focused_toplevel(MEETING);
    s.map(panel_for(PANEL, MEETING))
        .expect(&[xf(win(PANEL))])
        .expect_role(
            win(PANEL),
            Role::PanelOf {
                parent: win(MEETING),
            },
        );
    s.leave(x(MEETING)).enter(x(MAIN)).batch().expect(&[]);
    s.press(on(MAIN))
        .batch()
        .expect(&[xf(win(MAIN))])
        .expect_panel(None);
}

/// Typing into the danmaku panel with fcitx5: the candidate window (override-redirect,
/// another client) goes over the panel and takes no focus; clicking a candidate leaves
/// focus and keys with the panel.
/// Sources: tests/scenarios/sources/fcitx-candidates.log (obs/xwin.log:6478-6522);
/// inventory §5 row 4.
/// Find: grep -an " map .*class='fcitx'" obs/xwin.log | head
#[test]
fn danmaku_typing_with_fcitx5_candidates() {
    let mut s = Scenario::new("fcitx-candidates.log (obs/xwin.log:6478-6522); inventory §5 row 4");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(bar(BAR)).expect(&[]);
    s.map(panel_for(PANEL, BAR))
        .expect(&[xf_on(win(PANEL), O1)]);
    s.leave(x(MAIN))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(PANEL))]);
    s.map(candidates(CAND))
        .expect(&[])
        .expect_role(win(CAND), Role::OverlayPopupOf { panel: win(PANEL) });
    s.press(on(CAND))
        .batch()
        .expect(&[])
        .expect_panel(Some(win(PANEL)));
    s.unmap(win(CAND)).expect(&[]);
}

/// Feishu's incoming call bar: NORMAL, frameless, keep-above, program position, min size
/// only, no transient: on the overlay where it asks, no focus, never a panel even right
/// after a click in Feishu.
/// Sources: tests/scenarios/sources/call-bar.log (obs/xwin.log:447-475, geometry 736x172);
/// ABOVE/position/motif from inventory §5 row 5 and xstate test feishu_incoming_call_bar.
/// Find: grep -an " map .*class='Meeting' type=NORMAL" obs/xwin.log | head
#[test]
fn feishu_incoming_call_bar() {
    let mut s = Scenario::new("call-bar.log (obs/xwin.log:447-475); inventory §5 row 5");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.press(on(MAIN)).batch().expect(&[xf(win(MAIN))]);
    s.advance(200)
        .map(call_bar(CALL))
        .expect(&[])
        .expect_role(
            win(CALL),
            Role::OverlayWindow {
                output: O1,
                panel: false,
            },
        )
        .expect_panel(None);
}

/// Feishu's screen-sharing preview: the same kind of window as the call bar.
/// Sources: inventory §5 row 6 (7073337).
#[test]
fn feishu_screen_sharing_preview() {
    let mut s = Scenario::new("inventory §5 row 6");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(call_bar(PREVIEW).at(3000, 1600, 640, 400))
        .expect(&[])
        .expect_role(
            win(PREVIEW),
            Role::OverlayWindow {
                output: O1,
                panel: false,
            },
        );
}

/// Feishu's participant view while sharing: like the call bar but of a fixed size: a
/// toplevel the compositor floats, which takes no overlay focus.
/// Sources: inventory §5 row 7 (815c828); xstate test feishu_incoming_call_bar.
#[test]
fn feishu_participant_view_while_sharing() {
    let mut s = Scenario::new("inventory §5 row 7");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(
        call_bar(VIEW)
            .min(320, 238)
            .max(320, 238)
            .at(3424, 80, 320, 238),
    )
    .expect(&[token(win(VIEW), KbTarget::X(win(MAIN)))])
    .expect_role(
        win(VIEW),
        Role::Toplevel {
            parent: None,
            fixed_size: false,
        },
    );
}

/// Feishu's participant panel, opened by a click in the participant view: no
/// WM_TRANSIENT_FOR, shown within 1.5 s: a panel, xdg_popup of the view, focused; closed,
/// focus goes back to the view.
/// Sources: tests/scenarios/sources/participant-panel.log (obs/xwin.log:3741-3786, 672x1072
/// at +224+20); inventory §5 row 8 (ab5fae0).
/// Find: grep -an " map .*type=NOTIFICATION .*geom=672x1072" obs/xwin.log | head
#[test]
fn feishu_participant_panel_opened_from_the_view() {
    let mut s = Scenario::new("participant-panel.log (obs/xwin.log:3741-3786); inventory §5 row 8");
    s.output(O1, 0).focused_toplevel(VIEW);
    s.press(on(VIEW)).batch().expect(&[xf(win(VIEW))]);
    s.advance(400)
        .map(clicked_panel(PANEL))
        .expect(&[xf(win(PANEL))])
        .expect_role(win(PANEL), Role::PanelOf { parent: win(VIEW) });
    s.unmap(win(PANEL)).expect(&[xf(win(VIEW))]);
}

/// A contact window Feishu shows from the participant panel: a click in the panel keeps
/// it; the new window, activated and focused by the compositor, takes focus from the panel
/// (rule 12); an existing window activated with _NET_ACTIVE_WINDOW does the same.
/// Sources: inventory §5 rows 8-9 (815c828: "the window Feishu then activated to show a
/// contact").
#[test]
fn feishu_contact_window_from_the_participant_panel() {
    let mut s = Scenario::new("inventory §5 rows 8-9");
    s.output(O1, 0).focused_toplevel(VIEW);
    s.press(on(VIEW)).batch().expect(&[xf(win(VIEW))]);
    s.advance(400)
        .map(clicked_panel(PANEL))
        .expect(&[xf(win(PANEL))]);
    s.press(on(PANEL)).batch().expect(&[]);
    s.map(toplevel(CONTACT))
        .expect(&[token(win(CONTACT), KbTarget::X(win(VIEW)))]);
    s.leave(x(VIEW))
        .enter(x(CONTACT))
        .batch()
        .expect(&[xf(win(CONTACT))])
        .expect_panel(None);
    // Back in the view, a new panel; then Feishu activates the contact window again.
    s.leave(x(CONTACT))
        .enter(x(VIEW))
        .batch()
        .expect(&[xf(win(VIEW))]);
    s.press(on(VIEW)).batch().expect(&[xf(win(VIEW))]);
    s.advance(400)
        .map(clicked_panel(A | 20))
        .expect(&[xf(win(A | 20))]);
    s.activate(win(CONTACT))
        .expect(&[token(win(CONTACT), KbTarget::X(win(VIEW)))]);
    s.leave(x(VIEW))
        .enter(x(CONTACT))
        .batch()
        .expect(&[xf(win(CONTACT))])
        .expect_panel(None);
}

/// Feishu's fullscreen meeting with its emoji panel: the tooltip (override-redirect,
/// transient for the panel) goes over the panel; focus-follows-mouse onto the overlay with
/// no click keeps X focus on the panel and gives it the keys; a click in the panel keeps
/// it.
/// Sources: tests/scenarios/sources/emoji-panel.log (obs/xwin.log:7150-7199 and
/// obs/niri-events.log:503-510 around 20:20:02); inventory §5 row 10 (9bb7dce).
/// Find: grep -an "^20:20:0" obs/xwin.log | head -40
#[test]
fn feishu_fullscreen_emoji_panel_and_tooltip() {
    let mut s = Scenario::new("emoji-panel.log (obs/xwin.log:7150-7199); inventory §5 row 10");
    s.output(O1, 0);
    s.map(fullscreen_toplevel(MEETING)).expect(&[]);
    s.enter(x(MEETING)).batch().expect(&[xf(win(MEETING))]);
    s.map(panel_for(PANEL, MEETING))
        .expect(&[xf(win(PANEL))])
        .expect_role(
            win(PANEL),
            Role::PanelOf {
                parent: win(MEETING),
            },
        );
    s.map(tooltip(TIP, PANEL))
        .expect(&[])
        .expect_role(win(TIP), Role::OverlayPopupOf { panel: win(PANEL) });
    s.leave(x(MEETING))
        .enter(SurfaceRef::Overlay(O1))
        .batch()
        .expect(&[route(win(PANEL))]);
    s.press(on(PANEL))
        .batch()
        .expect(&[])
        .expect_panel(Some(win(PANEL)));
}

/// WeChat's Moments like/comment bubble: UTILITY+NORMAL, transient, frameless, input
/// false: a popup of the hovered window, never focused; a click on it leaves focus where
/// the compositor has it; it is destroyed (not reused) once dismissed.
/// Sources: tests/scenarios/sources/wechat-bubble.log (obs/xwin.log:9185-9209, e.g. the
/// `class='wechat' type=UTILITY,_KDEOVERRIDE,NORMAL … input=False` map, and the unmap+
/// destroy that follows it); inventory §5 row 14.
/// Find: grep -an " map .*class='wechat' type=UTILITY.*input=False" obs/xwin.log | head
#[test]
fn wechat_moments_bubble() {
    let mut s = Scenario::new("wechat-bubble.log (obs/xwin.log:9185-9209); inventory §5 row 14");
    s.output(O1, 0)
        .focused_toplevel(MOMENTS)
        .hover(win(MOMENTS));
    let bubble = facts(BUBBLE)
        .types(&[NetWmType::Utility, NetWmType::Other, NetWmType::Normal])
        .transient(MOMENTS)
        .no_decor()
        .min(362, 72)
        .input(InputHint::False)
        .class("wechat")
        .at(632, 1540, 272, 272);
    s.map(bubble).expect(&[]).expect_role(
        win(BUBBLE),
        Role::Popup {
            parent: win(MOMENTS),
        },
    );
    s.press(on(BUBBLE)).batch().expect(&[xf(win(MOMENTS))]);
    s.unmap(win(BUBBLE)).destroy(win(BUBBLE)).expect(&[]);
}

/// WeChat popup (upstream #277): UTILITY+NORMAL, override-redirect, transient, min = max:
/// a popup, never focused.
/// Sources: inventory §5 row 15; xstate test wechat_popup.
#[test]
fn wechat_popup_277() {
    let mut s = Scenario::new("inventory §5 row 15");
    s.output(O1, 0)
        .focused_toplevel(MOMENTS)
        .hover(win(MOMENTS));
    let popup = facts(WECHAT_POPUP)
        .or()
        .types(&[NetWmType::Utility, NetWmType::Normal])
        .transient(MOMENTS)
        .min(200, 100)
        .max(200, 100)
        .at(300, 300, 200, 100);
    s.map(popup).expect(&[]).expect_role(
        win(WECHAT_POPUP),
        Role::Popup {
            parent: win(MOMENTS),
        },
    );
}

/// Upstream #494 (Steam's dropdowns): override-redirect popups are never focused nor
/// offered focus; a popup advertising WM_TAKE_FOCUS is offered it, and X focus stays.
/// Sources: inventory §5 row 16 (add2795); server tests popup_*.
#[test]
fn upstream_494_popups() {
    let mut s = Scenario::new("inventory §5 row 16");
    s.output(O1, 0).focused_toplevel(MAIN).hover(win(MAIN));
    s.map(
        facts(A | 30)
            .or()
            .input(InputHint::True)
            .take_focus()
            .at(10, 20, 100, 50),
    )
    .expect(&[]);
    s.map(
        facts(A | 31)
            .guessed(WindowRole::Popup)
            .take_focus()
            .at(10, 20, 100, 50),
    )
    .expect(&[xf_take(win(A | 31))]);
}

/// Unity editor's add-component search (#397): a popup that needs keys (input true) is
/// focused at its first configure; a click back in the editor takes focus back.
/// Sources: inventory §5 row 17 (3273a0f, 5d1efbc).
#[test]
fn unity_add_component_search_397() {
    let mut s = Scenario::new("inventory §5 row 17");
    s.output(O1, 0).focused_toplevel(MAIN).hover(win(MAIN));
    s.map(menu(A | 32)).expect(&[xf(win(A | 32))]);
    s.key(true).key(false).expect(&[]);
    s.leave(x(MAIN)).enter(x(MAIN)).batch().expect(&[]);
    s.press(on(MAIN)).batch().expect(&[xf(win(MAIN))]);
}

/// Toasts: NOTIFICATION, no transient, no click before them: on the overlay, no focus;
/// without a mapped overlay, a fixed-size toplevel.
/// Sources: tests/scenarios/sources/toasts.log (obs/xwin.log:626-649, 528x144 at +1656+96);
/// inventory §5 row 18.
/// Find: grep -an " map .*type=NOTIFICATION .*geom=528x144" obs/xwin.log | head
#[test]
fn toasts() {
    let mut s = Scenario::new("toasts.log (obs/xwin.log:626-649); inventory §5 row 18");
    s.output(O1, 0).focused_toplevel(MAIN);
    s.map(toast(TOAST)).expect(&[]).expect_role(
        win(TOAST),
        Role::OverlayWindow {
            output: O1,
            panel: false,
        },
    );

    let mut s = Scenario::new("toasts.log (obs/xwin.log:626-649); inventory §5 row 18");
    s.raw(RawEvent::OutputGeometry {
        output: O1,
        x_rect: crate::server::model::XRect {
            x: 0,
            y: 0,
            width: 3840,
            height: 2160,
        },
        mode: (3840, 2160),
    })
    .batch();
    s.focused_toplevel(MAIN);
    s.map(toast(TOAST))
        .expect(&[token(win(TOAST), KbTarget::X(win(MAIN)))])
        .expect_role(
            win(TOAST),
            Role::Toplevel {
                parent: None,
                fixed_size: true,
            },
        );
}
