# CallXL desktop (`callxl`)

One Slint binary for Windows 10/11 and Debian/Ubuntu. Everything below the UI is
`callxl-core` (CLAUDE.md §9): the app only renders core state and sends core commands.

## Run

```sh
. scripts/dev-env.sh                 # shared target dir, clang-21, prebuilt libwebrtc
cargo run -p callxl-desktop          # the app (binary `callxl`)
cargo run -p callxl-desktop -- --data-dir /tmp/profile2   # a second, independent profile
cargo run -p callxl-desktop -- --headless-smoke           # prints startup_ms=… and ready, exits
cargo test -p callxl-desktop         # unit tests + screenshots in <target>/screens/*.png
apps/desktop/tests/gui_join.sh       # GUI joins a callxl-cli peer (Xvfb, real mDNS)
```

Linux runtime libraries (the `.deb` depends on them): libxkbcommon(-x11), libX11/xcb,
libwayland, libEGL/libGL, fontconfig, PulseAudio (PipeWire's Pulse layer) or ALSA, D-Bus.

## Structure

| File | Role |
|---|---|
| `build.rs` | `design/tokens.json` → `@tokens` Slint globals (in `OUT_DIR`), compiles `ui/*.slint` (never interpreted), embeds the `.ico` on Windows |
| `ui/state.slint` | `App` global: every model the UI shows and every command it can send |
| `ui/{home,call,chat,diagnostics,settings}.slint`, `ui/app.slint` | screens, dialogs, tray menu |
| `src/bridge.rs` | core ⇄ UI: the `on_event` listener only forwards; the `callxl-ui-bridge` thread owns the core handle, diffs snapshots into `Send` patches and posts them with `invoke_from_event_loop` |
| `src/view.rs` | pure conversions (rows, labels, sparkline paths) — run off the UI thread |
| `src/instance.rs` | single instance via a local socket (abstract Unix socket / named pipe) |
| `src/platform.rs` | profile dir, notifications thread, "open in folder", desktop prefs |
| `tests/screens.rs` | every screen rendered with the software renderer from fake snapshots |

## How tokens flow

`design/tokens.json` → `build.rs` writes `$OUT_DIR/tokens.slint` and registers it as the
library `@tokens` → `.slint` files `import { Colors, Space, … } from "@tokens"`. Changing a
token is a rebuild; there is no hand-maintained copy.

## Threading rules (CLAUDE.md §10)

- Core events: `on_event` → `Sender::send` (non-blocking), nothing else.
- UI callbacks: send a `UiCmd` to the bridge (non-blocking); a few apply an optimistic
  property change first (mute, APM toggles, bitrate) so the UI reacts in the same frame.
- Stats: `StatsTick` (1 Hz) → bridge formats values and sparkline paths → one patch that
  updates rows in place (`set_row_data` only for changed rows), so nothing re-layouts.
- Speaking glow: `SpeakingChanged` flips one row's `speaking`; the tile animates opacity and
  shadow only (fixed geometry), at the display's frame rate.

## Debug-only switches

`CALLXL_DEBUG_AUTOJOIN=1` joins the first listed session through the UI's own join
command (used by `tests/gui_join.sh`). Never set it in shipped configs.
