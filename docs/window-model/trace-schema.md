# Trace schema (JSONL)

Written by the `trace` cargo feature (old branch: T2; new code: T6a), read by the
differential replay (T6b). One JSON object per line, UTF-8, no spaces outside strings,
fields in the order listed. Every line starts with:

- `"t"`: u64, milliseconds on the trace clock (monotonic; old branch: since the tracer
  started; new code: the `now` the event was fed to the model with).
- `"k"`: the kind.

Window ids and output ids are JSON numbers (the X resource id; the wl_output global name).
Missing optional values are `null`. Strings escape `"`, `\` and control characters
(`\"`, `\\`, `\n`, `\r`, `\t`, `\u00XX`).

Trace file: `$XWLS_TRACE` if set, else
`${XDG_STATE_HOME:-$HOME/.local/state}/xwayland-satellite/trace-<unix-secs>-<pid>.jsonl`.

## Header

`{"t":0,"k":"start","version":"<version()>","pid":<u32>}`

## Stream 1: facts

`map` — MapNotify, after the window's properties are read:

`{"t":T,"k":"map","w":W,"or":B,"types":[TYPE,...],"motif_f":U|null,"motif_d":U|null,"class":S|null,"min":[w,h]|null,"max":[w,h]|null,"pos":B,"input":"absent"|"true"|"false","above":B,"transient":W|null,"take_focus":B,"delete":B,"geom":[x,y,w,h],"client":U,"guess":"Toplevel"|"Popup"|"Splash"|"Notification"}`

- `types`: `_NET_WM_WINDOW_TYPE` in order, each one of `"NORMAL"`, `"DIALOG"`, `"UTILITY"`,
  `"SPLASH"`, `"MENU"`, `"POPUP_MENU"`, `"DROPDOWN_MENU"`, `"TOOLTIP"`, `"DND"`, `"COMBO"`,
  `"NOTIFICATION"`, `"OTHER"`.
- `motif_f`/`motif_d`: known bits of `_MOTIF_WM_HINTS` functions/decorations; null when
  the property or the field's flag is missing.
- `min`/`max`: WM_NORMAL_HINTS PMinSize/PMaxSize (X px); `pos`: USPosition or PPosition.
- `input`: WM_HINTS input: no WM_HINTS = `"absent"`; WM_HINTS without the Input flag =
  `"true"`.
- `client`: resource id & !resource_id_mask.
- `guess`: what the running binary's `guess_window_role` returned (the old branch's
  includes the fork arms).

`hints` — WM_HINTS PropertyNotify: `{"t":T,"k":"hints","w":W,"input":"absent"|"true"|"false"}`

## Stream 2: raw events

Each bullet is a line, then when it is written.

- `{"t":T,"k":"role_create","w":W,"geom":[x,y,w,h]}` — set_serial of a mapped window, before the role object is made; geom = satellite's X geometry then.
- `{"t":T,"k":"unmap","w":W}` — UnmapNotify.
- `{"t":T,"k":"destroy","w":W,"reparent":B}` — DestroyNotify (`false`), ReparentNotify away from the root (`true`).
- `{"t":T,"k":"active_req","w":W}` — `_NET_ACTIVE_WINDOW` client message.
- `{"t":T,"k":"kb_enter","tk":TK,"id":N,"serial":U}` — wl_keyboard.enter; `TK` is `"x"` (id = window), `"overlay"` (id = output) or `"other"` (id = null).
- `{"t":T,"k":"kb_leave","tk":TK,"id":N,"serial":U}` — wl_keyboard.leave.
- `{"t":T,"k":"key","pressed":B,"serial":U}` — wl_keyboard.key (no key code).
- `{"t":T,"k":"press","tk":PK,"id":N,"serial":U,"touch":B}` — pointer button press (any button) or touch down; `PK` is `"x"`, `"decoration"` (id = the decorated window), `"overlay"` (id = output) or `"other"` (id = null).
- `{"t":T,"k":"ptr_enter","w":W}` — pointer enter on an X window's surface, when sent to Xwayland.
- `{"t":T,"k":"popup_configure","w":W}` — first xdg_popup configure.
- `{"t":T,"k":"popup_done","w":W}` — xdg_popup.popup_done.
- `{"t":T,"k":"surface_output","w":W,"o":O}` — wl_surface.enter(output) on an X window's surface.
- `{"t":T,"k":"output","o":O,"rect":[x,y,w,h],"mode":[w,h]}` — an output appeared, or its X rect (scaled origin, rotation-aware size) or mode changed; written before the next `batch_end` and before a `role_create`.
- `{"t":T,"k":"output_removed","o":O}` — wl_output global removed.
- `{"t":T,"k":"overlay_mapped","o":O}` — the overlay's first layer configure.
- `{"t":T,"k":"overlay_closed","o":O}` — layer_surface.closed.
- `{"t":T,"k":"overlay_rehome","w":W,"o":O}` — an overlay window re-made on another output's overlay.
- `{"t":T,"k":"token_done","w":W}` — xdg-activation token received for W.
- `{"t":T,"k":"batch_end"}` — end of `handle_clientside_events`, after X focus is applied.

## Stream 3: outputs

- `{"t":T,"k":"x_call","call":"focus_window","w":W,"output":S}` — `XConnection::focus_window`; W = 0 for None; S = output name or null.
- `{"t":T,"k":"x_call","call":"send_take_focus","w":W}`, and likewise `"close_window"`, `"unmap_window"`, `"raise_to_top"`.
- `{"t":T,"k":"x_call","call":"set_fullscreen","w":W,"on":B}`.
- `{"t":T,"k":"x_call","call":"set_window_dims","w":W,"rect":[x,y,w,h]}`.
- `{"t":T,"k":"role","w":W,"kind":KIND,"parent":W,"o":O,"why":WHY}` — a role object made for W; `KIND` is `"toplevel"`, `"fixed_toplevel"`, `"fullscreen"`, `"popup"` (parent set) or `"overlay_popup"` (o set); the unused one is null; `WHY` is `"create"` or `"rehome"`.
