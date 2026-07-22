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
| `observe` | Return full, visible, or interactive accessibility state, optional query matches, screenshot metadata, and the approved-monitor PNG. |
| `act_on_element` | Invoke, focus, run a named action, or set an element value through AT-SPI. |
| `pointer` | Move, click, drag, or scroll in screenshot pixel coordinates. |
| `keyboard` | Focus by screenshot point or observed element ID, then press a key chord or type literal text. |

`observe` returns an opaque `state_id`. The runtime retains a bounded set of
latest observations for different app/window targets; observing the same target
replaces its prior ID. A successful UI action returns a new observation and
invalidates the acted-on ID. `launch_application` clears all states, so call
`observe` after launch.

## Requirements

- KDE Plasma Wayland with `xdg-desktop-portal-kde`
- Rust 1.97 or newer
- AT-SPI, PipeWire, SPA, GLib/GIO, D-Bus, and libxkbcommon development files
- An XDG RemoteDesktop portal with EIS support

Ubuntu 24.04 build dependencies:

```sh
sudo apt-get install build-essential clang libclang-dev libdbus-1-dev \
  libglib2.0-dev libpipewire-0.3-dev libspa-0.2-dev \
  libxkbcommon-dev pkg-config
```

## Install

Build and install the binary from the repository:

```sh
cargo install --locked --git https://github.com/mirsella/open-computer-use-wayland
```

When OpenCode enables the MCP, the server immediately asks KDE to restore or
approve one monitor plus pointer and keyboard access. It does this before
advertising tools. If the session later fails or is revoked, tools fail and ask
you to re-enable the MCP instead of opening another chooser.

You can approve the grant separately before enabling the MCP if preferred:

```sh
open-computer-use init
```

`init` stores a private, one-shot restore token. KDE may ask again after
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

The MCP establishes its portal session at startup. The selected monitor's full
composited image is returned by observations, including unrelated apps,
occluding windows, and desktop content. The longest encoded dimension is capped
at 1280 pixels.

Pointer coordinates and point-focused keyboard calls use
`screenshot_png_pixels`. A keyboard call may instead use an observed
`element_id`; the server revalidates it, requests AT-SPI focus, and sends keys
without a pointer click only after the element reports focused and its window
reports active. Accessibility element frames use a separate
`atspi_window_coordinates` space and must not be used as screenshot coordinates.

Use `observe.view` with `full`, `visible`, or `interactive`. `visible` prunes
hidden document subtrees such as background browser tabs; `interactive` further
restricts output to elements with interactive roles, states, supported actions,
or set-value capability. Optional `observe.query` matches semantic roles, names,
values, text, states, and action labels without renumbering element IDs. These
length-bounded filters affect accessibility output only; screenshots still
contain the complete approved monitor.

Before each action, the server rechecks the cached target and approved input
mapping. Stale, missing, or ambiguous targets fail closed; see
[ARCHITECTURE.md](ARCHITECTURE.md) for the mapping model.

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
accepts one JSON call or a static array and keeps one runtime for that batch:

```json
[
  {"name":"list_applications","arguments":{"scope":"running"}},
  {"name":"observe","arguments":{"target":"KWrite","text_limit":500}}
]
```

Static arrays cannot insert an opaque `state_id` returned by an earlier entry.
Use MCP for observe-then-act workflows. Direct arrays are suitable for calls
that do not depend on earlier output.

## Security and support

AT-SPI can read text and trigger actions in apps within the graphical session.
Screenshots contain the complete approved monitor. Run this server only for a
trusted local MCP host and user.

KDE Plasma Wayland is the maintained target. The project does not support X11,
macOS, Windows, browser automation, or a standalone desktop UI.

See [ARCHITECTURE.md](ARCHITECTURE.md), [SECURITY.md](SECURITY.md), and
[DEVELOPMENT.md](DEVELOPMENT.md) for the detailed contracts and development
workflow.
