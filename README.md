# Open Computer Use for Wayland

This repository contains a local MCP server for Linux Wayland. It discovers
running applications through AT-SPI, returns bounded accessibility snapshots,
captures one user-approved monitor through PipeWire, performs explicit semantic
AT-SPI actions, and sends generated input through the RemoteDesktop session's
EIS connection.

OpenCode starts the server directly over MCP with `open-computer-use mcp`.

## Status

- `open-computer-use mcp` starts the MCP server on stdin and stdout.
- The portal session starts lazily on the first `observe`; standalone `list-apps`,
   `doctor`, and `snapshot` do not open a chooser.
- `open-computer-use list-apps` lists live accessible apps, PIDs, and a window title.
- `open-computer-use snapshot APP` prints a bounded, text-only accessibility tree.
- `open-computer-use doctor` checks Wayland, portal interface versions and
  capabilities, PipeWire, and AT-SPI without opening a portal session or
  prompting.
- `list_applications` with `scope: "running"` uses the signed-in user's AT-SPI
   bus. With `scope: "installed"`, it lists installed desktop entries.
- `launch_application` accepts only a full, exact, case-sensitive `desktop_id`
   from the installed listing and launches that exact GIO application. It never
   accepts a command, path, or arguments.
- `act_on_element` takes `state_id`, `element_id`, and one of `invoke`, `focus`,
   `named` (`name`), or `set_value` (`value`). These actions use AT-SPI only.
- The explicit `focus` action requests AT-SPI focus for the exact revalidated
  element. The caller must inspect the refreshed screenshot before coordinate or
  keyboard input.
- Coordinate click, drag, discrete wheel scroll, key chords, and literal text use
  the approved RemoteDesktop session after strict screenshot, EIS region, and
  window identity revalidation. The returned PNG is the complete approved monitor.
- `keyboard` takes `state_id`, screenshot-pixel `focus`, and `press` (key/chord)
   or `type` (literal text). It left-clicks that visible focus point before key
   input; Wayland EIS cannot route keys to a process.
  Desktop focus-switch shortcuts such as `Alt+Tab` are rejected.
- `pointer` takes `state_id` and `move`, `click` (optional `button` and `count`),
   `drag`, or `scroll` (direction and optional `steps`) in screenshot pixels.
   Scroll emits standard 120-unit EIS wheel steps and never uses AT-SPI geometry.
- Operations have one route. Element actions never fall back to pointer input,
   and keyboard text never falls back to semantic insertion.
- App, PID, window, snapshot generation, and element identity checks reject
  stale or ambiguous targets.
- Tool arguments that fail the server's schema validation return JSON-RPC
  `InvalidParams` (`-32602`) rather than a tool execution error.
- `observe` returns an opaque `state_id`, current AT-SPI state, and an MCP
   `image/png` block containing the complete approved monitor when capture is ready.
   The server has one global current observation: element, pointer, and keyboard
   calls require its exact ID; stale IDs fail and successful UI actions return a
   replacement observation. `launch_application` clears state and returns only a
   launch acknowledgement, so call `observe` afterwards.
- Structured observation metadata reports screenshot `ready`, `reason`, `width`,
   `height`, and `coordinate_space: "screenshot_png_pixels"`. Pointer and keyboard
   coordinates use that full-monitor space; element frames use separate
   `atspi_window_coordinates`. Structured elements report inspection status,
   `invoke`, `focus`, named actions, and text-or-number `set_value` capabilities.
- Runtime errors include structured `code`, `message`, `outcome`, `retryable`,
   and `recovery`.
- Screenshot consent, capture, and mapping failures keep `isError:false`, retain
  text state, and add a precise `Screenshot unavailable:` warning.
- Every successful generated action returns refreshed text and screenshot state.
- If portal consent invalidates a screenshot, the pending action stops; call
   `observe` and inspect its new image before retrying.

## Build requirements

Building requires Rust 1.89, a C/C++ toolchain, Clang and libclang, pkg-config,
and development files for GLib/GIO, D-Bus, PipeWire, SPA, and libxkbcommon. On Ubuntu
24.04:

```sh
sudo apt-get install build-essential clang libclang-dev libdbus-1-dev libglib2.0-dev \
  libpipewire-0.3-dev libspa-0.2-dev libxkbcommon-dev pkg-config
```

## Install and connect OpenCode

Install the binary directly from this checkout:

```sh
cargo install --locked --path crates/open-computer-use
```

Run the one-time KDE setup before registering the MCP server:

```sh
open-computer-use init
```

KDE asks you to approve one monitor plus keyboard and pointer access. The
command saves the private one-shot restore token and closes the temporary
session. Future MCP processes ask KDE to restore that approval. KDE can still
prompt again after revocation, a display change, or an expired grant; the
binary cannot approve the chooser itself.

Register the Cargo-installed binary directly with OpenCode:

```sh
opencode mcp add computer_use -- "$(command -v open-computer-use)" mcp
opencode mcp list
```

The OpenCode MCP server key is exactly `computer_use`, so tool names are
prefixed with `computer_use_`.

The server cannot approve a portal prompt, choose a monitor, or request all
 monitors. On the first `observe`, the desktop portal asks the signed-in
user to select exactly one monitor when KDE cannot restore the approval created
by `init`.

To remove the binary, run `cargo uninstall open-computer-use`. Remove
`mcp.computer_use` manually from OpenCode if it is no longer wanted. Cargo
uninstall does not delete the private portal token or revoke KDE's portal-side
grant.

## Portal and runtime behavior

Run the server inside the user's graphical Wayland login session with AT-SPI,
the session D-Bus, PipeWire and SPA modules, libxkbcommon,
`xdg-desktop-portal`, an EIS-capable RemoteDesktop portal, and a desktop portal
backend. The current supported and operator-tested target is KDE Plasma Wayland
with `xdg-desktop-portal-kde`. Distribution package names vary; on Debian-like
systems the runtime libraries normally come from `at-spi2-core`,
`libpipewire-0.3-0`, `libspa-0.2-modules`, `libxkbcommon0`, and GLib/GIO, alongside the KDE
portal packages.
Errors from `list-apps` explain how to fix a missing accessibility bus. The
server does not change global environment variables or enable accessibility on
the user's behalf.

The initial portal chooser controls which monitor is shared. The server requests
exactly one monitor and cannot choose or approve it itself. The screenshot
includes that complete composited monitor, including unrelated apps, desktop
content, and occluding windows. Its longest encoded dimension is limited to 1280
pixels.

Portal persistence mode `2` is requested by default. After the first approval,
KDE may restore the same monitor and input grant without another chooser. Restore
tokens are one-shot and private under the XDG state directory; directories use
mode `0700` and files use `0600`. Set `OPEN_COMPUTER_USE_PERSIST_PORTAL=0` to opt
out for a run; this does not delete an existing token or revoke KDE's grant. The
portal may decline restoration, revoke access, or prompt again after a display
change; the client cannot bypass that decision. Token-storage failures are logged
and the approved session continues without reusable restoration.

## Scope

The current support target is KDE Plasma Wayland used through OpenCode's MCP
client. Other desktops and portal backends are not part of the maintained
support target. The project does not target X11, macOS, Windows, browsers, or
a standalone desktop UI. It uses no external screenshot command, GStreamer
subprocess, compositor-private API, `/dev/uinput`, or X11 fallback. Generated
input uses only `ConnectToEIS` on the existing approved RemoteDesktop session.
It does not use portal `Notify*`, direct Linux input devices, or
compositor-specific correction factors.

## Direct commands

The diagnostic and accessibility-only commands remain available outside
OpenCode:

```sh
open-computer-use doctor
open-computer-use init
open-computer-use list-apps
open-computer-use snapshot APP
open-computer-use call CALLS.json
open-computer-use mcp
```

`call` runs one call object or an array in one process, preserving state between
calls. Use `-` to read JSON from stdin. This is useful for direct testing without
an MCP host:

```json
[
  {"name":"list_applications","arguments":{"scope":"running"}},
  {"name":"observe","arguments":{"target":"Zen Browser","text_limit":500}}
]
```

Use the returned `state_id` in a later direct `act_on_element`, `pointer`, or
`keyboard` call. A successful UI action returns the next ID.

It prints one standard MCP result per line and stops at the first error. A
validation error prints no result for the invalid call and exits nonzero. A
runtime error prints an `isError:true` result, exits nonzero, and skips remaining
calls.

See [ARCHITECTURE.md](ARCHITECTURE.md), [SECURITY.md](SECURITY.md), and
[DEVELOPMENT.md](DEVELOPMENT.md).
