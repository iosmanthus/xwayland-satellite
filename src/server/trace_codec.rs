// encode runs only with the trace feature, decode only in the replay tests.
#![allow(dead_code)]

use super::model::*;
use crate::jsonl::{Line, Value, parse_object};
use crate::xstate::{WindowDims, WindowRole};
use xcb::{Xid, XidNew, x};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Start,
    Raw {
        t: Millis,
        event: RawEvent,
    },
    FocusCall {
        t: Millis,
        call: FocusCall,
    },
    CloseCall {
        t: Millis,
        window: x::Window,
    },
    OtherCall,
    Role {
        t: Millis,
        window: x::Window,
        kind: RoleKind,
        parent: Option<x::Window>,
        output: Option<OutputId>,
        rehome: bool,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusCall {
    SetInput {
        window: x::Window,
        output: Option<String>,
    },
    TakeFocus(x::Window),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    Toplevel,
    FixedToplevel,
    Fullscreen,
    Popup,
    OverlayPopup,
}

pub fn encode(t: Millis, raw: &RawEvent) -> String {
    let kind = match raw {
        RawEvent::MapFacts(_) => "map",
        RawEvent::HintsChanged { .. } => "hints",
        RawEvent::RoleCreate { .. } => "role_create",
        RawEvent::Unmap { .. } => "unmap",
        RawEvent::Destroy { .. } => "destroy",
        RawEvent::ActiveWindowRequest { .. } => "active_req",
        RawEvent::KeyboardEnter { .. } => "kb_enter",
        RawEvent::KeyboardLeave { .. } => "kb_leave",
        RawEvent::Key { .. } => "key",
        RawEvent::Press { .. } => "press",
        RawEvent::PointerEnter { .. } => "ptr_enter",
        RawEvent::PopupFirstConfigure { .. } => "popup_configure",
        RawEvent::PopupDone { .. } => "popup_done",
        RawEvent::SurfaceEnterOutput { .. } => "surface_output",
        RawEvent::OutputGeometry { .. } => "output",
        RawEvent::OutputRemoved { .. } => "output_removed",
        RawEvent::OverlayMapped { .. } => "overlay_mapped",
        RawEvent::OverlayClosed { .. } => "overlay_closed",
        RawEvent::OverlayRehome { .. } => "overlay_rehome",
        RawEvent::ActivationTokenDone { .. } => "token_done",
        RawEvent::BatchEnd => "batch_end",
    };
    let mut line = Line::new(t, kind);
    match raw {
        RawEvent::MapFacts(f) => {
            let types: Vec<_> = f.types.iter().map(type_name).collect();
            let min = f.size_hints.and_then(|h| h.min).map(pair);
            let max = f.size_hints.and_then(|h| h.max).map(pair);
            line.u("w", window_id(f.window))
                .b("or", f.override_redirect)
                .strs("types", &types)
                .opt_u("motif_f", f.motif.functions.map(u64::from))
                .opt_u("motif_d", f.motif.decorations.map(u64::from))
                .opt_s("class", f.class.as_deref())
                .opt_ints("min", min.as_ref().map(|v| v.as_slice()))
                .opt_ints("max", max.as_ref().map(|v| v.as_slice()))
                .b("pos", f.size_hints.is_some_and(|h| h.position))
                .s("input", input_name(f.input))
                .b("above", f.keep_above)
                .opt_u("transient", f.transient_for.map(window_id))
                .b("take_focus", f.take_focus)
                .b("delete", f.delete)
                .ints("geom", &geometry(f.dims))
                .u("client", f.client.into())
                .s("guess", &format!("{:?}", f.guessed));
        }
        RawEvent::HintsChanged { window, input } => {
            line.u("w", window_id(*window))
                .s("input", input_name(*input));
        }
        RawEvent::RoleCreate { window, dims } => {
            line.u("w", window_id(*window))
                .ints("geom", &geometry(*dims));
        }
        RawEvent::Unmap { window }
        | RawEvent::ActiveWindowRequest { window }
        | RawEvent::PointerEnter { window }
        | RawEvent::PopupFirstConfigure { window }
        | RawEvent::PopupDone { window }
        | RawEvent::ActivationTokenDone { window } => {
            line.u("w", window_id(*window));
        }
        RawEvent::Destroy { window } => {
            line.u("w", window_id(*window)).b("reparent", false);
        }
        RawEvent::KeyboardEnter { target, serial } | RawEvent::KeyboardLeave { target, serial } => {
            let (kind, id) = match target {
                SurfaceRef::X(w) => ("x", Some(window_id(*w))),
                SurfaceRef::Overlay(o) => ("overlay", Some(o.0.into())),
                SurfaceRef::Other => ("other", None),
            };
            line.s("tk", kind)
                .opt_u("id", id)
                .u("serial", (*serial).into());
        }
        RawEvent::Key { pressed, serial } => {
            line.b("pressed", *pressed).u("serial", (*serial).into());
        }
        RawEvent::Press {
            target,
            serial,
            touch,
        } => {
            let (kind, id) = match target {
                PressRef::X(w) => ("x", Some(window_id(*w))),
                PressRef::Decoration(w) => ("decoration", Some(window_id(*w))),
                PressRef::Overlay(o) => ("overlay", Some(o.0.into())),
                PressRef::Other => ("other", None),
            };
            line.s("tk", kind)
                .opt_u("id", id)
                .u("serial", (*serial).into())
                .b("touch", *touch);
        }
        RawEvent::SurfaceEnterOutput { window, output }
        | RawEvent::OverlayRehome { window, output } => {
            line.u("w", window_id(*window)).u("o", output.0.into());
        }
        RawEvent::OutputGeometry {
            output,
            x_rect,
            mode,
        } => {
            line.u("o", output.0.into())
                .ints(
                    "rect",
                    &[
                        x_rect.x.into(),
                        x_rect.y.into(),
                        x_rect.width.into(),
                        x_rect.height.into(),
                    ],
                )
                .ints("mode", &pair(*mode));
        }
        RawEvent::OutputRemoved { output }
        | RawEvent::OverlayMapped { output }
        | RawEvent::OverlayClosed { output } => {
            line.u("o", output.0.into());
        }
        RawEvent::BatchEnd => {}
    }
    line.finish()
}

pub fn decode(line: &str) -> Result<Record, String> {
    let fields = parse_object(line)?;
    let f = fields.as_slice();
    let t = get_u(f, "t")?;
    let event =
        match get_s(f, "k")? {
            "start" => {
                get_s(f, "version")?;
                get_u32(f, "pid")?;
                return Ok(Record::Start);
            }
            "map" => {
                let min = get_opt_pair(f, "min")?;
                let max = get_opt_pair(f, "max")?;
                let position = get_b(f, "pos")?;
                let size_hints = (min.is_some() || max.is_some() || position)
                    .then_some(SizeHints { min, max, position });
                let guessed = match get_s(f, "guess")? {
                    "Toplevel" | "Notification" => WindowRole::Toplevel,
                    "Popup" => WindowRole::Popup,
                    "Splash" => WindowRole::Splash,
                    _ => return Err("invalid guess".into()),
                };
                RawEvent::MapFacts(WindowFacts {
                    window: get_window(f, "w")?,
                    override_redirect: get_b(f, "or")?,
                    types: get_types(f)?,
                    motif: MotifHints {
                        functions: get_opt_u32(f, "motif_f")?,
                        decorations: get_opt_u32(f, "motif_d")?,
                    },
                    class: get_opt_s(f, "class")?,
                    size_hints,
                    input: get_input(f)?,
                    keep_above: get_b(f, "above")?,
                    transient_for: get_opt_u32(f, "transient")?.map(window),
                    take_focus: get_b(f, "take_focus")?,
                    delete: get_b(f, "delete")?,
                    dims: get_dims(f, "geom")?,
                    client: get_u32(f, "client")?,
                    guessed,
                })
            }
            "hints" => RawEvent::HintsChanged {
                window: get_window(f, "w")?,
                input: get_input(f)?,
            },
            "role_create" => RawEvent::RoleCreate {
                window: get_window(f, "w")?,
                dims: get_dims(f, "geom")?,
            },
            "unmap" => RawEvent::Unmap {
                window: get_window(f, "w")?,
            },
            "destroy" => {
                get_b(f, "reparent")?;
                RawEvent::Destroy {
                    window: get_window(f, "w")?,
                }
            }
            "active_req" => RawEvent::ActiveWindowRequest {
                window: get_window(f, "w")?,
            },
            "kb_enter" => RawEvent::KeyboardEnter {
                target: get_surface(f)?,
                serial: get_u32(f, "serial")?,
            },
            "kb_leave" => RawEvent::KeyboardLeave {
                target: get_surface(f)?,
                serial: get_u32(f, "serial")?,
            },
            "key" => RawEvent::Key {
                pressed: get_b(f, "pressed")?,
                serial: get_u32(f, "serial")?,
            },
            "press" => RawEvent::Press {
                target: get_press(f)?,
                serial: get_u32(f, "serial")?,
                touch: get_b(f, "touch")?,
            },
            "ptr_enter" => RawEvent::PointerEnter {
                window: get_window(f, "w")?,
            },
            "popup_configure" => RawEvent::PopupFirstConfigure {
                window: get_window(f, "w")?,
            },
            "popup_done" => RawEvent::PopupDone {
                window: get_window(f, "w")?,
            },
            "surface_output" => RawEvent::SurfaceEnterOutput {
                window: get_window(f, "w")?,
                output: OutputId(get_u32(f, "o")?),
            },
            "output" => {
                let [x, y, width, height] = get_ints(f, "rect")?;
                let [mw, mh] = get_ints(f, "mode")?;
                RawEvent::OutputGeometry {
                    output: OutputId(get_u32(f, "o")?),
                    x_rect: XRect {
                        x: number(x, "rect")?,
                        y: number(y, "rect")?,
                        width: number(width, "rect")?,
                        height: number(height, "rect")?,
                    },
                    mode: (number(mw, "mode")?, number(mh, "mode")?),
                }
            }
            "output_removed" => RawEvent::OutputRemoved {
                output: OutputId(get_u32(f, "o")?),
            },
            "overlay_mapped" => RawEvent::OverlayMapped {
                output: OutputId(get_u32(f, "o")?),
            },
            "overlay_closed" => RawEvent::OverlayClosed {
                output: OutputId(get_u32(f, "o")?),
            },
            "overlay_rehome" => RawEvent::OverlayRehome {
                window: get_window(f, "w")?,
                output: OutputId(get_u32(f, "o")?),
            },
            "token_done" => RawEvent::ActivationTokenDone {
                window: get_window(f, "w")?,
            },
            "batch_end" => RawEvent::BatchEnd,
            "x_call" => {
                let call = get_s(f, "call")?;
                let window = get_window(f, "w")?;
                return Ok(match call {
                    "focus_window" => Record::FocusCall {
                        t,
                        call: FocusCall::SetInput {
                            window,
                            output: get_opt_s(f, "output")?,
                        },
                    },
                    "send_take_focus" => Record::FocusCall {
                        t,
                        call: FocusCall::TakeFocus(window),
                    },
                    "close_window" => Record::CloseCall { t, window },
                    "set_fullscreen" => {
                        get_b(f, "on")?;
                        Record::OtherCall
                    }
                    "set_window_dims" => {
                        let rect: [i64; 4] = get_ints(f, "rect")?;
                        for value in rect {
                            number::<i32>(value, "rect")?;
                        }
                        Record::OtherCall
                    }
                    _ => Record::OtherCall,
                });
            }
            "role" => {
                let kind = match get_s(f, "kind")? {
                    "toplevel" => RoleKind::Toplevel,
                    "fixed_toplevel" => RoleKind::FixedToplevel,
                    "fullscreen" => RoleKind::Fullscreen,
                    "popup" => RoleKind::Popup,
                    "overlay_popup" => RoleKind::OverlayPopup,
                    _ => return Err("invalid kind".into()),
                };
                let rehome = match get_s(f, "why")? {
                    "create" => false,
                    "rehome" => true,
                    _ => return Err("invalid why".into()),
                };
                return Ok(Record::Role {
                    t,
                    window: get_window(f, "w")?,
                    kind,
                    parent: get_opt_u32(f, "parent")?.map(window),
                    output: get_opt_u32(f, "o")?.map(OutputId),
                    rehome,
                });
            }
            _ => return Ok(Record::Unknown),
        };
    Ok(Record::Raw { t, event })
}

pub fn role_object(
    c: &Classification,
    roles: &RoleTable,
) -> (RoleKind, Option<x::Window>, Option<OutputId>) {
    match c.role {
        Role::Toplevel {
            fixed_size: false, ..
        } => (RoleKind::Toplevel, None, None),
        Role::Toplevel {
            fixed_size: true, ..
        } => (RoleKind::FixedToplevel, None, None),
        Role::FullscreenToplevel { .. } => (RoleKind::Fullscreen, None, None),
        Role::Popup { parent } | Role::PanelOf { parent } => (RoleKind::Popup, Some(parent), None),
        Role::OverlayWindow { output, .. } => (RoleKind::OverlayPopup, None, Some(output)),
        Role::OverlayPopupOf { panel } => match roles.classification(panel).map(|c| c.role) {
            Some(Role::OverlayWindow { output, .. }) => {
                (RoleKind::OverlayPopup, None, Some(output))
            }
            _ => (RoleKind::Popup, Some(panel), None),
        },
    }
}

pub fn role_line(
    t: Millis,
    window: x::Window,
    object: (RoleKind, Option<x::Window>, Option<OutputId>),
    rehome: bool,
) -> String {
    let (kind, parent, output) = object;
    let kind = match kind {
        RoleKind::Toplevel => "toplevel",
        RoleKind::FixedToplevel => "fixed_toplevel",
        RoleKind::Fullscreen => "fullscreen",
        RoleKind::Popup => "popup",
        RoleKind::OverlayPopup => "overlay_popup",
    };
    let mut line = Line::new(t, "role");
    line.u("w", window_id(window))
        .s("kind", kind)
        .opt_u("parent", parent.map(window_id))
        .opt_u("o", output.map(|o| o.0.into()))
        .s("why", if rehome { "rehome" } else { "create" });
    line.finish()
}

fn window_id(window: x::Window) -> u64 {
    window.resource_id().into()
}

fn window(id: u32) -> x::Window {
    x::Window::new(id)
}

fn pair((a, b): (i32, i32)) -> [i64; 2] {
    [a.into(), b.into()]
}

fn geometry(dims: WindowDims) -> [i64; 4] {
    [
        dims.x.into(),
        dims.y.into(),
        dims.width.into(),
        dims.height.into(),
    ]
}

fn input_name(input: InputHint) -> &'static str {
    match input {
        InputHint::Absent => "absent",
        InputHint::True => "true",
        InputHint::False => "false",
    }
}

fn type_name(kind: &NetWmType) -> &'static str {
    match kind {
        NetWmType::Normal => "NORMAL",
        NetWmType::Dialog => "DIALOG",
        NetWmType::Utility => "UTILITY",
        NetWmType::Splash => "SPLASH",
        NetWmType::Menu => "MENU",
        NetWmType::PopupMenu => "POPUP_MENU",
        NetWmType::DropdownMenu => "DROPDOWN_MENU",
        NetWmType::Tooltip => "TOOLTIP",
        NetWmType::Dnd => "DND",
        NetWmType::Combo => "COMBO",
        NetWmType::Notification => "NOTIFICATION",
        NetWmType::Other => "OTHER",
    }
}

type Fields = [(String, Value)];

fn get<'a>(fields: &'a Fields, key: &str) -> Result<&'a Value, String> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .ok_or_else(|| format!("missing {key}"))
}

fn number<T: TryFrom<i64>>(value: i64, key: &str) -> Result<T, String> {
    value.try_into().map_err(|_| format!("out of range {key}"))
}

fn get_u(fields: &Fields, key: &str) -> Result<u64, String> {
    match get(fields, key)? {
        Value::Int(value) => number(*value, key),
        _ => Err(format!("expected unsigned integer {key}")),
    }
}

fn get_u32(fields: &Fields, key: &str) -> Result<u32, String> {
    get_u(fields, key)?
        .try_into()
        .map_err(|_| format!("out of range {key}"))
}

fn get_opt_u32(fields: &Fields, key: &str) -> Result<Option<u32>, String> {
    match get(fields, key)? {
        Value::Null => Ok(None),
        _ => get_u32(fields, key).map(Some),
    }
}

fn get_window(fields: &Fields, key: &str) -> Result<x::Window, String> {
    get_u32(fields, key).map(window)
}

fn get_s<'a>(fields: &'a Fields, key: &str) -> Result<&'a str, String> {
    match get(fields, key)? {
        Value::Str(value) => Ok(value),
        _ => Err(format!("expected string {key}")),
    }
}

fn get_opt_s(fields: &Fields, key: &str) -> Result<Option<String>, String> {
    match get(fields, key)? {
        Value::Null => Ok(None),
        _ => get_s(fields, key).map(|s| Some(s.into())),
    }
}

fn get_b(fields: &Fields, key: &str) -> Result<bool, String> {
    match get(fields, key)? {
        Value::Bool(value) => Ok(*value),
        _ => Err(format!("expected boolean {key}")),
    }
}

fn get_ints<const N: usize>(fields: &Fields, key: &str) -> Result<[i64; N], String> {
    match get(fields, key)? {
        Value::Ints(values) => values
            .as_slice()
            .try_into()
            .map_err(|_| format!("expected {N} integers in {key}")),
        _ => Err(format!("expected integer array {key}")),
    }
}

fn get_opt_pair(fields: &Fields, key: &str) -> Result<Option<(i32, i32)>, String> {
    if matches!(get(fields, key)?, Value::Null) {
        return Ok(None);
    }
    let [a, b] = get_ints(fields, key)?;
    Ok(Some((number(a, key)?, number(b, key)?)))
}

fn get_dims(fields: &Fields, key: &str) -> Result<WindowDims, String> {
    let [x, y, width, height] = get_ints(fields, key)?;
    Ok(WindowDims {
        x: number(x, key)?,
        y: number(y, key)?,
        width: number(width, key)?,
        height: number(height, key)?,
    })
}

fn get_input(fields: &Fields) -> Result<InputHint, String> {
    match get_s(fields, "input")? {
        "absent" => Ok(InputHint::Absent),
        "true" => Ok(InputHint::True),
        "false" => Ok(InputHint::False),
        _ => Err("invalid input".into()),
    }
}

fn get_types(fields: &Fields) -> Result<Vec<NetWmType>, String> {
    let values = match get(fields, "types")? {
        Value::Strs(values) => values,
        Value::Ints(values) if values.is_empty() => return Ok(Vec::new()),
        _ => return Err("expected string array types".into()),
    };
    values
        .iter()
        .map(|s| {
            Ok(match s.as_str() {
                "NORMAL" => NetWmType::Normal,
                "DIALOG" => NetWmType::Dialog,
                "UTILITY" => NetWmType::Utility,
                "SPLASH" => NetWmType::Splash,
                "MENU" => NetWmType::Menu,
                "POPUP_MENU" => NetWmType::PopupMenu,
                "DROPDOWN_MENU" => NetWmType::DropdownMenu,
                "TOOLTIP" => NetWmType::Tooltip,
                "DND" => NetWmType::Dnd,
                "COMBO" => NetWmType::Combo,
                "NOTIFICATION" => NetWmType::Notification,
                "OTHER" => NetWmType::Other,
                _ => return Err("invalid types entry".into()),
            })
        })
        .collect()
}

fn get_surface(fields: &Fields) -> Result<SurfaceRef, String> {
    match get_s(fields, "tk")? {
        "x" => Ok(SurfaceRef::X(get_window(fields, "id")?)),
        "overlay" => Ok(SurfaceRef::Overlay(OutputId(get_u32(fields, "id")?))),
        "other" if matches!(get(fields, "id")?, Value::Null) => Ok(SurfaceRef::Other),
        "other" => Err("expected null id".into()),
        _ => Err("invalid tk".into()),
    }
}

fn get_press(fields: &Fields) -> Result<PressRef, String> {
    if get_s(fields, "tk")? == "decoration" {
        return Ok(PressRef::Decoration(get_window(fields, "id")?));
    }
    Ok(match get_surface(fields)? {
        SurfaceRef::X(w) => PressRef::X(w),
        SurfaceRef::Overlay(o) => PressRef::Overlay(o),
        SurfaceRef::Other => PressRef::Other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::model::testkit::*;
    use crate::xstate::WindowRole;

    #[test]
    fn known_records_require_valid_fields() {
        for (line, field) in [
            (r#"{"k":"batch_end"}"#, "t"),
            (r#"{"t":0,"k":"unmap"}"#, "w"),
            (r#"{"t":0,"k":"unmap","w":-1}"#, "w"),
            (r#"{"t":0,"k":"unmap","w":4294967296}"#, "w"),
            (r#"{"t":0,"k":"destroy","w":1,"reparent":null}"#, "reparent"),
            (r#"{"t":0,"k":"role_create","w":1,"geom":[1,2,3]}"#, "geom"),
            (
                r#"{"t":0,"k":"role_create","w":1,"geom":[32768,0,1,1]}"#,
                "geom",
            ),
            (r#"{"t":0,"k":"hints","w":1,"input":"yes"}"#, "input"),
            (
                r#"{"t":0,"k":"kb_enter","tk":"other","id":1,"serial":1}"#,
                "id",
            ),
            (
                r#"{"t":0,"k":"kb_enter","tk":"decoration","id":1,"serial":1}"#,
                "tk",
            ),
            (
                r#"{"t":0,"k":"x_call","call":"focus_window","w":1}"#,
                "output",
            ),
            (
                r#"{"t":0,"k":"x_call","call":"set_fullscreen","w":1,"on":1}"#,
                "on",
            ),
            (
                r#"{"t":0,"k":"x_call","call":"set_window_dims","w":1,"rect":null}"#,
                "rect",
            ),
            (r#"{"t":0,"k":"x_call","call":"raise_to_top"}"#, "w"),
            (r#"{"t":0,"k":"start","pid":1}"#, "version"),
        ] {
            let error = decode(line).expect_err(line);
            assert!(error.contains(field), "{field}: {error}");
        }
    }

    #[test]
    fn role_lines_round_trip() {
        for kind in [
            RoleKind::Toplevel,
            RoleKind::FixedToplevel,
            RoleKind::Fullscreen,
            RoleKind::Popup,
            RoleKind::OverlayPopup,
        ] {
            let parent = (kind == RoleKind::Popup).then_some(win(A | 2));
            let output = (kind == RoleKind::OverlayPopup).then_some(O2);
            for rehome in [false, true] {
                let line = role_line(12, win(A | 1), (kind, parent, output), rehome);
                assert_eq!(
                    decode(&line),
                    Ok(Record::Role {
                        t: 12,
                        window: win(A | 1),
                        kind,
                        parent,
                        output,
                        rehome
                    })
                );
            }
        }
    }

    #[test]
    #[ignore]
    fn decode_a_trace_file() {
        use std::io::BufRead;
        let path = std::env::var_os("XWLS_TRACE_CHECK").expect("set XWLS_TRACE_CHECK");
        let file = std::fs::File::open(path).unwrap();
        for (index, line) in std::io::BufReader::new(file).lines().enumerate() {
            let line = line.unwrap();
            assert!(
                decode(&line).is_ok(),
                "line {}: {:?}",
                index + 1,
                decode(&line)
            );
        }
    }

    fn every_raw_event() -> Vec<RawEvent> {
        let f = facts(A | 1)
            .types(&[NetWmType::Notification, NetWmType::Other])
            .transient(A | 2)
            .no_decor()
            .functions_none()
            .min(10, 20)
            .max(30, 40)
            .position()
            .input(InputHint::False)
            .keep_above()
            .take_focus()
            .class("Meeting \"x\"")
            .at(-5, 6, 70, 80)
            .guessed(WindowRole::Popup);
        let w = win(A | 1);
        vec![
            RawEvent::MapFacts(f),
            RawEvent::MapFacts(facts(A | 3)),
            RawEvent::HintsChanged {
                window: w,
                input: InputHint::True,
            },
            RawEvent::RoleCreate {
                window: w,
                dims: dims(1, 2, 3, 4),
            },
            RawEvent::Unmap { window: w },
            RawEvent::Destroy { window: w },
            RawEvent::ActiveWindowRequest { window: w },
            RawEvent::KeyboardEnter {
                target: SurfaceRef::X(w),
                serial: 7,
            },
            RawEvent::KeyboardEnter {
                target: SurfaceRef::Overlay(O1),
                serial: 8,
            },
            RawEvent::KeyboardLeave {
                target: SurfaceRef::Other,
                serial: 9,
            },
            RawEvent::Key {
                pressed: true,
                serial: 10,
            },
            RawEvent::Press {
                target: PressRef::Decoration(w),
                serial: 11,
                touch: true,
            },
            RawEvent::Press {
                target: PressRef::Overlay(O2),
                serial: 12,
                touch: false,
            },
            RawEvent::PointerEnter { window: w },
            RawEvent::PopupFirstConfigure { window: w },
            RawEvent::PopupDone { window: w },
            RawEvent::SurfaceEnterOutput {
                window: w,
                output: O2,
            },
            RawEvent::OutputGeometry {
                output: O1,
                x_rect: XRect {
                    x: -3840,
                    y: 0,
                    width: 3840,
                    height: 2160,
                },
                mode: (3840, 2160),
            },
            RawEvent::OutputRemoved { output: O1 },
            RawEvent::OverlayMapped { output: O1 },
            RawEvent::OverlayClosed { output: O1 },
            RawEvent::OverlayRehome {
                window: w,
                output: O2,
            },
            RawEvent::ActivationTokenDone { window: w },
            RawEvent::BatchEnd,
        ]
    }

    #[test]
    fn every_raw_event_round_trips() {
        for (t, raw) in every_raw_event().into_iter().enumerate() {
            let line = encode(t as Millis, &raw);
            assert_eq!(
                decode(&line),
                Ok(Record::Raw {
                    t: t as Millis,
                    event: raw
                }),
                "{line}"
            );
        }
    }

    #[test]
    fn old_branch_lines_decode() {
        let map = r#"{"t":5,"k":"map","w":2097153,"or":false,"types":["NOTIFICATION"],"motif_f":null,"motif_d":0,"class":"Meeting","min":[562,56],"max":null,"pos":false,"input":"absent","above":false,"transient":null,"take_focus":false,"delete":true,"geom":[1650,2020,538,56],"client":2097152,"guess":"Notification"}"#;
        let Ok(Record::Raw {
            t: 5,
            event: RawEvent::MapFacts(f),
        }) = decode(map)
        else {
            panic!("{:?}", decode(map));
        };
        assert_eq!(f.guessed, WindowRole::Toplevel);
        assert_eq!(f.types, vec![NetWmType::Notification]);
        assert!(f.motif.no_decorations() && f.delete);
        assert_eq!(
            f.size_hints,
            Some(SizeHints {
                min: Some((562, 56)),
                max: None,
                position: false
            })
        );
        assert_eq!(
            decode(r#"{"t":6,"k":"destroy","w":2097153,"reparent":true}"#),
            Ok(Record::Raw {
                t: 6,
                event: RawEvent::Destroy { window: win(A | 1) }
            })
        );
        assert_eq!(
            decode(r#"{"t":7,"k":"x_call","call":"focus_window","w":0,"output":null}"#),
            Ok(Record::FocusCall {
                t: 7,
                call: FocusCall::SetInput {
                    window: xcb::x::WINDOW_NONE,
                    output: None
                }
            })
        );
        assert_eq!(
            decode(r#"{"t":8,"k":"x_call","call":"send_take_focus","w":2097153}"#),
            Ok(Record::FocusCall {
                t: 8,
                call: FocusCall::TakeFocus(win(A | 1))
            })
        );
        assert_eq!(
            decode(r#"{"t":9,"k":"x_call","call":"raise_to_top","w":1}"#),
            Ok(Record::OtherCall)
        );
        assert_eq!(
            decode(r#"{"t":9,"k":"x_call","call":"close_window","w":2097153}"#),
            Ok(Record::CloseCall {
                t: 9,
                window: win(A | 1)
            })
        );
        assert_eq!(
            decode(
                r#"{"t":10,"k":"role","w":2097153,"kind":"overlay_popup","parent":null,"o":1,"why":"rehome"}"#
            ),
            Ok(Record::Role {
                t: 10,
                window: win(A | 1),
                kind: RoleKind::OverlayPopup,
                parent: None,
                output: Some(O1),
                rehome: true
            })
        );
        assert_eq!(
            decode(r#"{"t":0,"k":"start","version":"0.8.3","pid":1}"#),
            Ok(Record::Start)
        );
        assert_eq!(
            decode(r#"{"t":11,"k":"something_new"}"#),
            Ok(Record::Unknown)
        );
        assert!(decode("{\"t\":1,").is_err());
    }

    #[test]
    fn role_objects() {
        let mut roles = RoleTable::default();
        add(
            &mut roles,
            facts(A | 3),
            Role::OverlayWindow {
                output: O1,
                panel: true,
            },
        );
        add(
            &mut roles,
            facts(A | 4),
            Role::PanelOf { parent: win(A | 1) },
        );
        let c = |role| Classification {
            kind: XKind::Toplevel,
            role,
            focus_on_map: FocusOnMap::None,
        };
        assert_eq!(
            role_object(&c(TOPLEVEL), &roles),
            (RoleKind::Toplevel, None, None)
        );
        assert_eq!(
            role_object(
                &c(Role::Toplevel {
                    parent: None,
                    fixed_size: true
                }),
                &roles
            ),
            (RoleKind::FixedToplevel, None, None)
        );
        assert_eq!(
            role_object(
                &c(Role::FullscreenToplevel {
                    parent: None,
                    fixed_size: false
                }),
                &roles
            ),
            (RoleKind::Fullscreen, None, None)
        );
        assert_eq!(
            role_object(&c(Role::PanelOf { parent: win(A | 1) }), &roles),
            (RoleKind::Popup, Some(win(A | 1)), None)
        );
        assert_eq!(
            role_object(
                &c(Role::OverlayWindow {
                    output: O2,
                    panel: false
                }),
                &roles
            ),
            (RoleKind::OverlayPopup, None, Some(O2))
        );
        assert_eq!(
            role_object(&c(Role::OverlayPopupOf { panel: win(A | 3) }), &roles),
            (RoleKind::OverlayPopup, None, Some(O1))
        );
        assert_eq!(
            role_object(&c(Role::OverlayPopupOf { panel: win(A | 4) }), &roles),
            (RoleKind::Popup, Some(win(A | 4)), None)
        );
    }
}
