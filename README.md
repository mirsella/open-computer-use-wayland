# Open Computer Use for Wayland

[![CI](https://github.com/mirsella/open-computer-use-wayland/actions/workflows/ci.yml/badge.svg)](https://github.com/mirsella/open-computer-use-wayland/actions/workflows/ci.yml)

A local [Model Context Protocol](https://modelcontextprotocol.io/) server for
computer use on Linux Wayland. It is built for KDE Plasma and OpenCode.

The server combines:

- AT-SPI for app discovery, accessibility state, and semantic actions
- XDG ScreenCast for screenshots of one user-approved monitor
- XDG RemoteDesktop and EIS for pointer and keyboard input
- GIO for installed app discovery and launch

It does not use X11, `/dev/uinput`, clipboard injection, compositor-private
APIs, or guessed display geometry.

## Tools

| Tool | Purpose |
| --- | --- |
| `list_applications` | List running AT-SPI apps or installed desktop entries. |
| `launch_application` | Launch an exact desktop ID returned by the installed listing. |
| `observe` | Return accessibility state, screenshot metadata, and the approved-monitor PNG. |
| `act_on_element` | Invoke, focus, run a named action, or set an element value through AT-SPI. |
| `pointer` | Move, click, drag, or scroll in screenshot pixel coordinates. |
| `keyboard` | Click a visible focus point, then press a key chord or type literal text. |

`observe` returns an opaque `state_id`. Element, pointer, and keyboard calls
must use the current ID. A successful UI action returns a new observation and
invalidates the old ID. `launch_application` clears the current state, so call
`observe` after launch.

## Requirements

- KDE Plasma Wayland with `xdg-desktop-portal-kde`
- Rust 1.89 or newer
- AT-SPI, PipeWire, SPA, GLib/GIO, D-Bus, and libxkbcommon development files
- An XDG RemoteDesktop portal with EIS support

Ubuntu 24.04 build dependencies:

```sh
sudo apt-get install build-essential clang libclang-dev libdbus-1-dev \
  libglib2.0-dev libpipewire-0.3-dev libspa-0.2-dev \
  libxkbcommon-dev pkg-config
```

## Install

Build and install the binary from this checkout:

```sh
cargo install --locked --path crates/open-computer-use
```

Ask KDE to approve one monitor plus pointer and keyboard access:

```sh
open-computer-use init
```

The command stores a private, one-shot restore token. KDE may ask again after
revocation, a display change, or an expired grant. The server cannot approve
the chooser or select a monitor on the user's behalf.

Register the binary with OpenCode:

```sh
opencode mcp add computer_use -- "$(command -v open-computer-use)" mcp
opencode mcp list
```

The MCP key is `computer_use`, so OpenCode prefixes tool names with
`computer_use_`.

## Runtime model

The portal session starts lazily on the first `observe`. The selected monitor's
full composited image is returned, including unrelated apps, occluding windows,
and desktop content. The longest encoded dimension is capped at 1280 pixels.

Pointer and keyboard coordinates use `screenshot_png_pixels`. Accessibility
element frames use a separate `atspi_window_coordinates` space and must not be
used as screenshot coordinates.

Before each action, the server rechecks the app PID, app and window identity,
state generation, portal session, stream metadata, and exact EIS monitor region.
Stale or ambiguous targets fail instead of falling back to another input route.

Runtime errors include `code`, `message`, `outcome`, `retryable`, and
`recovery`. If screenshot capture fails, `observe` still returns the text state
with `screenshot.ready: false` and a reason.

## Direct commands

These commands work without an MCP host:

```sh
open-computer-use doctor
open-computer-use init
open-computer-use list-apps
open-computer-use snapshot APP
open-computer-use call CALLS.json
open-computer-use mcp
```

`doctor`, `list-apps`, and `snapshot` do not open a portal chooser. `call`
accepts one JSON call or an array and keeps state between calls:

```json
[
  {"name":"list_applications","arguments":{"scope":"running"}},
  {"name":"observe","arguments":{"target":"KWrite","text_limit":500}}
]
```

Use the returned `state_id` in a later `act_on_element`, `pointer`, or
`keyboard` call.

## Security and support

AT-SPI can read text and trigger actions in apps within the graphical session.
Screenshots contain the complete approved monitor. Run this server only for a
trusted local MCP host and user.

KDE Plasma Wayland is the maintained target. The project does not support X11,
macOS, Windows, browser automation, or a standalone desktop UI.

See [ARCHITECTURE.md](ARCHITECTURE.md), [SECURITY.md](SECURITY.md), and
[DEVELOPMENT.md](DEVELOPMENT.md) for the detailed contracts and development
workflow.
