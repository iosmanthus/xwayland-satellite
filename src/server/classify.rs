//! Classification of a window at role creation (design §1.1, §1.1-bis): upstream's guess,
//! the fork's own arms re-walked over the window types, then where the window goes and
//! what it does to focus when shown.

// Wired into the event layer in T5; T5c removes this.
#![allow(dead_code)]

use super::model::{
    Classification, ClassifyContext, FocusOnMap, InputHint, NetWmType, ParentRole, Role,
    WindowFacts, XKind,
};
use crate::xstate::{POPUP_WM_CLASSES, WindowRole};
use xcb::x;

/// The X-side layer: upstream's guess, unless one of the fork's arms fires. Those sat
/// inside upstream's walk of the window types, which returns at the first type it
/// recognises; so they are decided by the first type upstream recognises or NOTIFICATION.
pub fn x_kind(facts: &WindowFacts) -> XKind {
    let guessed = match facts.guessed {
        WindowRole::Popup => XKind::Popup,
        WindowRole::Splash => XKind::Splash,
        // Toplevel, and the fork's own `Notification` guess, which the server still makes
        // until T5a restores upstream's guess (and which old traces record): whenever the
        // fork guessed Notification, A4 or A9 below fires on the same facts, so the guess
        // itself is never used for it.
        _ => XKind::Toplevel,
    };
    // Upstream decides these before it looks at the types.
    let pre = facts.override_redirect
        || facts.motif.functions_empty()
        || facts
            .class
            .as_deref()
            .is_some_and(|class| POPUP_WM_CLASSES.contains(&class));
    if pre {
        return guessed;
    }
    let forced_size = facts.size_hints.is_some_and(|h| h.forced_size());
    let positioned = facts.size_hints.is_some_and(|h| h.position);
    match first_type(facts) {
        // Toasts and a call's bars and panels, which their client sizes and places.
        Some(NetWmType::Notification) => XKind::Notification,
        // A frameless window asking to stay above the others, at a place of its choosing,
        // for no window in particular: a prompt (Feishu's incoming call bar). Not one of a
        // fixed size: a tile the user keeps and moves (its participant view).
        Some(NetWmType::Normal)
            if facts.keep_above
                && positioned
                && facts.motif.no_decorations()
                && !forced_size
                && facts.transient_for.is_none() =>
        {
            XKind::CallBar
        }
        // A frameless helper for another window that takes no input (WeChat's Moments
        // bubble): as a toplevel it took activation from that window, which closed it.
        Some(NetWmType::Utility)
            if facts.transient_for.is_some()
                && facts.motif.no_decorations()
                && facts.input == InputHint::False =>
        {
            XKind::NoInputHelper
        }
        _ => guessed,
    }
}

/// The first window type upstream's walk would stop at, or NOTIFICATION if that comes
/// first; the list defaults as upstream defaults it.
fn first_type(facts: &WindowFacts) -> Option<NetWmType> {
    if facts.types.is_empty() {
        return Some(if facts.transient_for.is_some() {
            NetWmType::Dialog
        } else {
            NetWmType::Normal
        });
    }
    facts.types.iter().copied().find(|t| *t != NetWmType::Other)
}

/// Where `facts`' window goes and what it does to focus when shown.
pub fn classify(facts: &WindowFacts, ctx: &ClassifyContext) -> Classification {
    let kind = x_kind(facts);
    let size = (i32::from(facts.dims.width), i32::from(facts.dims.height));
    if ctx.output_modes.contains(&size) {
        let parent = if kind.is_popup() {
            None
        } else {
            direct_toplevel(ctx)
        };
        return Classification {
            kind,
            role: Role::FullscreenToplevel {
                parent,
                fixed_size: kind.is_fixed_size(),
            },
            focus_on_map: FocusOnMap::None,
        };
    }
    match kind {
        XKind::Notification | XKind::CallBar => notification(facts, ctx, kind),
        XKind::Popup | XKind::NoInputHelper => popup(facts, ctx, kind),
        XKind::Toplevel | XKind::Splash => Classification {
            kind,
            role: Role::Toplevel {
                parent: direct_toplevel(ctx),
                fixed_size: kind == XKind::Splash,
            },
            focus_on_map: FocusOnMap::None,
        },
    }
}

/// The transient parent, if it is a toplevel itself (`xdg_toplevel.set_parent`).
fn direct_toplevel(ctx: &ClassifyContext) -> Option<x::Window> {
    match ctx.transient_parent {
        Some(ParentRole::Toplevel(t)) => Some(t),
        _ => None,
    }
}

/// A window its client sizes and places: a panel to focus, or a bar or toast.
fn notification(facts: &WindowFacts, ctx: &ClassifyContext, kind: XKind) -> Classification {
    let overlay = ctx.centre_overlay.or(ctx.fallback_overlay);
    let hints_allow =
        matches!(facts.input, InputHint::Absent | InputHint::True) || facts.take_focus;
    if kind == XKind::Notification && !facts.override_redirect && hints_allow {
        // Transient for a window, or opened by a click in another window of its client
        // (Feishu closes its panels the moment they lose focus).
        let anchor = match facts.transient_for {
            Some(_) => ctx.transient_parent,
            None => ctx
                .recent_press
                .filter(|p| p.window != facts.window && p.client == facts.client)
                .map(|p| p.role),
        };
        match anchor {
            Some(ParentRole::Toplevel(t) | ParentRole::OfToplevel(t)) => {
                return Classification {
                    kind,
                    role: Role::PanelOf { parent: t },
                    focus_on_map: FocusOnMap::Panel,
                };
            }
            Some(ParentRole::Overlay | ParentRole::MappedNoRole) => {
                if let Some(output) = overlay {
                    return Classification {
                        kind,
                        role: Role::OverlayWindow {
                            output,
                            panel: true,
                        },
                        focus_on_map: FocusOnMap::Panel,
                    };
                }
            }
            Some(ParentRole::Unknown) | None => {}
        }
    }
    let role = match overlay {
        Some(output) => Role::OverlayWindow {
            output,
            panel: false,
        },
        None => Role::Toplevel {
            parent: direct_toplevel(ctx),
            fixed_size: true,
        },
    };
    Classification {
        kind,
        role,
        focus_on_map: FocusOnMap::None,
    }
}

/// A popup: over the open panel if it belongs to it, else of the hovered or last
/// focused toplevel, else a toplevel of its own.
fn popup(facts: &WindowFacts, ctx: &ClassifyContext, kind: XKind) -> Classification {
    if let Some(panel) = ctx.panel {
        // An input method's candidates (any client) and the panel's own menus go with it:
        // as popups of a window below it they would be hidden.
        if facts.override_redirect {
            return Classification {
                kind,
                role: Role::OverlayPopupOf {
                    panel: panel.window,
                },
                focus_on_map: FocusOnMap::None,
            };
        }
        if facts.client == panel.client {
            return Classification {
                kind,
                role: Role::Popup {
                    parent: panel.window,
                },
                focus_on_map: popup_focus(facts),
            };
        }
    }
    match ctx.last_hovered.or(ctx.last_toplevel) {
        Some(parent) => Classification {
            kind,
            role: Role::Popup { parent },
            focus_on_map: popup_focus(facts),
        },
        None => Classification {
            kind,
            role: Role::Toplevel {
                parent: None,
                fixed_size: false,
            },
            focus_on_map: FocusOnMap::None,
        },
    }
}

/// Upstream #494: override-redirect popups never take focus; one that advertises
/// WM_TAKE_FOCUS is offered it; one whose WM_HINTS say it takes input gets it.
fn popup_focus(facts: &WindowFacts) -> FocusOnMap {
    if facts.override_redirect {
        FocusOnMap::None
    } else if facts.take_focus {
        FocusOnMap::TakeFocus
    } else if facts.input == InputHint::True {
        FocusOnMap::SetInput
    } else {
        FocusOnMap::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::model::testkit::*;
    use crate::server::model::*;
    use crate::xstate::WindowRole;
    use NetWmType::*;

    fn kind(f: WindowFacts) -> XKind {
        x_kind(&f)
    }

    fn ctx() -> ClassifyContext {
        ClassifyContext::default()
    }

    /// Output O1's overlay is under the window's centre.
    fn on_overlay() -> ClassifyContext {
        ClassifyContext {
            centre_overlay: Some(O1),
            ..ctx()
        }
    }

    fn class_of(f: &WindowFacts, c: &ClassifyContext) -> Classification {
        classify(f, c)
    }

    fn role_of(f: &WindowFacts, c: &ClassifyContext) -> Role {
        classify(f, c).role
    }

    fn panel_ctx(window: u32, client: u32) -> PanelContext {
        PanelContext {
            window: win(window),
            client,
            placement: PanelPlacement::Overlay(O1),
        }
    }

    // --- The fork X-side layer (moved from src/xstate/tests.rs) ---

    // WeChat's like/comment bubble in Moments (b348923): a frameless Qt::Tool window for
    // the Moments window that takes no input. Moved from xstate.
    #[test]
    fn wechat_moments_comment_bubble() {
        let bubble = || {
            facts(A | 2)
                .types(&[Utility, Normal])
                .transient(A | 1)
                .no_decor()
                .min(362, 72)
                .class("wechat")
        };
        assert_eq!(kind(bubble().input(InputHint::False)), XKind::NoInputHelper);
        // Taking input, or unrelated to another window, it is a window of its own.
        let unrelated = facts(A | 2)
            .types(&[Utility, Normal])
            .no_decor()
            .min(362, 72)
            .input(InputHint::False);
        for f in [bubble().input(InputHint::True), bubble(), unrelated] {
            assert_eq!(kind(f), XKind::Toplevel);
        }
    }

    // Feishu's incoming call bar (7073337, 815c828). Moved from xstate.
    #[test]
    fn feishu_incoming_call_bar() {
        let call_bar = |position: bool, keep_above: bool, transient: bool, framed: bool| {
            let mut f = facts(A | 3).types(&[Normal]).min(736, 172).class("Meeting");
            if position {
                f = f.position();
            }
            if keep_above {
                f = f.keep_above();
            }
            if transient {
                f = f.transient(A | 1);
            }
            f.motif.decorations = Some(u32::from(framed));
            f
        };
        assert_eq!(kind(call_bar(true, true, false, false)), XKind::CallBar);
        // The participant view while sharing is the same kind of window, of a fixed size.
        let participants = facts(A | 4)
            .types(&[Normal])
            .no_decor()
            .position()
            .min(320, 238)
            .max(320, 238)
            .keep_above();
        assert_eq!(kind(participants), XKind::Toplevel);
        for (position, keep_above, transient, framed) in [
            (false, true, false, false),
            (true, false, false, false),
            (true, true, true, false),
            (true, true, false, true),
        ] {
            assert_eq!(
                kind(call_bar(position, keep_above, transient, framed)),
                XKind::Toplevel
            );
        }
    }

    // Feishu's meeting control bars (1485417). Moved from xstate.
    #[test]
    fn feishu_meeting_bar() {
        let bar = facts(A | 5)
            .types(&[Notification])
            .min(562, 56)
            .no_decor()
            .class("Meeting");
        assert_eq!(kind(bar), XKind::Notification);
    }

    // The fork arms are decided by the first recognised type, as upstream's walk returns at
    // the first type it recognises (design §1.1 order cases).
    #[test]
    fn first_recognised_type_decides_the_fork_arms() {
        let dialog_popup = |types: &[NetWmType]| {
            facts(A | 6)
                .types(types)
                .transient(A | 1)
                .no_decor()
                .min(10, 10)
                .max(10, 10)
                .guessed(WindowRole::Popup)
        };
        assert_eq!(
            kind(dialog_popup(&[Notification, Dialog])),
            XKind::Notification
        );
        assert_eq!(kind(dialog_popup(&[Dialog, Notification])), XKind::Popup);

        let call_bar_facts = |types: &[NetWmType]| {
            facts(A | 7)
                .types(types)
                .keep_above()
                .position()
                .no_decor()
                .min(736, 172)
        };
        assert_eq!(kind(call_bar_facts(&[Utility, Normal])), XKind::Toplevel);
        assert_eq!(kind(call_bar_facts(&[])), XKind::CallBar);
        assert_eq!(kind(call_bar_facts(&[Other, Normal])), XKind::CallBar);

        let helper_facts = |types: &[NetWmType]| {
            facts(A | 8)
                .types(types)
                .transient(A | 1)
                .no_decor()
                .input(InputHint::False)
        };
        assert_eq!(kind(helper_facts(&[Normal, Utility])), XKind::Toplevel);
        assert_eq!(kind(helper_facts(&[Utility])), XKind::NoInputHelper);

        assert_eq!(
            kind(facts(A | 9).types(&[Other, Notification])),
            XKind::Notification
        );
        assert_eq!(kind(facts(A | 9).types(&[Other])), XKind::Toplevel);
    }

    // `pre` (override-redirect, no motif functions, yabridge) keeps upstream's guess, and
    // every type is its own value (upstream's xstate fixture gives TOOLTIP UTILITY's atom).
    #[test]
    fn pre_keeps_the_guess_and_types_are_distinct() {
        let notification = || facts(A | 10).types(&[Notification]);
        assert_eq!(kind(notification().or()), XKind::Popup);
        assert_eq!(
            kind(notification().functions_none().guessed(WindowRole::Popup)),
            XKind::Popup
        );
        assert_eq!(
            kind(
                notification()
                    .class("yabridge-host.exe")
                    .guessed(WindowRole::Popup)
            ),
            XKind::Popup
        );
        assert_eq!(
            kind(facts(A | 10).types(&[Tooltip]).guessed(WindowRole::Popup)),
            XKind::Popup
        );
        assert_eq!(
            kind(
                facts(A | 10)
                    .types(&[Tooltip])
                    .transient(A | 1)
                    .no_decor()
                    .input(InputHint::False)
                    .guessed(WindowRole::Popup)
            ),
            XKind::Popup
        );
        assert_eq!(
            kind(facts(A | 10).types(&[Splash]).guessed(WindowRole::Splash)),
            XKind::Splash
        );
    }

    // --- Placement ---

    // B1: the size of an output is fullscreen, whatever the window is.
    #[test]
    fn output_sized_windows_are_fullscreen() {
        let c = ClassifyContext {
            output_modes: vec![(3840, 2160)],
            transient_parent: Some(ParentRole::Toplevel(win(A | 1))),
            ..on_overlay()
        };
        let big = |f: WindowFacts| f.at(0, 0, 3840, 2160);
        assert_eq!(
            class_of(&big(facts(A | 2).transient(A | 1)), &c),
            Classification {
                kind: XKind::Toplevel,
                role: Role::FullscreenToplevel {
                    parent: Some(win(A | 1)),
                    fixed_size: false
                },
                focus_on_map: FocusOnMap::None,
            }
        );
        assert_eq!(
            role_of(&big(facts(A | 3).or().transient(A | 1)), &c),
            Role::FullscreenToplevel {
                parent: None,
                fixed_size: false
            }
        );
        assert_eq!(
            role_of(
                &big(facts(A | 4).types(&[Notification]).transient(A | 1)),
                &c
            ),
            Role::FullscreenToplevel {
                parent: Some(win(A | 1)),
                fixed_size: true
            }
        );
        assert_eq!(
            role_of(
                &big(facts(A | 5).types(&[Splash]).guessed(WindowRole::Splash)),
                &c
            ),
            Role::FullscreenToplevel {
                parent: Some(win(A | 1)),
                fixed_size: true
            }
        );
        // Mode sizes, not X rects, decide (Q5): a rotated output's X size is not one.
        let modes_only = ClassifyContext {
            output_modes: vec![(3840, 2160)],
            ..ctx()
        };
        assert_eq!(
            role_of(&facts(A | 6).at(0, 0, 2160, 3840), &modes_only),
            TOPLEVEL
        );
    }

    // Toplevels: the transient parent when it is a toplevel itself; Splash keeps its size.
    #[test]
    fn toplevels_and_their_parents() {
        let with_parent = |p| ClassifyContext {
            transient_parent: Some(p),
            ..ctx()
        };
        let f = facts(A | 2).transient(A | 1);
        assert_eq!(
            role_of(&f, &with_parent(ParentRole::Toplevel(win(A | 1)))),
            Role::Toplevel {
                parent: Some(win(A | 1)),
                fixed_size: false
            }
        );
        for p in [
            ParentRole::OfToplevel(win(A | 1)),
            ParentRole::Overlay,
            ParentRole::MappedNoRole,
            ParentRole::Unknown,
        ] {
            assert_eq!(role_of(&f, &with_parent(p)), TOPLEVEL);
        }
        let splash = facts(A | 3).types(&[Splash]).guessed(WindowRole::Splash);
        assert_eq!(
            class_of(&splash, &ctx()),
            Classification {
                kind: XKind::Splash,
                role: Role::Toplevel {
                    parent: None,
                    fixed_size: true
                },
                focus_on_map: FocusOnMap::None,
            }
        );
    }

    // B3/B4: ordinary popups are popups of the hovered window, else of the last toplevel,
    // else plain toplevels.
    #[test]
    fn popup_parents() {
        let popup = facts(A | 5).or();
        let c = ClassifyContext {
            last_hovered: Some(win(A | 1)),
            last_toplevel: Some(win(A | 2)),
            ..ctx()
        };
        assert_eq!(role_of(&popup, &c), Role::Popup { parent: win(A | 1) });
        let c = ClassifyContext {
            last_hovered: None,
            ..c
        };
        assert_eq!(role_of(&popup, &c), Role::Popup { parent: win(A | 2) });
        assert_eq!(
            class_of(&popup, &ctx()),
            Classification {
                kind: XKind::Popup,
                role: TOPLEVEL,
                focus_on_map: FocusOnMap::None,
            }
        );
        // The WeChat bubble is placed as a popup (its WM_TRANSIENT_FOR is not its parent).
        let bubble = facts(A | 6)
            .types(&[Utility, Normal])
            .transient(A | 9)
            .no_decor()
            .input(InputHint::False);
        assert_eq!(
            class_of(&bubble, &c),
            Classification {
                kind: XKind::NoInputHelper,
                role: Role::Popup { parent: win(A | 2) },
                focus_on_map: FocusOnMap::None,
            }
        );
    }

    // Rule 11 / upstream #494: OR never; WM_TAKE_FOCUS → TakeFocus; input true → SetInput.
    #[test]
    fn popup_focus_on_map() {
        let c = ClassifyContext {
            last_toplevel: Some(win(A | 1)),
            ..ctx()
        };
        let popup = || facts(A | 5).guessed(WindowRole::Popup).types(&[PopupMenu]);
        let focus = |f: WindowFacts| class_of(&f, &c).focus_on_map;
        assert_eq!(
            focus(popup().or().take_focus().input(InputHint::True)),
            FocusOnMap::None
        );
        assert_eq!(
            focus(popup().take_focus().input(InputHint::False)),
            FocusOnMap::TakeFocus
        );
        assert_eq!(focus(popup().input(InputHint::True)), FocusOnMap::SetInput);
        assert_eq!(focus(popup().input(InputHint::False)), FocusOnMap::None);
        assert_eq!(focus(popup()), FocusOnMap::None);
    }

    // Rule 11: under an open panel, OR popups of any client and non-OR popups of the
    // panel's client attach to it; other clients' popups keep their ordinary parent.
    #[test]
    fn popups_under_an_open_panel() {
        let c = ClassifyContext {
            panel: Some(panel_ctx(A | 3, A)),
            last_hovered: Some(win(A | 1)),
            ..ctx()
        };
        assert_eq!(
            class_of(&facts(C | 1).or().class("fcitx"), &c),
            Classification {
                kind: XKind::Popup,
                role: Role::OverlayPopupOf { panel: win(A | 3) },
                focus_on_map: FocusOnMap::None,
            }
        );
        let menu = |id| {
            facts(id)
                .guessed(WindowRole::Popup)
                .types(&[PopupMenu])
                .input(InputHint::True)
        };
        assert_eq!(
            class_of(&menu(A | 4), &c),
            Classification {
                kind: XKind::Popup,
                role: Role::Popup { parent: win(A | 3) },
                focus_on_map: FocusOnMap::SetInput,
            }
        );
        assert_eq!(
            role_of(&menu(B | 4), &c),
            Role::Popup { parent: win(A | 1) }
        );
    }

    // --- Panels (design §1.1-bis) ---

    fn notification(id: u32) -> WindowFacts {
        facts(id).types(&[Notification]).no_decor().class("Meeting")
    }

    // (a) Transient for a mapped window: placement by the anchor table.
    #[test]
    fn panels_transient_for_a_window() {
        let with = |p| ClassifyContext {
            transient_parent: Some(p),
            ..on_overlay()
        };
        let panel = notification(A | 3).transient(A | 1);
        let panel_of = |t: u32| Classification {
            kind: XKind::Notification,
            role: Role::PanelOf { parent: win(t) },
            focus_on_map: FocusOnMap::Panel,
        };
        assert_eq!(
            class_of(&panel, &with(ParentRole::Toplevel(win(A | 1)))),
            panel_of(A | 1)
        );
        assert_eq!(
            class_of(&panel, &with(ParentRole::OfToplevel(win(A | 2)))),
            panel_of(A | 2)
        );
        let overlay_panel = Classification {
            kind: XKind::Notification,
            role: Role::OverlayWindow {
                output: O1,
                panel: true,
            },
            focus_on_map: FocusOnMap::Panel,
        };
        assert_eq!(class_of(&panel, &with(ParentRole::Overlay)), overlay_panel);
        // Anchor X-mapped without a role yet: today's B6 outcome.
        assert_eq!(
            class_of(&panel, &with(ParentRole::MappedNoRole)),
            overlay_panel
        );
        // Anchor unmapped or unknown: not a panel.
        assert_eq!(
            class_of(&panel, &with(ParentRole::Unknown)),
            Classification {
                kind: XKind::Notification,
                role: Role::OverlayWindow {
                    output: O1,
                    panel: false
                },
                focus_on_map: FocusOnMap::None,
            }
        );
        // An overlay anchor with no overlay for this window: not a panel, B7.
        let no_overlay = ClassifyContext {
            transient_parent: Some(ParentRole::Overlay),
            ..ctx()
        };
        assert_eq!(
            role_of(&panel, &no_overlay),
            Role::Toplevel {
                parent: None,
                fixed_size: true
            }
        );
        // The fallback overlay is the centre's overlay when the centre is on none (Q4).
        let fallback = ClassifyContext {
            transient_parent: Some(ParentRole::Overlay),
            fallback_overlay: Some(O2),
            ..ctx()
        };
        assert_eq!(
            role_of(&panel, &fallback),
            Role::OverlayWindow {
                output: O2,
                panel: true
            }
        );
    }

    // Hints: input false is never a panel; WM_TAKE_FOCUS makes it one anyway.
    #[test]
    fn panel_hints() {
        let c = ClassifyContext {
            transient_parent: Some(ParentRole::Toplevel(win(A | 1))),
            ..on_overlay()
        };
        let panel = || notification(A | 3).transient(A | 1);
        assert_eq!(
            role_of(&panel().input(InputHint::False), &c),
            Role::OverlayWindow {
                output: O1,
                panel: false
            }
        );
        assert_eq!(
            role_of(&panel().input(InputHint::False).take_focus(), &c),
            Role::PanelOf { parent: win(A | 1) }
        );
        assert_eq!(
            role_of(&panel().input(InputHint::True), &c),
            Role::PanelOf { parent: win(A | 1) }
        );
    }

    // (b) No WM_TRANSIENT_FOR: opened by a press in another window of its client within
    // 1.5 s (the context carries only such a press).
    #[test]
    fn panels_opened_by_a_click() {
        let pressed = |window: u32, client: u32, role| ClassifyContext {
            recent_press: Some(RecentPress {
                window: win(window),
                client,
                role,
            }),
            ..on_overlay()
        };
        let panel = notification(A | 3);
        assert_eq!(
            class_of(&panel, &pressed(A | 1, A, ParentRole::Toplevel(win(A | 1)))),
            Classification {
                kind: XKind::Notification,
                role: Role::PanelOf { parent: win(A | 1) },
                focus_on_map: FocusOnMap::Panel,
            }
        );
        assert_eq!(
            role_of(&panel, &pressed(A | 2, A, ParentRole::Overlay)),
            Role::OverlayWindow {
                output: O1,
                panel: true
            }
        );
        let toast = Role::OverlayWindow {
            output: O1,
            panel: false,
        };
        // Another client's window, the window itself, or no recent press: a toast.
        assert_eq!(
            role_of(&panel, &pressed(B | 1, B, ParentRole::Toplevel(win(B | 1)))),
            toast
        );
        assert_eq!(
            role_of(&panel, &pressed(A | 3, A, ParentRole::MappedNoRole)),
            toast
        );
        assert_eq!(role_of(&panel, &on_overlay()), toast);
        // With WM_TRANSIENT_FOR, only (a) applies.
        let transient = ClassifyContext {
            transient_parent: Some(ParentRole::Unknown),
            ..pressed(A | 1, A, ParentRole::Toplevel(win(A | 1)))
        };
        assert_eq!(
            role_of(&notification(A | 3).transient(A | 9), &transient),
            toast
        );
    }

    // A4 call bars place themselves but are never panels (smell #20), click or not.
    #[test]
    fn call_bars_are_never_panels() {
        let c = ClassifyContext {
            recent_press: Some(RecentPress {
                window: win(A | 1),
                client: A,
                role: ParentRole::Toplevel(win(A | 1)),
            }),
            ..on_overlay()
        };
        let bar = facts(A | 3)
            .types(&[Normal])
            .no_decor()
            .keep_above()
            .position()
            .min(736, 172);
        assert_eq!(
            class_of(&bar, &c),
            Classification {
                kind: XKind::CallBar,
                role: Role::OverlayWindow {
                    output: O1,
                    panel: false
                },
                focus_on_map: FocusOnMap::None,
            }
        );
        assert_eq!(
            role_of(&bar, &ctx()),
            Role::Toplevel {
                parent: None,
                fixed_size: true
            }
        );
    }

    // Toasts and bars: on the overlay when there is one, else fixed-size toplevels (B7).
    #[test]
    fn non_panel_notifications() {
        assert_eq!(
            class_of(&notification(A | 4), &on_overlay()),
            Classification {
                kind: XKind::Notification,
                role: Role::OverlayWindow {
                    output: O1,
                    panel: false
                },
                focus_on_map: FocusOnMap::None,
            }
        );
        assert_eq!(
            role_of(&notification(A | 4), &ctx()),
            Role::Toplevel {
                parent: None,
                fixed_size: true
            }
        );
    }
}
