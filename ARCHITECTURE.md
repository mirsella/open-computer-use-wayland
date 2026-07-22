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
- `geometry` defines validated pixel rectangles and transforms.
- `encoder` transforms, crops, bounds, and encodes PNG data in process.
- `screenshot` coordinates consent, AT-SPI refresh, capture, mapping, encoding,
  and the typed screenshot mapping cache contract.
- `input` owns PNG-to-EIS normalization, the EIS sender lifecycle, XKB
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
snapshot carries the PID, window identity, generation, traversal limits, view,
and query. Visible and interactive views prune hidden document subtrees;
interactive views further restrict output to elements with interactive roles,
states, supported actions, or set-value capability, and a query filters
the presented elements without renumbering their generation-scoped IDs. Queries
are length-bounded and normalized once per presentation pass. The runtime keeps
a byte- and count-bounded cache of up to eight observations, at most one per exact
app/window, each with a separate mutable single-use screenshot mapping. A new
snapshot commit starts without a mapping and replaces only the same target's
prior state. Any mutation invalidates every retained screenshot mapping because
the monitor pixels may have changed. Semantic actions require the exact opaque `state_id` and the same
current accessible object, role, and name; replacement objects or replaced IDs
require `observe`.
Explicit element focus targets the freshly revalidated accessible object through
AT-SPI `Component.GrabFocus`; invoke never falls back to focus.

## Action flow

The production MCP establishes its RemoteDesktop/ScreenCast session before the
stdio protocol starts. KDE therefore restores or requests monitor, pointer, and
keyboard approval as soon as the host enables the MCP. Startup fails if approval
or capture setup fails. A failed or revoked established session is not recreated
inside a tool call; the host must restart the MCP before KDE is prompted again.

An element, pointer, or keyboard action requires a cached state ID. Before acting,
the service re-discovers the app, verifies the PID and exact window, traverses
fresh state, and relocates any generation-scoped element. After a bounded settle
delay it re-resolves the same app/window and returns a new observation.

All tool calls share one server-side execution barrier so cancellation cleanup
finishes before any queued call starts. Stateful actions recheck their cached
generation after acquiring that barrier and cannot act on replaced state.
Cleanup has a bounded deadline; a failure or timeout closes the desktop session
rather than blocking later work.

Each operation has one implementation route. `list_applications` lists running
AT-SPI applications or installed desktop entries. `launch_application` resolves
only an exact full case-sensitive `desktop_id` from the installed listing before
GIO launch; it clears the observation cache and returns an acknowledgement, not an
observation. Element `invoke` uses a recognized primary AT-SPI action. If and
only if the element exposes one action with an empty name and description, it
invokes index zero; multiple anonymous or named but unrecognized actions fail closed.
`named`, `focus`, and `set_value` are semantic-only; focus uses
`Component.GrabFocus` on the exact current object.
Structured observations report each element's inspected invoke, focus,
named-action, and text-or-number set-value capabilities. Coordinate pointer movement,
click, drag, discrete scroll, key chords, and literal text use EIS only. Scroll takes full-monitor
screenshot coordinates and standard 120-unit wheel steps; it never uses AT-SPI
geometry. Keyboard tools accept either a full-monitor screenshot point, which
is left-clicked before sending keys, or a generation-scoped element ID. Element
focus is freshly revalidated and acquired with AT-SPI `Component.GrabFocus`,
then re-read to require the exact element to be focused and its window active
before keys are sent without a pointer click. Missing semantic support or
generated-input prerequisites return an error; no route falls back.

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
It uses ashpd's maintained interface proxies and zbus for the raw Start response
because ashpd 0.13.13 does not expose ScreenCast v6 `pipewire-serial`.

The user chooses one monitor. Portal stream metadata is retained, but capture
encodes the complete transformed PipeWire crop without inventing desktop
geometry. Invalid frame geometry or any stream count other than one fails
closed. AT-SPI global window coordinates are never used for
screenshot cropping or generated input.

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
and target-node loss exhaust the process-local desktop session; the MCP must be
disabled and re-enabled before KDE can restore or request approval again.

Each accepted PipeWire format has its own generation. A format change clears the
watch channel before buffer renegotiation, so frame waits cannot return pixels
from the old format. Screenshot mappings retain that generation and generated
input requires a current-generation frame, even when the new dimensions match.
Each observation also waits for a frame produced after capture begins, so its
PNG cannot predate the accessibility snapshot it accompanies.

The mapper normalizes each PNG axis directly into the selected private EIS
region, so differing dimensions need no guessed desktop-wide scale. The cache
records PID, app/window identity, AT-SPI and portal generations, session and
stream identity, PipeWire serial and frame metadata, PNG size, and mapping ID.
After frame acquisition, AT-SPI discovery must
still report the exact PID, app object, and window object before cache binding.

Before generated input, the portal `mapping_id` must match exactly one resumed
EIS absolute region. EIS coordinates are compositor-private and are not compared
to portal desktop positions or dimensions. Streams without `mapping_id` remain
observable but cannot safely authorize generated input.
The first selected device, resume generation, origin, and dimensions remain
bound for the portal session; later changes fail closed.
Ambiguous or missing regions fail
closed. EIS frame timestamps use the
system `CLOCK_MONOTONIC` microsecond epoch. Keyboard state remains unavailable
until a post-resume connection sync callback confirms the latest modifier state.
One emulation transaction spans each complete pointer or keyboard action. A
point-focused keyboard transaction includes its focus click and a short
compositor settle delay before key emission; an element-focused transaction
uses prior AT-SPI focus and emits no pointer events. Every transaction ends with
a connection sync barrier; keyboard transactions also refresh modifier state.
Active physical shortcut modifiers cause generated keyboard input to fail
closed.

## MCP results and errors

Observations return text plus structured `state_id`, target identity, element
capabilities, and screenshot metadata. A ready screenshot adds a complete-monitor
`image/png` block. Screenshot metadata reports readiness, reason, width, height,
and `screenshot_png_pixels`; pointer and point-focused keyboard calls use that
space. Element frames remain in separate `atspi_window_coordinates`. Capture
failures leave the observation available for semantic actions but mark the
screenshot unavailable.

Runtime tool errors include structured `code`, `message`, `outcome`
(`not_started`, `unknown`, or `completed`), `retryable`, and `recovery`. Schema
errors use JSON-RPC `InvalidParams` (`-32602`).
