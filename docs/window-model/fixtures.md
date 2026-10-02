# Scenario fixtures

Fixtures are Rust tests in `src/server/scenario/` (no data files: no serde). Each is a
`#[test]` whose doc comment names what it pins and cites its source: an observer-log
excerpt under `tests/scenarios/sources/` with line range, a design section, or an
inventory row. They drive the pure model through `Model::feed`, the same pipeline the
event layer uses.

A `Scenario` holds a `Model`, a clock (`now`, starting at 1000 ms) and the outputs emitted
since the last `expect`.

Steps (each returns `&mut Scenario`):

| Step | Raw events fed |
|---|---|
| `raw(e)` | `e` |
| `advance(ms)` | none; the clock moves |
| `batch()` | `BatchEnd` |
| `output(o, x)` | `OutputGeometry` (3840x2160 at X `(x, 0)`, mode 3840x2160), `OverlayMapped`, `BatchEnd` |
| `map(f)` | `MapFacts(f)`, `RoleCreate{f.window, f.dims}`, `BatchEnd`; then, if the window's role is not a toplevel, `PopupFirstConfigure`, `BatchEnd` |
| `map_unconfigured(f)` | `MapFacts(f)`, `RoleCreate`, `BatchEnd` |
| `configure(w)` | `PopupFirstConfigure`, `BatchEnd` |
| `facts_only(f)` | `MapFacts(f)` (role not created yet) |
| `enter(t)` / `leave(t)` | `KeyboardEnter`/`KeyboardLeave` (no batch end) |
| `key(pressed)` | `Key` |
| `press(t)` | `Press` (touch false) |
| `hover(w)` | `PointerEnter` |
| `unmap(w)` / `destroy(w)` / `activate(w)` | `Unmap` / `Destroy` / `ActiveWindowRequest`, then `BatchEnd` (an X event is always followed by a batch end) |

Checks:

- `expect(&[outputs])`: the outputs emitted since the previous `expect` (or the start),
  in order, are exactly these; then forgets them. Routing and token outputs appear when
  their event is fed, `XFocus` at the `BatchEnd` that applies it, so "no `XFocus` this
  batch" (`expect(&[])`) and "`XFocus` of the same window again" differ.
- `expect_role(w, role)`: `w`'s classification has this role (outputs untouched).
- `expect_panel(w)`: the focus machine's panel slot holds `w` (`None`: empty).

Output constructors: `xf(w)` (SetInput, primary None), `xf_on(w, o)`, `xf_take(w)`,
`xf_take_on(w, o)`, `xf_none()`, `route(w)`, `unroute()`, `token(w, KbTarget)`.

A fixture whose expectation contradicts the rule text it cites is fixed by changing the
fixture only after the reviewer agrees; a fixture that shows a rule's outcome is wrong is a
stop for the controller.
