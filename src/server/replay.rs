//! Differential replay (design §3.2 (2)): a day's trace of the old code, its raw events fed
//! through the window model, the model's X focus decisions and role objects compared with
//! what the old code did, batch by batch. Every difference must be one of the deliberate
//! changes (an allowance); any other fails the test.

use super::classify::{classify, x_kind};
use super::context::derive_context;
use super::focus::in_family;
use super::model::{
    Compositor, FocusState, InputHint, Method, Millis, Model, Output, OutputId, PressRef, RawEvent,
    Role, RoleTable, SurfaceRef, WindowFacts, XFocusChange, XKind,
};
use super::trace_codec::{FocusCall, Record, RoleKind, decode, role_object};
use std::collections::BTreeMap;
use xcb::{Xid, x};

/// Evidence which must survive a batch boundary, reset when a window id is reused.
#[derive(Debug, Clone, Default)]
pub struct History {
    pub hints_changed: Vec<x::Window>,
    pub press_mask_changed: Vec<x::Window>,
}

impl History {
    fn observe(&mut self, raw: &RawEvent, model: &Model, now: Millis) {
        match raw {
            RawEvent::MapFacts(WindowFacts { window, .. })
            | RawEvent::Unmap { window }
            | RawEvent::Destroy { window } => {
                self.hints_changed.retain(|w| w != window);
                self.press_mask_changed.retain(|w| w != window);
            }
            RawEvent::HintsChanged { window, .. } if model.roles.is_mapped(*window) => {
                if !self.hints_changed.contains(window) {
                    self.hints_changed.push(*window);
                }
            }
            RawEvent::RoleCreate { window, dims } => {
                self.press_mask_changed.retain(|w| w != window);
                let Some(entry) = model.roles.entry(*window) else {
                    return;
                };
                let mut facts = entry.facts.clone();
                facts.dims = *dims;
                if x_kind(&facts) != XKind::Notification || facts.transient_for.is_some() {
                    return;
                }
                let mut ctx =
                    derive_context(&facts, &model.roles, &model.focus, &model.presses, now);
                let Some(press) = ctx.recent_press else {
                    return;
                };
                // The old branch hard-coded the low 21 resource-id bits. Change only
                // that equality test, keeping age, anchor and all other facts fixed.
                let legacy_same =
                    window.resource_id() & !0x1f_ffff == press.window.resource_id() & !0x1f_ffff;
                if legacy_same == (press.client == facts.client) {
                    return;
                }
                let new = classify(&facts, &ctx);
                ctx.recent_press.as_mut().unwrap().client = if legacy_same {
                    facts.client
                } else {
                    facts.client ^ 1
                };
                if classify(&facts, &ctx) != new {
                    self.press_mask_changed.push(*window);
                }
            }
            _ => {}
        }
    }
}

/// What one batch did, old and new.
#[derive(Debug)]
pub struct Batch {
    pub index: usize,
    /// The raw events of the batch, in order.
    pub raw: Vec<RawEvent>,
    /// The focus machine's state and the model's table before the batch.
    pub before: FocusState,
    pub roles: RoleTable,
    /// Windows asked to close (an `x_call` `close_window`) and not unmapped yet.
    pub closing: Vec<x::Window>,
    pub old: Vec<FocusCall>,
    pub new: Vec<XFocusChange>,
    pub history: History,
}

#[derive(Debug)]
pub enum Divergence {
    Focus(Batch),
    Role {
        index: usize,
        window: x::Window,
        old: (RoleKind, Option<x::Window>, Option<OutputId>),
        new: (RoleKind, Option<x::Window>, Option<OutputId>),
        before: FocusState,
        roles: RoleTable,
        closing: Vec<x::Window>,
        raw: Vec<RawEvent>,
        history: History,
    },
    /// The old code made a role for a window the model has no classification for (a
    /// `role_create` missing from the trace, or a model that lost the window): always a finding.
    Unclassified {
        index: usize,
        window: x::Window,
        raw: Vec<RawEvent>,
    },
}

/// The deliberate changes a divergence may be (design §2.2, §5.2, Q-decisions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeId {
    Rule1PopupKeepsFocus,
    Rule2EnterThenLeave,
    Rule5PressFocusesPressed,
    Rule5PressWithoutPanel,
    Rule7BarUnderPanel,
    Rule7OneShotPress,
    Rule7ImmediatePress,
    Rule8OverlayHoverKeepsFocus,
    Rule9RestoreOnGone,
    Rule10CloseRequestKeepsReferences,
    Rule11AttachUnderPanel,
    Rule12ActivationReleasesPanel,
    Rule13NoO1WhileNotFocused,
    PanelPredicate,
    HintsChangedAfterMap,
    Smell5OverrideRedirectPress,
    Q1RestoreMethod,
}

/// Same focus outcome under the fixed mapping (the output name is ignored, rule 13 has its
/// own tests).
fn same(old: &FocusCall, new: &XFocusChange) -> bool {
    match (old, new) {
        (FocusCall::SetInput { window, .. }, XFocusChange::None) => *window == x::WINDOW_NONE,
        (
            FocusCall::SetInput { window, .. },
            XFocusChange::Window {
                window: w,
                method: Method::SetInput,
                ..
            },
        ) => window == w,
        (
            FocusCall::TakeFocus(window),
            XFocusChange::Window {
                window: w,
                method: Method::TakeFocus,
                ..
            },
        ) => window == w,
        _ => false,
    }
}

/// Q14: equal when both did nothing, or both did something and the last calls agree.
fn batch_agrees(batch: &Batch) -> bool {
    match (batch.old.last(), batch.new.last()) {
        (None, None) => true,
        (Some(old), Some(new)) => same(old, new),
        _ => false,
    }
}

#[derive(Debug, Default)]
pub struct ReplayReport {
    pub divergences: Vec<Divergence>,
    pub batches: usize,
    pub undecodable: usize,
    /// One-based line numbers and decoder errors; bounded even for a damaged day trace.
    pub decode_errors: Vec<(usize, String)>,
    pub unknown: usize,
    pub unfinished_batch: bool,
}

pub fn replay(trace: &str) -> Vec<Divergence> {
    replay_with_report(trace).divergences
}

pub fn replay_with_report(trace: &str) -> ReplayReport {
    let mut model = Model::default();
    let mut report = ReplayReport::default();
    let mut history = History::default();
    let mut closing: Vec<x::Window> = Vec::new();
    let fresh = |index: usize, model: &Model, closing: &[x::Window], history: &History| Batch {
        index,
        raw: Vec::new(),
        before: model.focus.clone(),
        roles: model.roles.clone(),
        closing: closing.to_vec(),
        old: Vec::new(),
        new: Vec::new(),
        history: history.clone(),
    };
    let mut batch = fresh(0, &model, &closing, &history);
    for (n, line) in trace.lines().enumerate() {
        let record = match decode(line) {
            Ok(record) => record,
            Err(error) => {
                report.undecodable += 1;
                if report.decode_errors.len() < 20 {
                    report.decode_errors.push((n + 1, error));
                }
                continue;
            }
        };
        match record {
            Record::Raw { t, event } => {
                let end = event == RawEvent::BatchEnd;
                history.observe(&event, &model, t);
                batch.history.press_mask_changed = history.press_mask_changed.clone();
                // A window gone, by unmap, destroy or reparent-away (a `Destroy` in the trace),
                // is no longer being closed; a later window reusing its id must not match.
                if let RawEvent::Unmap { window } | RawEvent::Destroy { window } = event {
                    closing.retain(|&w| w != window);
                }
                for output in model.feed(&event, t) {
                    if let Output::XFocus(change) = output {
                        batch.new.push(change);
                    }
                }
                batch.raw.push(event);
                if end {
                    report.batches += 1;
                    let next = fresh(batch.index + 1, &model, &closing, &history);
                    let done = std::mem::replace(&mut batch, next);
                    if !batch_agrees(&done) {
                        report.divergences.push(Divergence::Focus(done));
                    }
                }
            }
            Record::FocusCall { call, .. } => batch.old.push(call),
            Record::CloseCall { window, .. } => {
                if !closing.contains(&window) {
                    closing.push(window);
                }
            }
            Record::Role {
                window,
                kind,
                parent,
                output,
                rehome: false,
                ..
            } => match model.roles.classification(window) {
                Some(c) => {
                    let new = role_object(&c, &model.roles);
                    if new != (kind, parent, output) {
                        report.divergences.push(Divergence::Role {
                            index: batch.index,
                            window,
                            old: (kind, parent, output),
                            new,
                            before: batch.before.clone(),
                            roles: batch.roles.clone(),
                            closing: closing.clone(),
                            raw: batch.raw.clone(),
                            history: batch.history.clone(),
                        });
                    }
                }
                None => report.divergences.push(Divergence::Unclassified {
                    index: batch.index,
                    window,
                    raw: batch.raw.clone(),
                }),
            },
            Record::Unknown => report.unknown += 1,
            Record::Role { .. } | Record::Start | Record::OtherCall => {}
        }
    }
    // Do not invent a BatchEnd: the writer may have stopped mid-batch.
    report.unfinished_batch = !batch.raw.is_empty() || !batch.old.is_empty();
    report
}

const CHANGES: [ChangeId; 17] = [
    ChangeId::Rule1PopupKeepsFocus,
    ChangeId::Rule2EnterThenLeave,
    ChangeId::Rule5PressFocusesPressed,
    ChangeId::Rule5PressWithoutPanel,
    ChangeId::Rule7BarUnderPanel,
    ChangeId::Rule7OneShotPress,
    ChangeId::Rule7ImmediatePress,
    ChangeId::Rule8OverlayHoverKeepsFocus,
    ChangeId::Rule9RestoreOnGone,
    ChangeId::Rule10CloseRequestKeepsReferences,
    ChangeId::Rule11AttachUnderPanel,
    ChangeId::Rule12ActivationReleasesPanel,
    ChangeId::Rule13NoO1WhileNotFocused,
    ChangeId::PanelPredicate,
    ChangeId::HintsChangedAfterMap,
    ChangeId::Smell5OverrideRedirectPress,
    ChangeId::Q1RestoreMethod,
];

fn old_window(call: &FocusCall) -> Option<x::Window> {
    match call {
        FocusCall::SetInput { window, .. } if *window == x::WINDOW_NONE => None,
        FocusCall::SetInput { window, .. } | FocusCall::TakeFocus(window) => Some(*window),
    }
}

fn new_window(change: &XFocusChange) -> Option<x::Window> {
    match change {
        XFocusChange::Window { window, .. } => Some(*window),
        XFocusChange::None => None,
    }
}

fn pressed(event: &RawEvent) -> Option<x::Window> {
    match event {
        RawEvent::Press {
            target: PressRef::X(w) | PressRef::Decoration(w),
            ..
        } => Some(*w),
        _ => None,
    }
}

fn focuses_press_or_compositor(batch: &Batch, event: &RawEvent) -> bool {
    let Some(window) = batch.new.last().and_then(new_window) else {
        return false;
    };
    pressed(event) == Some(window) || batch.before.compositor == Compositor::X(window)
}

fn facts_for<'a>(
    roles: &'a RoleTable,
    raw: &'a [RawEvent],
    w: x::Window,
) -> Option<&'a WindowFacts> {
    raw.iter()
        .rev()
        .find_map(|event| match event {
            RawEvent::MapFacts(f) if f.window == w => Some(f),
            _ => None,
        })
        .or_else(|| roles.entry(w).map(|e| &e.facts))
}

fn overlay(roles: &RoleTable, w: x::Window) -> Option<(OutputId, bool)> {
    match roles.classification(w)?.role {
        Role::OverlayWindow { output, panel } => Some((output, panel)),
        _ => None,
    }
}

fn overlay_enter(raw: &[RawEvent]) -> bool {
    raw.iter().any(|event| {
        matches!(
            event,
            RawEvent::KeyboardEnter {
                target: SurfaceRef::Overlay(_),
                ..
            }
        )
    })
}

fn panel_facts(roles: &RoleTable, raw: &[RawEvent], history: &History, w: x::Window) -> bool {
    facts_for(roles, raw, w).is_some_and(|facts| {
        let kind = x_kind(facts);
        kind == XKind::CallBar
            || (kind == XKind::Notification
                && (facts.input == InputHint::False || history.press_mask_changed.contains(&w)))
    })
}

fn hints_changed(history: &History, raw: &[RawEvent], w: x::Window) -> bool {
    history.hints_changed.contains(&w)
        && !raw
            .iter()
            .any(|event| matches!(event, RawEvent::MapFacts(f) if f.window == w))
}

/// The first matching deliberate change, in the brief's order. Missing classifications
/// are findings even if another window in the batch would qualify for an allowance.
pub fn allowed(divergence: &Divergence) -> Option<ChangeId> {
    CHANGES.into_iter().find(|id| explains(*id, divergence))
}

fn explains(id: ChangeId, divergence: &Divergence) -> bool {
    match (id, divergence) {
        (ChangeId::Rule1PopupKeepsFocus, Divergence::Focus(b)) => {
            b.new.is_empty() && b.raw.iter().any(|event| match event {
                RawEvent::KeyboardEnter { target: SurfaceRef::X(t), .. } => {
                    b.old.last().and_then(old_window) == Some(*t)
                        && b.before.desired.is_some_and(|w| {
                            b.roles.classification(w).is_some_and(|c| c.role.is_popup())
                                && in_family(&b.roles, w, *t)
                        })
                }
                _ => false,
            })
        }
        (ChangeId::Rule2EnterThenLeave, Divergence::Focus(b)) => {
            b.old.is_empty() && b.new.last() == Some(&XFocusChange::None)
                && b.raw.iter().enumerate().any(|(i, event)| match event {
                    RawEvent::KeyboardLeave { target: SurfaceRef::X(w), .. } => {
                        b.raw[..i].iter().any(|e| matches!(e, RawEvent::KeyboardEnter { target: SurfaceRef::X(t), .. } if t == w))
                            && !b.raw[i + 1..].iter().any(|e| matches!(e, RawEvent::KeyboardEnter { .. }))
                    }
                    _ => false,
                })
        }
        (ChangeId::Rule5PressFocusesPressed, Divergence::Focus(b)) => {
            b.before.panel.is_some_and(|panel| b.raw.iter().any(|event| {
                pressed(event).is_some_and(|w| overlay(&b.roles, w).is_none() && !in_family(&b.roles, w, panel))
                    && focuses_press_or_compositor(b, event)
            }))
        }
        (ChangeId::Rule5PressWithoutPanel, Divergence::Focus(b)) => {
            b.before.panel.is_none() && b.old.is_empty()
                && b.raw.iter().any(|event| matches!(event, RawEvent::Press { .. })
                    && pressed(event).is_none_or(|w| overlay(&b.roles, w).is_none())
                    && focuses_press_or_compositor(b, event))
        }
        (ChangeId::Rule7BarUnderPanel, Divergence::Focus(b)) => {
            b.before.panel.is_some() && b.new.is_empty()
                && b.old.last().and_then(old_window).is_some_and(|w| {
                    overlay(&b.roles, w).is_some_and(|(_, panel)| !panel)
                })
        }
        (ChangeId::Rule7OneShotPress, Divergence::Focus(b)) => {
            overlay_enter(&b.raw) && b.before.pending_press.is_none() && b.new.is_empty()
                && b.old.last().and_then(old_window).is_some_and(|w| {
                    overlay(&b.roles, w).is_some()
                        && !b.raw.iter().any(|event| pressed(event) == Some(w))
                })
        }
        (ChangeId::Rule7ImmediatePress, Divergence::Focus(b)) => {
            b.old.is_empty() && b.raw.iter().any(|event| match event {
                RawEvent::Press { target: PressRef::X(w), .. } => {
                    overlay(&b.roles, *w).is_some_and(|(o, _)| b.before.compositor == Compositor::Overlay(o))
                        && b.new.last().and_then(new_window) == Some(*w)
                }
                _ => false,
            })
        }
        (ChangeId::Rule8OverlayHoverKeepsFocus, Divergence::Focus(b)) => {
            b.new.is_empty()
                && matches!(b.old.last(), Some(FocusCall::SetInput { window, .. }) if *window == x::WINDOW_NONE)
                && b.raw.iter().any(|event| match event {
                    RawEvent::KeyboardEnter { target: SurfaceRef::Overlay(o), .. } => {
                        b.before.pending_press.is_none_or(|(_, output)| output != *o)
                            && !b.raw.iter().filter_map(pressed).any(|w| {
                                overlay(&b.roles, w).is_some_and(|(output, _)| output == *o)
                            })
                    }
                    _ => false,
                })
        }
        (ChangeId::Rule9RestoreOnGone, Divergence::Focus(b)) => {
            b.raw.iter().any(|event| match event {
                RawEvent::Unmap { window } | RawEvent::Destroy { window } => {
                    b.before.desired == Some(*window)
                        || b.before.applied.as_ref().and_then(new_window) == Some(*window)
                }
                _ => false,
            })
        }
        (ChangeId::Rule10CloseRequestKeepsReferences, Divergence::Role { new, closing, .. }) => {
            new.0 == RoleKind::Popup && new.1.is_some_and(|p| closing.contains(&p))
        }
        (ChangeId::Rule11AttachUnderPanel, Divergence::Role { before, old, new, .. }) => {
            before.panel.is_some() && old.0 == RoleKind::OverlayPopup
                && new.0 == RoleKind::Popup && new.1.is_some()
        }
        (ChangeId::Rule12ActivationReleasesPanel, Divergence::Focus(b)) => {
            b.raw.iter().any(|event| match event {
                RawEvent::KeyboardEnter { target: SurfaceRef::X(w), .. }
                    if b.before.pending_activation == Some(*w) => b.new.last().and_then(new_window) == Some(*w),
                RawEvent::ActiveWindowRequest { window }
                    if b.before.compositor == Compositor::X(*window) => b.new.last().and_then(new_window) == Some(*window),
                _ => false,
            })
        }
        (ChangeId::Rule13NoO1WhileNotFocused, Divergence::Focus(b)) => {
            b.new.is_empty() && b.raw.iter().any(|event| match event {
                RawEvent::SurfaceEnterOutput { window, .. } => b.before.applied.as_ref().and_then(new_window) != Some(*window),
                _ => false,
            })
        }
        (ChangeId::PanelPredicate, Divergence::Role { window, roles, raw, history, .. }) => {
            panel_facts(roles, raw, history, *window)
        }
        (ChangeId::PanelPredicate, Divergence::Focus(b)) => {
            b.raw.iter().any(|event| match event {
                RawEvent::PopupFirstConfigure { window } => {
                    (b.old.last().and_then(old_window) == Some(*window) || b.new.last().and_then(new_window) == Some(*window))
                        && panel_facts(&b.roles, &b.raw, &b.history, *window)
                }
                _ => false,
            })
        }
        (ChangeId::HintsChangedAfterMap, Divergence::Role { window, raw, history, .. }) => {
            hints_changed(history, raw, *window)
        }
        (ChangeId::HintsChangedAfterMap, Divergence::Focus(b)) => {
            b.old.last().and_then(old_window).into_iter()
                .chain(b.new.last().and_then(new_window))
                .any(|w| hints_changed(&b.history, &b.raw, w))
        }
        (ChangeId::Smell5OverrideRedirectPress, Divergence::Focus(b)) => {
            matches!(b.old.last(), Some(FocusCall::SetInput { window, .. })
                if facts_for(&b.roles, &b.raw, *window).is_some_and(|f| f.override_redirect))
        }
        (ChangeId::Q1RestoreMethod, Divergence::Focus(b)) => {
            matches!((b.old.last(), b.new.last()),
                (Some(FocusCall::TakeFocus(old)), Some(XFocusChange::Window { window, method: Method::SetInput, .. }))
                    if old == window && b.roles.classification(*window).is_some_and(|c| c.role.is_toplevel()))
        }
        _ => false,
    }
}

#[test]
fn differential_replay() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/scenarios/traces");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no differential trace fixtures");
    let mut counts: BTreeMap<Option<ChangeId>, usize> =
        CHANGES.into_iter().map(|id| (Some(id), 0)).collect();
    counts.insert(None, 0);
    let mut unattributed = Vec::new();
    let mut undecodable = 0;
    for path in paths {
        let trace = std::fs::read_to_string(&path).unwrap();
        assert!(
            trace.len() <= 20_000_000,
            "trace exceeds 20 MB: {}",
            path.display()
        );
        assert!(
            !trace.contains("\"title\""),
            "trace contains a title key: {}",
            path.display()
        );
        let report = replay_with_report(&trace);
        undecodable += report.undecodable;
        eprintln!(
            "{}: batches={}, undecodable={}, unknown={}, unfinished_batch={}, decode_errors={:?}",
            path.display(),
            report.batches,
            report.undecodable,
            report.unknown,
            report.unfinished_batch,
            report.decode_errors
        );
        for d in report.divergences {
            let id = allowed(&d);
            *counts.entry(id).or_default() += 1;
            if id.is_none() && unattributed.len() < 20 {
                match &d {
                    Divergence::Focus(b) => eprintln!("unattributed focus: batch {}", b.index),
                    Divergence::Role { index, window, .. } => {
                        eprintln!("unattributed role: batch {index}, window {window:?}")
                    }
                    Divergence::Unclassified { index, window, raw } => {
                        eprintln!("unclassified: batch {index}, window {window:?}, raw={raw:?}")
                    }
                }
                unattributed.push((path.clone(), d));
            }
        }
    }
    eprintln!("divergences by change: {counts:#?}; undecodable lines: {undecodable}");
    assert!(
        unattributed.is_empty(),
        "unattributed divergences: {unattributed:#?}"
    );
}

#[cfg(test)]
mod tests {
    use super::super::model::testkit::{facts, win};
    use super::super::trace_codec::{encode, role_line};
    use super::*;

    fn trace(events: &[RawEvent]) -> String {
        events
            .iter()
            .enumerate()
            .map(|(t, event)| encode(t as u64, event) + "\n")
            .collect()
    }

    #[test]
    fn undecodable_lines_do_not_panic() {
        let trace = concat!(
            "{\"t\":0,\"k\":\"kb_enter\",\"tk\":\"overlay\",\"id\":null,\"serial\":1}\n",
            "{\"t\":1,\"k\":\"batch_end\"}\n",
        );
        assert!(replay(trace).is_empty());
    }

    #[test]
    fn missing_classification_is_always_unattributed() {
        let trace = role_line(0, win(1), (RoleKind::Popup, Some(win(2)), None), false);
        let divergences = replay(&trace);
        assert!(
            matches!(divergences.as_slice(), [Divergence::Unclassified { window, .. }] if *window == win(1))
        );
        assert_eq!(allowed(&divergences[0]), None);
    }

    #[test]
    fn popup_parent_enter_is_attributed() {
        let parent = facts(1);
        let popup = facts(2).or().input(super::super::model::InputHint::True);
        let mut model = Model::default();
        for (t, event) in [
            RawEvent::MapFacts(parent.clone()),
            RawEvent::RoleCreate {
                window: parent.window,
                dims: parent.dims,
            },
            RawEvent::PointerEnter {
                window: parent.window,
            },
            RawEvent::MapFacts(popup.clone()),
            RawEvent::RoleCreate {
                window: popup.window,
                dims: popup.dims,
            },
        ]
        .iter()
        .enumerate()
        {
            model.feed(event, t as u64);
        }
        model.focus.desired = Some(popup.window);
        let batch = Batch {
            index: 0,
            raw: vec![RawEvent::KeyboardEnter {
                target: super::super::model::SurfaceRef::X(parent.window),
                serial: 1,
            }],
            before: model.focus,
            roles: model.roles,
            closing: vec![],
            old: vec![FocusCall::SetInput {
                window: parent.window,
                output: None,
            }],
            new: vec![],
            history: History::default(),
        };
        assert_eq!(
            allowed(&Divergence::Focus(batch)),
            Some(ChangeId::Rule1PopupKeepsFocus)
        );
    }

    #[test]
    fn model_focus_waits_for_recorded_batch_end() {
        let f = facts(1);
        let mut trace = trace(&[
            RawEvent::MapFacts(f.clone()),
            RawEvent::RoleCreate {
                window: f.window,
                dims: f.dims,
            },
            RawEvent::KeyboardEnter {
                target: super::super::model::SurfaceRef::X(f.window),
                serial: 1,
            },
        ]);
        trace +=
            "{\"t\":3,\"k\":\"x_call\",\"call\":\"focus_window\",\"w\":1,\"output\":\"ignored\"}\n";
        trace += &encode(4, &RawEvent::BatchEnd);
        assert!(replay(&trace).is_empty());
    }
}

#[cfg(test)]
mod allowance_tests {
    use super::super::model::testkit::{O1, facts, win};
    use super::super::model::{
        Classification, Compositor, FocusOnMap, InputHint, PressRef, Role, SurfaceRef, WindowEntry,
        XKind,
    };
    use super::*;

    fn input(window: u32) -> FocusCall {
        FocusCall::SetInput {
            window: win(window),
            output: None,
        }
    }

    fn focus(window: u32) -> XFocusChange {
        XFocusChange::Window {
            window: win(window),
            method: Method::SetInput,
            primary_output: None,
        }
    }

    fn enter(target: SurfaceRef) -> RawEvent {
        RawEvent::KeyboardEnter { target, serial: 1 }
    }

    fn press(window: u32) -> RawEvent {
        RawEvent::Press {
            target: PressRef::X(win(window)),
            serial: 1,
            touch: false,
        }
    }

    fn batch() -> Batch {
        let mut roles = RoleTable::default();
        for (id, role) in [
            (
                1,
                Role::Toplevel {
                    parent: None,
                    fixed_size: false,
                },
            ),
            (
                2,
                Role::Toplevel {
                    parent: None,
                    fixed_size: false,
                },
            ),
            (3, Role::Popup { parent: win(1) }),
            (4, Role::PanelOf { parent: win(1) }),
            (
                5,
                Role::OverlayWindow {
                    output: O1,
                    panel: false,
                },
            ),
            (
                6,
                Role::OverlayWindow {
                    output: O1,
                    panel: true,
                },
            ),
        ] {
            roles.windows.insert(
                win(id),
                WindowEntry {
                    facts: facts(id),
                    mapped: true,
                    output: None,
                    classification: Some(Classification {
                        kind: XKind::Toplevel,
                        role,
                        focus_on_map: FocusOnMap::None,
                    }),
                },
            );
        }
        Batch {
            index: 0,
            raw: vec![],
            before: FocusState::default(),
            roles,
            closing: vec![],
            old: vec![input(1)],
            new: vec![focus(2)],
            history: History::default(),
        }
    }

    fn role(b: Batch, old: RoleKind, parent: u32) -> Divergence {
        Divergence::Role {
            index: b.index,
            window: win(3),
            old: (old, None, None),
            new: (RoleKind::Popup, Some(win(parent)), None),
            before: b.before,
            roles: b.roles,
            closing: b.closing,
            raw: b.raw,
            history: b.history,
        }
    }

    #[test]
    fn one_batch_for_each_allowance() {
        let mut cases = Vec::new();
        let mut b = batch();
        b.before.desired = Some(win(3));
        b.raw = vec![enter(SurfaceRef::X(win(1)))];
        b.new.clear();
        cases.push((ChangeId::Rule1PopupKeepsFocus, Divergence::Focus(b)));

        let mut b = batch();
        b.old.clear();
        b.new = vec![XFocusChange::None];
        b.raw = vec![
            enter(SurfaceRef::X(win(1))),
            RawEvent::KeyboardLeave {
                target: SurfaceRef::X(win(1)),
                serial: 2,
            },
        ];
        cases.push((ChangeId::Rule2EnterThenLeave, Divergence::Focus(b)));

        let mut b = batch();
        b.before.panel = Some(win(4));
        b.before.desired = Some(win(4));
        b.old = vec![input(4)];
        b.raw = vec![press(2)];
        cases.push((ChangeId::Rule5PressFocusesPressed, Divergence::Focus(b)));

        let mut b = batch();
        b.old.clear();
        b.raw = vec![press(2)];
        cases.push((ChangeId::Rule5PressWithoutPanel, Divergence::Focus(b)));

        let mut b = batch();
        b.before.panel = Some(win(4));
        b.old = vec![input(5)];
        b.new.clear();
        b.raw = vec![enter(SurfaceRef::Overlay(O1))];
        cases.push((ChangeId::Rule7BarUnderPanel, Divergence::Focus(b)));

        let mut b = batch();
        b.old = vec![input(5)];
        b.new.clear();
        b.raw = vec![enter(SurfaceRef::Overlay(O1))];
        cases.push((ChangeId::Rule7OneShotPress, Divergence::Focus(b)));

        let mut b = batch();
        b.before.compositor = Compositor::Overlay(O1);
        b.old.clear();
        b.new = vec![focus(5)];
        b.raw = vec![press(5)];
        cases.push((ChangeId::Rule7ImmediatePress, Divergence::Focus(b)));

        let mut b = batch();
        b.old = vec![input(0)];
        b.new.clear();
        b.raw = vec![enter(SurfaceRef::Overlay(O1))];
        cases.push((ChangeId::Rule8OverlayHoverKeepsFocus, Divergence::Focus(b)));

        let mut b = batch();
        b.before.desired = Some(win(1));
        b.raw = vec![RawEvent::Unmap { window: win(1) }];
        cases.push((ChangeId::Rule9RestoreOnGone, Divergence::Focus(b)));

        let mut b = batch();
        b.closing = vec![win(1)];
        cases.push((
            ChangeId::Rule10CloseRequestKeepsReferences,
            role(b, RoleKind::Toplevel, 1),
        ));

        let mut b = batch();
        b.before.panel = Some(win(4));
        cases.push((
            ChangeId::Rule11AttachUnderPanel,
            role(b, RoleKind::OverlayPopup, 4),
        ));

        let mut b = batch();
        b.before.panel = Some(win(4));
        b.before.pending_activation = Some(win(2));
        b.raw = vec![enter(SurfaceRef::X(win(2)))];
        cases.push((
            ChangeId::Rule12ActivationReleasesPanel,
            Divergence::Focus(b),
        ));

        let mut b = batch();
        b.before.panel = Some(win(4));
        b.before.applied = Some(focus(4));
        b.new.clear();
        b.raw = vec![RawEvent::SurfaceEnterOutput {
            window: win(1),
            output: O1,
        }];
        cases.push((ChangeId::Rule13NoO1WhileNotFocused, Divergence::Focus(b)));

        let mut b = batch();
        b.raw = vec![RawEvent::MapFacts(
            facts(3)
                .types(&[super::super::model::NetWmType::Notification])
                .input(InputHint::False),
        )];
        cases.push((ChangeId::PanelPredicate, role(b, RoleKind::OverlayPopup, 1)));

        let mut b = batch();
        b.history.hints_changed.push(win(2));
        cases.push((ChangeId::HintsChangedAfterMap, Divergence::Focus(b)));

        let mut b = batch();
        b.roles
            .windows
            .get_mut(&win(1))
            .unwrap()
            .facts
            .override_redirect = true;
        b.new.clear();
        b.raw = vec![press(1)];
        cases.push((ChangeId::Smell5OverrideRedirectPress, Divergence::Focus(b)));

        let mut b = batch();
        b.old = vec![FocusCall::TakeFocus(win(1))];
        b.new = vec![focus(1)];
        cases.push((ChangeId::Q1RestoreMethod, Divergence::Focus(b)));

        assert_eq!(cases.iter().map(|(id, _)| *id).collect::<Vec<_>>(), CHANGES);
        for (id, d) in cases {
            assert!(explains(id, &d), "{id:?}: {d:#?}");
            assert_eq!(allowed(&d), Some(id), "{id:?}: {d:#?}");
        }
        assert_eq!(allowed(&Divergence::Focus(batch())), None);
    }

    #[test]
    fn allowances_reject_nearby_unexplained_changes() {
        let mut cases = Vec::new();
        let mut b = batch();
        b.before.desired = Some(win(3));
        b.raw = vec![enter(SurfaceRef::X(win(2)))];
        b.old = vec![input(2)];
        b.new.clear();
        cases.push(Divergence::Focus(b));

        let mut b = batch();
        b.old.clear();
        b.new = vec![XFocusChange::None];
        b.raw = vec![
            enter(SurfaceRef::X(win(1))),
            RawEvent::KeyboardLeave {
                target: SurfaceRef::X(win(1)),
                serial: 2,
            },
            enter(SurfaceRef::X(win(2))),
        ];
        cases.push(Divergence::Focus(b));

        let mut b = batch();
        b.old = vec![input(5)];
        b.new.clear();
        b.raw = vec![press(5), enter(SurfaceRef::Overlay(O1))];
        cases.push(Divergence::Focus(b));

        let mut b = batch();
        b.before.pending_press = Some((win(5), O1));
        b.old = vec![input(5)];
        b.new.clear();
        b.raw = vec![enter(SurfaceRef::Overlay(O1))];
        cases.push(Divergence::Focus(b));

        let mut b = batch();
        b.before.pending_press = Some((win(5), O1));
        b.old = vec![input(0)];
        b.new.clear();
        b.raw = vec![enter(SurfaceRef::Overlay(O1))];
        cases.push(Divergence::Focus(b));

        let mut b = batch();
        b.before.desired = Some(win(1));
        b.raw = vec![RawEvent::Destroy { window: win(3) }];
        cases.push(Divergence::Focus(b));

        let mut b = batch();
        b.closing = vec![win(2)];
        cases.push(role(b, RoleKind::Toplevel, 1));

        let mut b = batch();
        b.before.panel = Some(win(4));
        cases.push(role(b, RoleKind::Toplevel, 1));

        let mut b = batch();
        b.before.pending_activation = Some(win(1));
        b.raw = vec![enter(SurfaceRef::X(win(2)))];
        cases.push(Divergence::Focus(b));

        let mut b = batch();
        b.before.applied = Some(focus(1));
        b.new.clear();
        b.raw = vec![RawEvent::SurfaceEnterOutput {
            window: win(1),
            output: O1,
        }];
        cases.push(Divergence::Focus(b));

        let mut b = batch();
        b.raw = vec![RawEvent::MapFacts(facts(3).input(InputHint::False))];
        cases.push(role(b, RoleKind::Toplevel, 1));

        let mut b = batch();
        b.history.hints_changed = vec![win(3)];
        cases.push(Divergence::Focus(b));

        let mut b = batch();
        b.roles
            .windows
            .get_mut(&win(1))
            .unwrap()
            .facts
            .override_redirect = true;
        b.old = vec![FocusCall::TakeFocus(win(1))];
        cases.push(Divergence::Focus(b));

        let mut b = batch();
        b.old = vec![FocusCall::TakeFocus(win(3))];
        b.new = vec![focus(3)];
        cases.push(Divergence::Focus(b));

        for d in cases {
            assert_eq!(allowed(&d), None, "{d:#?}");
        }
    }

    #[test]
    fn press_allowances_keep_family_target_and_first_match_boundaries() {
        let mut b = batch();
        b.before.panel = Some(win(4));
        b.raw = vec![press(4)];
        b.new = vec![focus(4)];
        assert_eq!(allowed(&Divergence::Focus(b)), None);

        let mut b = batch();
        b.old.clear();
        b.raw = vec![press(1)];
        assert_eq!(allowed(&Divergence::Focus(b)), None);

        let mut b = batch();
        b.before.panel = Some(win(4));
        b.raw = vec![RawEvent::Press {
            target: PressRef::Decoration(win(2)),
            serial: 1,
            touch: false,
        }];
        assert_eq!(
            allowed(&Divergence::Focus(b)),
            Some(ChangeId::Rule5PressFocusesPressed)
        );

        let mut b = batch();
        b.before.panel = Some(win(4));
        b.before.compositor = Compositor::X(win(2));
        b.raw = vec![press(3)];
        assert_eq!(
            allowed(&Divergence::Focus(b)),
            Some(ChangeId::Rule5PressFocusesPressed)
        );

        let mut b = batch();
        b.before.panel = Some(win(4));
        b.before.compositor = Compositor::Overlay(O1);
        b.old.clear();
        b.raw = vec![press(6)];
        b.new = vec![focus(6)];
        let d = Divergence::Focus(b);
        assert!(explains(ChangeId::Rule7ImmediatePress, &d));
        assert_eq!(allowed(&d), Some(ChangeId::Rule7ImmediatePress));
    }

    #[test]
    fn activation_gone_and_panel_predicate_alternatives() {
        let mut b = batch();
        b.before.compositor = Compositor::X(win(2));
        b.raw = vec![RawEvent::ActiveWindowRequest { window: win(2) }];
        assert_eq!(
            allowed(&Divergence::Focus(b)),
            Some(ChangeId::Rule12ActivationReleasesPanel)
        );

        let mut b = batch();
        b.before.applied = Some(focus(1));
        b.raw = vec![RawEvent::Destroy { window: win(1) }];
        assert_eq!(
            allowed(&Divergence::Focus(b)),
            Some(ChangeId::Rule9RestoreOnGone)
        );

        let mut b = batch();
        let mut call_bar = facts(3).no_decor().keep_above();
        call_bar.size_hints = Some(super::super::model::SizeHints {
            position: true,
            ..Default::default()
        });
        b.raw = vec![RawEvent::MapFacts(call_bar)];
        assert_eq!(
            allowed(&role(b, RoleKind::Toplevel, 1)),
            Some(ChangeId::PanelPredicate)
        );

        let mut b = batch();
        b.raw = vec![
            RawEvent::MapFacts(
                facts(2)
                    .types(&[super::super::model::NetWmType::Notification])
                    .input(InputHint::False),
            ),
            RawEvent::PopupFirstConfigure { window: win(2) },
        ];
        assert_eq!(
            allowed(&Divergence::Focus(b)),
            Some(ChangeId::PanelPredicate)
        );
    }
}

#[cfg(test)]
mod replay_tests {
    use super::super::model::testkit::{facts, win};
    use super::super::trace_codec::{encode, role_line};
    use super::*;

    fn raw(trace: &mut String, t: u64, event: RawEvent) {
        *trace += &encode(t, &event);
        trace.push('\n');
    }

    fn mapped(trace: &mut String, t: u64, facts: WindowFacts) {
        let event = RawEvent::RoleCreate {
            window: facts.window,
            dims: facts.dims,
        };
        raw(trace, t, RawEvent::MapFacts(facts));
        raw(trace, t, event);
    }

    #[test]
    fn mapping_and_last_call_granularity() {
        let input = |w| FocusCall::SetInput {
            window: win(w),
            output: Some("ignored".into()),
        };
        let focus = |w, method| XFocusChange::Window {
            window: win(w),
            method,
            primary_output: Some(OutputId(99)),
        };
        assert!(same(&input(0), &XFocusChange::None));
        assert!(same(&input(1), &focus(1, Method::SetInput)));
        assert!(same(
            &FocusCall::TakeFocus(win(1)),
            &focus(1, Method::TakeFocus)
        ));
        assert!(!same(&input(1), &focus(2, Method::SetInput)));
        assert!(!same(&input(1), &focus(1, Method::TakeFocus)));
        assert!(!same(
            &FocusCall::TakeFocus(win(1)),
            &focus(1, Method::SetInput)
        ));
        assert!(!same(&input(1), &XFocusChange::None));
        let mut b = Batch {
            index: 0,
            raw: vec![],
            before: FocusState::default(),
            roles: RoleTable::default(),
            closing: vec![],
            old: vec![input(2), input(1)],
            new: vec![focus(1, Method::SetInput)],
            history: History::default(),
        };
        assert!(batch_agrees(&b));
        b.new.clear();
        assert!(!batch_agrees(&b));
        b.old.clear();
        assert!(batch_agrees(&b));
        b.new.push(XFocusChange::None);
        assert!(!batch_agrees(&b));
    }

    #[test]
    fn decode_errors_are_counted_and_later_events_still_replay() {
        let mut trace =
            "{\"t\":0,\"k\":\"kb_enter\",\"tk\":\"overlay\",\"id\":null,\"serial\":1}\nnot json\n"
                .to_owned();
        raw(&mut trace, 1, RawEvent::BatchEnd);
        mapped(&mut trace, 2, facts(1));
        raw(
            &mut trace,
            3,
            RawEvent::KeyboardEnter {
                target: SurfaceRef::X(win(1)),
                serial: 1,
            },
        );
        raw(&mut trace, 4, RawEvent::BatchEnd);
        let report = replay_with_report(&trace);
        assert_eq!(report.undecodable, 2);
        assert_eq!(
            report
                .decode_errors
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(report.batches, 2);
        assert!(
            matches!(&report.divergences[..], [Divergence::Focus(b)] if b.index == 1 && b.old.is_empty() && b.new.len() == 1)
        );
        assert!(!report.unfinished_batch);
        assert_eq!(allowed(&report.divergences[0]), None);
    }

    #[test]
    fn decode_diagnostics_are_bounded_but_the_count_is_not() {
        let report = replay_with_report(&"invalid\n".repeat(25));
        assert_eq!(report.undecodable, 25);
        assert_eq!(report.decode_errors.len(), 20);
    }

    #[test]
    fn tail_is_reported_without_inventing_a_batch() {
        let mut trace = String::new();
        mapped(&mut trace, 0, facts(1));
        raw(
            &mut trace,
            1,
            RawEvent::KeyboardEnter {
                target: SurfaceRef::X(win(1)),
                serial: 1,
            },
        );
        let report = replay_with_report(&trace);
        assert!(report.unfinished_batch);
        assert_eq!(report.batches, 0);
        assert!(report.divergences.is_empty());
    }

    #[test]
    fn post_map_hints_survive_batches_but_not_remapping() {
        let mut trace = String::new();
        mapped(&mut trace, 0, facts(1));
        raw(
            &mut trace,
            1,
            RawEvent::HintsChanged {
                window: win(1),
                input: InputHint::False,
            },
        );
        raw(&mut trace, 2, RawEvent::BatchEnd);
        let role = role_line(3, win(1), (RoleKind::Popup, Some(win(2)), None), false) + "\n";
        trace += &role;
        let first = replay(&trace);
        assert_eq!(allowed(&first[0]), Some(ChangeId::HintsChangedAfterMap));
        raw(&mut trace, 4, RawEvent::Unmap { window: win(1) });
        mapped(&mut trace, 5, facts(1));
        trace += &role;
        let remapped = replay(&trace);
        assert_eq!(allowed(&remapped[1]), None);
        raw(&mut trace, 6, RawEvent::BatchEnd);
        trace += &role;
        assert_eq!(allowed(&replay(&trace)[2]), None);
    }

    #[test]
    fn hints_before_map_or_in_the_current_batch_do_not_match() {
        let mut trace = String::new();
        raw(
            &mut trace,
            0,
            RawEvent::HintsChanged {
                window: win(1),
                input: InputHint::False,
            },
        );
        mapped(&mut trace, 1, facts(1));
        raw(&mut trace, 2, RawEvent::BatchEnd);
        raw(
            &mut trace,
            3,
            RawEvent::HintsChanged {
                window: win(1),
                input: InputHint::True,
            },
        );
        trace += &role_line(4, win(1), (RoleKind::Popup, Some(win(2)), None), false);
        assert_eq!(allowed(&replay(&trace)[0]), None);
    }

    #[test]
    fn client_mask_allowance_requires_a_live_recent_anchor() {
        for (at, expected) in [(10, Some(ChangeId::PanelPredicate)), (1600, None)] {
            let mut trace = String::new();
            mapped(&mut trace, 0, facts(0x200001));
            raw(
                &mut trace,
                0,
                RawEvent::Press {
                    target: PressRef::X(win(0x200001)),
                    serial: 1,
                    touch: false,
                },
            );
            raw(&mut trace, 0, RawEvent::BatchEnd);
            let mut notification =
                facts(0x300001).types(&[super::super::model::NetWmType::Notification]);
            notification.client = 0x300000;
            mapped(&mut trace, at, notification);
            trace += &role_line(
                at,
                win(0x300001),
                (RoleKind::Popup, Some(win(0x200001)), None),
                false,
            );
            let divergences = replay(&trace);
            let d = divergences
                .iter()
                .find(|d| matches!(d, Divergence::Role { .. }))
                .unwrap();
            assert_eq!(allowed(d), expected, "{d:#?}");
        }
    }

    #[test]
    fn close_requests_are_kept_until_gone_and_id_reuse_is_not_closing() {
        for gone in [
            RawEvent::Unmap { window: win(1) },
            RawEvent::Destroy { window: win(1) },
        ] {
            let mut trace = String::new();
            mapped(&mut trace, 0, facts(1));
            raw(&mut trace, 1, RawEvent::PointerEnter { window: win(1) });
            raw(&mut trace, 2, RawEvent::BatchEnd);
            trace += "{\"t\":3,\"k\":\"x_call\",\"call\":\"close_window\",\"w\":1}\n";
            mapped(&mut trace, 4, facts(2).or());
            trace += &(role_line(5, win(2), (RoleKind::Toplevel, None, None), false) + "\n");
            assert_eq!(
                allowed(&replay(&trace)[0]),
                Some(ChangeId::Rule10CloseRequestKeepsReferences)
            );
            raw(&mut trace, 6, gone);
            mapped(&mut trace, 7, facts(1));
            raw(&mut trace, 8, RawEvent::PointerEnter { window: win(1) });
            mapped(&mut trace, 9, facts(3).or());
            trace += &role_line(10, win(3), (RoleKind::Toplevel, None, None), false);
            assert_eq!(allowed(&replay(&trace)[1]), None);
        }
    }

    #[test]
    fn role_mapping_compares_parent_and_output_and_ignores_rehomes() {
        let mut trace = String::new();
        mapped(&mut trace, 0, facts(1));
        trace += &(role_line(0, win(1), (RoleKind::Toplevel, None, None), false) + "\n");
        raw(&mut trace, 1, RawEvent::PointerEnter { window: win(1) });
        mapped(&mut trace, 2, facts(2).or());
        trace += &(role_line(3, win(2), (RoleKind::Popup, Some(win(1)), None), false) + "\n");
        trace += &(role_line(4, win(2), (RoleKind::Popup, Some(win(99)), None), false) + "\n");
        trace += &(role_line(
            5,
            win(2),
            (RoleKind::Popup, Some(win(1)), Some(OutputId(99))),
            false,
        ) + "\n");
        trace += &role_line(
            6,
            win(99),
            (RoleKind::OverlayPopup, None, Some(OutputId(99))),
            true,
        );
        let report = replay_with_report(&trace);
        assert_eq!(report.divergences.len(), 2);
        assert!(
            report
                .divergences
                .iter()
                .all(|d| matches!(d, Divergence::Role { window, .. } if *window == win(2)))
        );
    }
}
