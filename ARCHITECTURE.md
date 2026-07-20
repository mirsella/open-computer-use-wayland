# Architecture

## Boundaries

The `open-computer-use` crate keeps protocol, policy, and desktop access apart:

- `contract` owns the six ordered MCP tools and closed JSON Schemas:
  `list_applications`, `launch_application`, `observe`, `act_on_element`,
  `pointer`, and `keyboard`.
- `validation` converts untrusted JSON arguments into typed tool calls.
- `runtime` defines the async `DesktopRuntime` boundary and MCP output helper.
- `accessibility` owns app resolution, bounded traversal, formatting, cache
  generations, relocation policy, and semantic action policy.
- `atspi_adapter` is the only production module that talks to AT-SPI and zbus.
- `desktop_launcher` uses GIO's installed application registry and launcher;
  arbitrary commands and arguments never enter it.
- `portal` owns the XDG RemoteDesktop/ScreenCast request and session lifecycle,
  raw versioned stream metadata, capability checks, and restore-token storage.
- `capture` owns the dedicated PipeWire thread, format negotiation, shared-memory
  frame conversion, and newest-frame channels.
- `geometry` maps separate logical and pixel spaces and rejects ambiguous maps.
- `encoder` transforms, crops, bounds, and encodes PNG data in process.
- `screenshot` coordinates consent, AT-SPI refresh, capture, mapping, encoding,
  and the typed screenshot mapping cache contract.
- `input` owns screenshot-coordinate inversion, the EIS sender lifecycle, XKB
  key parsing, per-device keyboard synchronization, and held-state cleanup.
- `server`, `cli`, and `errors` own their transport and presentation boundaries.

`AccessibilityAdapter` exposes discovery, node reads, and semantic operations.
The service layer depends only on that trait, so fake trees test ordering,
limits, timeouts, identity checks, and actions without a desktop.

## Discovery and identity

The adapter asks `org.a11y.Bus` on the user's session bus for the accessibility
bus address, then opens that address with zbus. It enumerates application roots
from the AT-SPI registry and obtains each PID from the accessibility bus daemon.
It neither runs shell helpers nor changes the process environment.

App queries resolve in this order: cached PID, numeric PID, exact app name,
exact window title, then app/window substring. A tier must have one match.
Window selection prefers active, then showing, then the first viable top-level
window. Later safety checks use exact window identity rather than requiring an
AT-SPI active flag or global window position, neither of which KDE exposes
reliably.

`observe` uses bounded depth-first traversal with per-adapter-call and whole
snapshot timeouts. Cached elements carry their D-Bus object identity, depth,
role, full name, and validated window-relative extents. The immutable committed
snapshot carries the PID, window identity, generation, and traversal limits.
The runtime keeps one global current observation as a shared snapshot plus a
separate, mutable, single-use screenshot mapping. A new snapshot commit always
starts without a mapping. Semantic
actions require its exact opaque `state_id` and the same current accessible
object, role, and name; replacement objects or stale IDs require `observe`.
Explicit element focus targets the freshly revalidated accessible object through
AT-SPI `Component.GrabFocus`; invoke never falls back to focus.

## Action flow

An element, pointer, or keyboard action requires the current state ID. Before acting,
the service re-discovers the app, verifies the PID and exact window, traverses
fresh state, and relocates any generation-scoped element. After a bounded settle
delay it re-resolves the same app/window and returns a new observation.

All tool calls share one server-side execution barrier so cancellation cleanup
finishes before any queued call starts. Stateful actions recheck the current
cache generation after acquiring that barrier and cannot act on stale state.
Cleanup has a bounded deadline; a failure or timeout closes the desktop session
rather than blocking later work.

Each operation has one implementation route. `list_applications` lists running
AT-SPI applications or installed desktop entries. `launch_application` resolves
only an exact full case-sensitive `desktop_id` from the installed listing before
GIO launch; it clears current state and returns an acknowledgement, not an
observation. Element `invoke` uses only a recognized primary AT-SPI action.
`named`, `focus`, and `set_value` are semantic-only; focus uses
`Component.GrabFocus` on the exact current object.
Structured observations report each element's inspected invoke, focus,
named-action, and text-or-number set-value capabilities. Coordinate pointer movement,
click, drag, discrete scroll, key chords, and literal text use EIS only. Scroll takes full-monitor
screenshot coordinates and standard 120-unit wheel steps; it never uses AT-SPI
geometry. Keyboard tools also require a full-monitor screenshot point and
left-click it before sending keys because EIS targets the active seat, not an
application. Missing semantic support or generated-input prerequisites return
an error; no route falls back.

Capability inspection is fail-closed. A failed interface query cannot produce
any claimed capability. Action inspection separately records no Action
interface, a successful empty or populated action list, or inspection failure;
focus, editable text, and numeric value support remain available when only
action inspection fails.

Successful element, pointer, and keyboard actions settle, observe, and return a
replacement state ID. Generated input requires the latest screenshot mapping and a fresh AT-SPI read
to agree on PID, exact app/window identity, and cache generation. The PipeWire
format generation and frame metadata must also remain current.

## Portal and capture flow

One RemoteDesktop session owns one monitor ScreenCast selection, requests
keyboard and pointer devices, and records the exact `Start` grant. The server
requests persistence mode `2` by default and stores each replacement restore
token privately for KDE to reuse; the portal can still reject restoration or
prompt again. The environment variable `OPEN_COMPUTER_USE_PERSIST_PORTAL=0`
disables persistence. The server
subscribes generically to portal Request responses before each method call,
filters by the returned path, closes dropped requests, distinguishes user cancel
from denial, watches `Session.Closed`, and closes the session on cleanup.
Initial consent has a 60-second deadline; frame acquisition remains capped at 12
seconds so a stalled PipeWire stream cannot hold an MCP call indefinitely.
`ConnectToEIS` is one-shot. Setup requires the exact resumed monitor region;
keyboard actions additionally wait for one synchronized keyboard on that pointer
seat. EIS calls and queued held-input releases share an async lock. Cleanup is an awaited barrier.
Setup cancellation, timeout, EOF, protocol errors, and disconnects invalidate
and close the whole portal session rather than switching transports.
If consent invalidates a screenshot, the pending action stops and the caller
must obtain and inspect a new state image before retrying.
It uses ashpd's maintained interface proxies and zbus for the raw Start response
because ashpd 0.13.13 does not expose ScreenCast v6 `pipewire-serial`.

The user chooses one monitor. Its stream must include compositor position and
logical size. The returned PNG contains the complete transformed monitor crop;
missing geometry or any stream count other than one fails closed. AT-SPI global
window coordinates are never used for screenshot cropping or generated input.

PipeWire objects and listeners stay on one dedicated thread. The restricted
portal file descriptor creates the core. Each stream targets its v6 serial when
available and otherwise uses its session-scoped node ID. It negotiates BGRx,
RGBx, BGRA, or RGBA. After format selection it requests MemFd/MemPtr buffers and
Header, VideoCrop, and VideoTransform metadata. Missing crop means the full
negotiated frame; present crop metadata must be valid and in bounds. DMA-BUF-only
capture invalidates the stream. Header/chunk corruption, chunk offset modulo
maxsize, aligned wrapped rows, positive or negative stride, row padding,
transform, and renegotiated dimensions are validated before the newest owned
complete frame enters the bounded watch channel. Stream errors, disconnects,
and target-node loss invalidate capture and cause clean recreation on the next
observation.

Each accepted PipeWire format has its own generation. A format change clears the
watch channel before buffer renegotiation, so frame waits cannot return pixels
from the old format. Screenshot mappings retain that generation and generated
input requires a current-generation frame, even when the new dimensions match.
Each observation also waits for a frame produced after capture begins, so its
PNG cannot predate the accessibility snapshot it accompanies.

The mapper keeps PipeWire frame pixels, portal stream logical geometry, EIS
global logical coordinates, and PNG pixels as separate values. It computes scale
per axis, so negative origins, rotation, and fractional scaling need no guessed
desktop-wide scale. The cache records PID, app/window identity, AT-SPI and portal
generations, session and stream identity, PipeWire serial and frame generation,
monitor crop, PNG size, transform, scales, mapping ID, and the exact device mask
granted by `RemoteDesktop.Start`. After frame acquisition, AT-SPI discovery must
still report the exact PID, app object, and window object before cache binding.

Before generated input, the monitor stream must match exactly one resumed EIS
absolute region. A portal `mapping_id` is required to match when present; KDE
sessions without mapping IDs instead require exact region position and logical
size. Ambiguous or missing regions fail closed. EIS frame timestamps use the
system `CLOCK_MONOTONIC` microsecond epoch. Keyboard state remains unavailable
until a post-resume connection sync callback confirms the latest modifier state.
One emulation transaction spans each complete pointer or keyboard action. A
keyboard transaction includes its focus click and a short compositor settle
delay before key emission. Every transaction ends with a connection sync barrier;
keyboard transactions also refresh modifier state. Active physical shortcut
modifiers cause generated keyboard input to fail closed.

## MCP results and errors

Observations return text plus structured `state_id`, target identity, element
capabilities, and screenshot metadata. A ready screenshot adds a complete-monitor
`image/png` block. Screenshot metadata reports readiness, reason, width, height,
and `screenshot_png_pixels`; pointer and keyboard use that space. Element frames
remain in separate `atspi_window_coordinates`. Capture failures leave the
observation available for semantic actions but mark the screenshot unavailable.

Runtime tool errors include structured `code`, `message`, `outcome`
(`not_started`, `unknown`, or `completed`), `retryable`, and `recovery`. Schema
errors use JSON-RPC `InvalidParams` (`-32602`).
