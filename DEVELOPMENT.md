# Development

## Toolchain and dependencies

The workspace pins Rust 1.89.0 and declares 1.89 as its MSRV. Update both values
together. Workspace dependencies use exact versions. The AT-SPI stack uses
`atspi`, `atspi-proxies`, and zbus with Tokio compatibility; keep features as
small as upstream permits.

Capture uses ashpd for maintained portal proxies, raw zbus response decoding for
ScreenCast v6 metadata, pipewire-rs directly, and image's PNG encoder. Desktop
launch uses GIO's installed application registry. Input uses reis for the EI
protocol, xkbcommon for the compositor-provided keymap, and rustix for
`CLOCK_MONOTONIC`. Do not add portal `Notify*`, GStreamer, external
screenshot tools, direct input devices, or compositor-specific paths.

Native builds need the packages listed in the
[README requirements](README.md#requirements). CI uses Ubuntu 24.04 and the same
package set.

The installed binary still needs the signed-in Wayland session's D-Bus, AT-SPI,
PipeWire and SPA runtime, libxkbcommon, `xdg-desktop-portal`, and an EIS-capable
RemoteDesktop portal backend. KDE Plasma Wayland with
`xdg-desktop-portal-kde` is the current supported target. Do not infer support
for another desktop from a successful headless CI build.

If the host Cargo config injects nightly-only flags, use an isolated Cargo home:

```sh
export CARGO_HOME=/tmp/opencode/open-computer-use-cargo-home
```

## Required checks

Run these commands from the workspace root:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-features --all-targets -- -D warnings
cargo test --locked --workspace --all-features
```

Tests must use `AccessibilityAdapter` fakes for discovery, every app resolution
tier, window choice, traversal order and bounds, formatting, relocation, cache
generations, semantic actions, literal text, post-action snapshots, and failure
paths. Portal lifecycle, raw metadata, token permissions, frame layout, bounded
latest-frame delivery, geometry, mapping generations, and PNG limits also need
deterministic tests. Cover request cancellation and changed handles,
`Session.Closed`, restricted-FD ownership transfer, restore-token replacement,
post-format buffer/metadata negotiation, stream failure/recreation, wrapped SPA
chunks, pixel orientation, and post-frame AT-SPI revalidation. Do not make the
normal test suite depend on a live desktop.

Generated-input coverage includes ConnectToEIS lifecycle state, exact monitor
region matching, key chord parsing, keymap resolution, pointer move/click and wheel sequences,
full-monitor coordinate inversion, resume synchronization, and held-state
cleanup. These tests must stay
local and must not contact a desktop portal.
Keep regression coverage for complete-action emulation transactions, fresh
post-snapshot frames, post-keyboard synchronization, bounded shutdown, and
consent-driven screenshot invalidation.

An ignored, non-mutating live discovery test is available:

```sh
cargo test -p open-computer-use live_discovery_is_non_mutating -- --ignored
```

You can also run `open-computer-use list-apps` and an MCP `observe` call
against a non-sensitive visible app. The portal chooser needs real user consent.
Do not automate live click, typing, or any other generated input.

## OpenCode MCP integration

Follow the canonical Cargo installation, `init`, and OpenCode registration in
[README.md](README.md#install). The binary starts MCP only
through the explicit `mcp` subcommand; no-argument launch prints CLI help.
`doctor`, `list-apps`, and `snapshot APP` do not start a portal session.
`call FILE` executes one JSON call object or static array in one production
runtime without duplicating MCP validation or action logic. Static arrays cannot
feed an opaque `state_id` returned by one entry into another; use MCP for
observe-then-act testing.
The MCP API has exactly six tools: `list_applications` (`running` or `installed`),
`launch_application`, `observe`, `act_on_element`, `pointer`, and `keyboard`.
Keep their closed schemas and action unions in `contract.rs` aligned with
`validation.rs`. `observe` owns the one current global state ID; element,
pointer, and keyboard calls must reject stale IDs and successful UI actions must
return replacement observations. Preserve structured observation metadata:
element capabilities; screenshot readiness, reason, dimensions, and coordinate
spaces; and structured runtime error `outcome`, `retryable`, and `recovery`.
`launch_application` must remain restricted to an exact full case-sensitive
desktop ID from the installed listing and must
never grow an arbitrary command or argument escape hatch.

## Generated input

Generated input uses `SemanticRuntime::screenshot_mapping` and the existing
RemoteDesktop session's one-shot EIS connection. Screenshot coordinates always
refer to the complete approved monitor. Never derive a global target from AT-SPI
window or element extents. Tests must use fake backends and must not open a
portal, send desktop input, or run ignored live tests. Add no portal `Notify*`,
X11, clipboard, subprocess, or `/dev/uinput` fallback.
Generated keyboard tools must keep their required focus click and key events in
one cleanup-safe EIS transaction. An app string alone never proves or changes
seat focus.
