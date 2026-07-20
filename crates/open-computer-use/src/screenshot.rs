use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use tokio::sync::Mutex;

use crate::{
    accessibility::{ObjectId, Snapshot},
    capture::{CaptureBackend, CaptureSession, OwnedFrame, PipeWireCapture},
    encoder::{PngScreenshotEncoder, ScreenshotEncoder},
    geometry::{GeometryMapper, PixelRect, SafeGeometryMapper, StreamGeometry, Transform},
    input::{
        GeneratedInputAction, GeneratedInputFuture, GeneratedInputProvider, backend::InputBackend,
        coordinates::ValidatedMapping, eis::ReisInputBackend, keyboard_input, pointer,
    },
    portal::{GrantedDevices, PortalBackend, PortalSessionLease, PortalStream, XdgPortalBackend},
};

pub type ScreenshotFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ScreenshotError>> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotError(pub String);

impl std::fmt::Display for ScreenshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ScreenshotError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrepareCapture {
    pub consent_interrupted_observation: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenshotMapping {
    pub app_pid: u32,
    pub app_identity: ObjectId,
    pub window_identity: ObjectId,
    pub accessibility_generation: u64,
    pub portal_session_identity: String,
    pub portal_session_generation: u64,
    pub remote_desktop_devices: GrantedDevices,
    pub stream_index: usize,
    pub stream_id: Option<String>,
    pub stream_position: Option<(i32, i32)>,
    pub stream_logical_size: Option<(i32, i32)>,
    pub pipewire_node_id: u32,
    pub pipewire_serial: Option<u64>,
    pub source_frame_generation: u64,
    pub source_format_generation: u64,
    pub source_frame_size: (u32, u32),
    pub original_frame_crop: PixelRect,
    pub transformed_monitor_crop: PixelRect,
    pub output_png_size: (u32, u32),
    pub png_to_transformed_x: f64,
    pub png_to_transformed_y: f64,
    pub scale_x: f64,
    pub scale_y: f64,
    pub transform: Transform,
    pub mapping_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenshotObservation {
    pub png_base64: String,
    pub mapping: ScreenshotMapping,
}

pub trait ScreenshotProvider: Send + Sync + 'static {
    fn prepare(&self) -> ScreenshotFuture<'_, PrepareCapture>;
    fn capture<'a>(&'a self, snapshot: &'a Snapshot)
    -> ScreenshotFuture<'a, ScreenshotObservation>;
}

#[derive(Debug, Default)]
pub struct NoScreenshots;

impl ScreenshotProvider for NoScreenshots {
    fn prepare(&self) -> ScreenshotFuture<'_, PrepareCapture> {
        Box::pin(async {
            Ok(PrepareCapture {
                consent_interrupted_observation: false,
            })
        })
    }

    fn capture<'a>(
        &'a self,
        _snapshot: &'a Snapshot,
    ) -> ScreenshotFuture<'a, ScreenshotObservation> {
        Box::pin(async { Err(ScreenshotError("capture backend is not configured".into())) })
    }
}

pub type ProductionScreenshotCoordinator = ScreenshotCoordinator<
    XdgPortalBackend,
    PipeWireCapture,
    SafeGeometryMapper,
    PngScreenshotEncoder,
>;

pub struct ScreenshotCoordinator<P, C, G, E> {
    portal: P,
    capture: C,
    geometry: G,
    encoder: E,
    state: Mutex<Option<ActiveCapture>>,
}

impl<P, C, G, E> std::fmt::Debug for ScreenshotCoordinator<P, C, G, E>
where
    P: std::fmt::Debug,
    C: std::fmt::Debug,
    G: std::fmt::Debug,
    E: std::fmt::Debug,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScreenshotCoordinator")
            .field("portal", &self.portal)
            .field("capture", &self.capture)
            .field("geometry", &self.geometry)
            .field("encoder", &self.encoder)
            .finish_non_exhaustive()
    }
}

impl Default for ProductionScreenshotCoordinator {
    fn default() -> Self {
        Self::new(
            XdgPortalBackend::default(),
            PipeWireCapture,
            SafeGeometryMapper,
            PngScreenshotEncoder,
        )
    }
}

impl<P, C, G, E> ScreenshotCoordinator<P, C, G, E> {
    pub fn new(portal: P, capture: C, geometry: G, encoder: E) -> Self {
        Self {
            portal,
            capture,
            geometry,
            encoder,
            state: Mutex::new(None),
        }
    }
}

struct ActiveCapture {
    // Rust drops fields in declaration order: stop PipeWire before closing the session.
    capture: Box<dyn CaptureSession>,
    session: Arc<PortalSessionLease>,
    stream: PortalStream,
    input: Option<Arc<ReisInputBackend>>,
    input_frame_generation: Option<u64>,
}

impl<P, C, G, E> ScreenshotProvider for ScreenshotCoordinator<P, C, G, E>
where
    P: PortalBackend,
    C: CaptureBackend,
    G: GeometryMapper,
    E: ScreenshotEncoder,
{
    fn prepare(&self) -> ScreenshotFuture<'_, PrepareCapture> {
        Box::pin(async {
            let mut state = self.state.lock().await;
            if state.as_ref().is_some_and(|active| {
                !active.session.is_closed() && active.capture.failure().is_none()
            }) {
                return Ok(PrepareCapture {
                    consent_interrupted_observation: false,
                });
            }
            if let Some(active) = state.as_ref() {
                let reason = active
                    .capture
                    .failure()
                    .unwrap_or_else(|| "portal session closed".into());
                eprintln!("open-computer-use: recreating capture after {reason}");
                *state = None;
            }
            let connection = self.portal.establish().await.map_err(ScreenshotError)?;
            let target = connection.stream.capture_target();
            let mut capture = self
                .capture
                .start(connection.fd, vec![target])
                .map_err(ScreenshotError)?;
            tokio::time::timeout(Duration::from_secs(5), capture.wait_ready())
                .await
                .map_err(|_| {
                    ScreenshotError(
                        "timed out waiting for PipeWire shared-memory capture; the source may be DMA-BUF-only"
                            .into(),
                    )
                })?
                .map_err(ScreenshotError)?;
            let session = connection.session;
            *state = Some(ActiveCapture {
                capture,
                session,
                stream: connection.stream,
                input: None,
                input_frame_generation: None,
            });
            Ok(PrepareCapture {
                consent_interrupted_observation: connection.consent_interrupted_observation,
            })
        })
    }

    fn capture<'a>(
        &'a self,
        snapshot: &'a Snapshot,
    ) -> ScreenshotFuture<'a, ScreenshotObservation> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            let active = state.as_mut().ok_or_else(|| {
                ScreenshotError("portal capture session is not established".into())
            })?;
            if active.session.is_closed() {
                eprintln!(
                    "open-computer-use: capture requested after portal Session.Closed: session={} generation={}",
                    active.session.identity(),
                    active.session.generation()
                );
                return Err(ScreenshotError(
                    "portal RemoteDesktop session closed".into(),
                ));
            }
            let stream = &active.stream;
            let baseline = active
                .capture
                .latest_after(stream.stream_index, None, Duration::from_secs(2))
                .await
                .map_err(ScreenshotError)?;
            let frame = active
                .capture
                .latest_after(
                    stream.stream_index,
                    Some(baseline.generation),
                    Duration::from_secs(2),
                )
                .await
                .map_err(ScreenshotError)?;
            let mapping = self
                .geometry
                .map(&[StreamGeometry {
                    stream_index: stream.stream_index,
                    position: stream.position,
                    logical_size: stream.logical_size,
                    frame_size: (frame.width, frame.height),
                    frame_crop: Some(frame.crop),
                    transform: frame.transform,
                }])
                .map_err(ScreenshotError)?;
            if mapping.stream_index != stream.stream_index {
                eprintln!(
                    "open-computer-use: geometry mapper returned unknown stream index {}",
                    mapping.stream_index
                );
                return Err(ScreenshotError(
                    "geometry mapper stream invariant failed".into(),
                ));
            }
            verify_frame_mapping(&frame, &mapping)?;
            let encoded = self
                .encoder
                .encode(
                    frame.rgba,
                    (frame.width, frame.height),
                    mapping.source_frame_crop,
                    mapping.transform,
                    mapping.transformed_crop,
                )
                .map_err(ScreenshotError)?;
            if active.session.is_closed() {
                return Err(ScreenshotError(
                    "portal session closed while encoding the screenshot".into(),
                ));
            }
            Ok(ScreenshotObservation {
                png_base64: STANDARD.encode(&encoded.bytes),
                mapping: ScreenshotMapping {
                    app_pid: snapshot.app.pid,
                    app_identity: snapshot.app.object.clone(),
                    window_identity: snapshot.window.object.clone(),
                    accessibility_generation: snapshot.generation,
                    portal_session_identity: active.session.identity().to_owned(),
                    portal_session_generation: active.session.generation(),
                    remote_desktop_devices: active.session.granted_devices(),
                    stream_index: stream.stream_index,
                    stream_id: stream.id.clone(),
                    stream_position: stream.position,
                    stream_logical_size: stream.logical_size,
                    pipewire_node_id: stream.node_id,
                    pipewire_serial: stream.pipewire_serial,
                    source_frame_generation: frame.generation,
                    source_format_generation: frame.format_generation,
                    source_frame_size: (frame.width, frame.height),
                    original_frame_crop: frame.crop,
                    transformed_monitor_crop: mapping.transformed_crop,
                    output_png_size: (encoded.width, encoded.height),
                    png_to_transformed_x: encoded.png_to_transformed_x,
                    png_to_transformed_y: encoded.png_to_transformed_y,
                    scale_x: mapping.scale_x,
                    scale_y: mapping.scale_y,
                    transform: mapping.transform,
                    mapping_id: stream.mapping_id.clone(),
                },
            })
        })
    }
}

impl<P, C, G, E> GeneratedInputProvider for ScreenshotCoordinator<P, C, G, E>
where
    P: PortalBackend,
    C: CaptureBackend,
    G: GeometryMapper,
    E: ScreenshotEncoder,
{
    fn prepare_input<'a>(
        &'a self,
        snapshot: &'a Snapshot,
        mapping: &'a ScreenshotMapping,
        action: &'a GeneratedInputAction,
    ) -> GeneratedInputFuture<'a> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            let active = state
                .as_mut()
                .ok_or_else(|| "portal capture/input session is not established".to_owned())?;
            ValidatedMapping::new(snapshot, mapping, &active.session, &active.stream)?;
            validate_current_capture(active, mapping).await?;
            let keyboard_required = matches!(
                action,
                GeneratedInputAction::PressKey { .. } | GeneratedInputAction::TypeText { .. }
            );
            if !active.session.granted_devices().pointer()
                || (keyboard_required && !active.session.granted_devices().keyboard())
            {
                return Err("the portal session lacks grants required for this EIS action".into());
            }
            let stream_position = mapping
                .stream_position
                .ok_or_else(|| "selected monitor stream has no compositor position".to_owned())?;
            let stream_size = mapping
                .stream_logical_size
                .ok_or_else(|| "selected monitor stream has no logical size".to_owned())?;
            let connected_now = active.input.is_none();
            if connected_now {
                match ReisInputBackend::connect(
                    Arc::clone(&active.session),
                    mapping.mapping_id.clone(),
                    stream_position,
                    stream_size,
                )
                .await
                {
                    Ok(input) => active.input = Some(input),
                    Err(error) => {
                        *state = None;
                        return Err(error);
                    }
                }
            }
            let active = state
                .as_mut()
                .ok_or_else(|| "capture state disappeared after EIS setup".to_owned())?;
            if connected_now {
                validate_current_capture(active, mapping).await?;
            }
            let input = active
                .input
                .as_ref()
                .ok_or_else(|| "EIS backend disappeared after setup".to_owned())?;
            let region = input.wait_for_action(keyboard_required).await?;
            ValidatedMapping::new(snapshot, mapping, &active.session, &active.stream)?
                .eis_mapper(region)?;
            require_action_capabilities(input.as_ref(), action)
        })
    }

    fn perform_input<'a>(
        &'a self,
        snapshot: &'a Snapshot,
        mapping: &'a ScreenshotMapping,
        action: GeneratedInputAction,
    ) -> GeneratedInputFuture<'a> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            let active = state
                .as_mut()
                .ok_or_else(|| "portal capture/input session is not established".to_owned())?;
            ValidatedMapping::new(snapshot, mapping, &active.session, &active.stream)?;
            if let Err(error) = validate_current_capture(active, mapping).await {
                eprintln!("open-computer-use: invalidating capture before input: {error}");
                *state = None;
                return Err(error);
            }
            let active = state
                .as_mut()
                .ok_or_else(|| "capture state disappeared after pre-input validation".to_owned())?;
            let input = active
                .input
                .as_ref()
                .ok_or_else(|| "EIS input was not prepared for this action".to_owned())?
                .clone();
            let backend: Arc<dyn InputBackend> = input.clone();
            let validated =
                ValidatedMapping::new(snapshot, mapping, &active.session, &active.stream)?;
            let region = input.region()?;
            let mapper = validated.eis_mapper(region)?;
            require_action_capabilities(backend.as_ref(), &action)?;

            match action {
                GeneratedInputAction::MovePointer { x, y } => {
                    let (x, y) = mapper.point(x, y)?;
                    pointer::move_pointer(backend, x, y).await?;
                }
                GeneratedInputAction::Click {
                    x,
                    y,
                    button,
                    count,
                } => {
                    let (x, y) = mapper.point(x, y)?;
                    pointer::click(backend, x, y, button, count).await?;
                }
                GeneratedInputAction::Drag { from, to } => {
                    let from = mapper.point(from.0, from.1)?;
                    let to = mapper.point(to.0, to.1)?;
                    pointer::drag(backend, from, to).await?;
                }
                GeneratedInputAction::Scroll {
                    x,
                    y,
                    delta_x,
                    delta_y,
                } => {
                    let (x, y) = mapper.point(x, y)?;
                    pointer::scroll(backend, x, y, delta_x, delta_y).await?;
                }
                GeneratedInputAction::PressKey { focus, key } => {
                    let focus = mapper.point(focus.0, focus.1)?;
                    keyboard_input::press_key(input.clone(), focus, &key).await?;
                }
                GeneratedInputAction::TypeText { focus, text } => {
                    let focus = mapper.point(focus.0, focus.1)?;
                    keyboard_input::type_text(input.clone(), focus, &text).await?;
                }
            }
            if active.session.is_closed() {
                return Err("portal Session.Closed during generated input".into());
            }
            Ok(())
        })
    }

    fn cleanup_input(&self) -> crate::input::backend::InputFuture<'_> {
        Box::pin(async move {
            let input = {
                let state = self.state.lock().await;
                let Some(active) = state.as_ref() else {
                    return Ok(());
                };
                active.input.clone()
            };
            match input {
                Some(input) => input.cleanup_barrier().await,
                None => Ok(()),
            }
        })
    }

    fn shutdown_input(&self) -> crate::input::backend::InputFuture<'_> {
        Box::pin(async move {
            let active = self.state.lock().await.take();
            let Some(active) = active else {
                return Ok(());
            };
            let ActiveCapture {
                capture,
                session,
                stream: _,
                input,
                input_frame_generation: _,
            } = active;
            let cleanup = match input.as_ref() {
                Some(input) => {
                    tokio::time::timeout(Duration::from_secs(2), input.cleanup_barrier())
                        .await
                        .unwrap_or_else(|_| {
                            Err("timed out neutralizing EIS input during shutdown".to_owned())
                        })
                }
                None => Ok(()),
            };
            drop(capture);
            drop(input);
            let close = tokio::time::timeout(
                Duration::from_secs(2),
                session.close("computer-use shutdown"),
            )
            .await
            .unwrap_or_else(|_| {
                Err("timed out closing the portal session during shutdown".to_owned())
            });
            cleanup.and(close)
        })
    }
}

async fn validate_current_capture(
    active: &mut ActiveCapture,
    mapping: &ScreenshotMapping,
) -> Result<(), String> {
    if let Some(error) = active.capture.failure() {
        return Err(format!("PipeWire capture is unhealthy: {error}"));
    }
    let after_generation = active
        .input_frame_generation
        .unwrap_or(mapping.source_frame_generation)
        .max(mapping.source_frame_generation);
    let frame = active
        .capture
        .latest_after(
            mapping.stream_index,
            Some(after_generation),
            Duration::from_millis(250),
        )
        .await?;
    verify_current_frame_metadata(&frame, mapping)?;
    active.input_frame_generation = Some(frame.generation);
    Ok(())
}

fn verify_current_frame_metadata(
    frame: &OwnedFrame,
    mapping: &ScreenshotMapping,
) -> Result<(), String> {
    if frame.stream_index != mapping.stream_index
        || frame.format_generation != mapping.source_format_generation
        || (frame.width, frame.height) != mapping.source_frame_size
        || frame.crop != mapping.original_frame_crop
        || frame.transform != mapping.transform
    {
        return Err(format!(
            "PipeWire stream metadata renegotiated after screenshot: format_generation={} size={:?} crop={:?} transform={:?}",
            frame.format_generation,
            (frame.width, frame.height),
            frame.crop,
            frame.transform
        ));
    }
    Ok(())
}

fn require_action_capabilities(
    backend: &dyn InputBackend,
    action: &GeneratedInputAction,
) -> Result<(), String> {
    let capabilities = backend.capabilities();
    let available = match action {
        GeneratedInputAction::MovePointer { .. } => true,
        GeneratedInputAction::Click { .. } | GeneratedInputAction::Drag { .. } => {
            capabilities.button
        }
        GeneratedInputAction::Scroll { .. } => capabilities.scroll,
        GeneratedInputAction::PressKey { .. } | GeneratedInputAction::TypeText { .. } => {
            capabilities.button && capabilities.keyboard
        }
    };
    if !available {
        return Err("EIS backend lacks the device capabilities required for this action".into());
    }
    Ok(())
}

fn verify_frame_mapping(
    frame: &OwnedFrame,
    mapping: &crate::geometry::CropMapping,
) -> Result<(), ScreenshotError> {
    if frame.stream_index != mapping.stream_index
        || frame.crop != mapping.source_frame_crop
        || frame.transform != mapping.transform
    {
        eprintln!(
            "open-computer-use: PipeWire frame changed after geometry mapping: stream={} generation={} crop={:?} transform={:?}",
            frame.stream_index, frame.generation, frame.crop, frame.transform
        );
        return Err(ScreenshotError(
            "PipeWire frame crop or transform changed during mapping".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io::{Read, Write},
        os::fd::OwnedFd,
        os::unix::net::UnixStream,
        sync::{
            Mutex as StdMutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;
    use crate::{
        accessibility::{AppInfo, WindowInfo},
        capture::{CaptureFuture, CaptureSession, CaptureTarget},
        portal::{GrantedDevices, PortalCapabilities, PortalConnection, PortalFuture},
    };

    #[test]
    fn stale_frame_geometry_is_rejected() {
        let frame = OwnedFrame {
            stream_index: 2,
            generation: 9,
            format_generation: 1,
            width: 10,
            height: 10,
            rgba: vec![0; 400],
            crop: PixelRect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
            transform: Transform::Normal,
        };
        let mapping = crate::geometry::CropMapping {
            stream_index: 2,
            transformed_crop: frame.crop,
            source_frame_crop: frame.crop,
            scale_x: 1.0,
            scale_y: 1.0,
            transform: Transform::Rotate90,
        };
        assert!(
            verify_frame_mapping(&frame, &mapping)
                .unwrap_err()
                .0
                .contains("changed")
        );
    }

    struct FakePortal {
        connections: StdMutex<VecDeque<PortalConnection>>,
        establishes: AtomicUsize,
    }

    impl PortalBackend for FakePortal {
        fn establish(&self) -> PortalFuture<'_, PortalConnection> {
            Box::pin(async {
                self.establishes.fetch_add(1, Ordering::AcqRel);
                self.connections
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or_else(|| "no fake portal connection remains".into())
            })
        }

        fn capabilities(&self) -> PortalFuture<'_, PortalCapabilities> {
            Box::pin(async { Ok(test_capabilities()) })
        }
    }

    #[derive(Default)]
    struct FakeCaptureState {
        markers: StdMutex<Vec<u8>>,
        failures: StdMutex<Vec<Arc<StdMutex<Option<String>>>>>,
        drops: AtomicUsize,
    }

    struct FakeCaptureBackend(Arc<FakeCaptureState>);

    impl CaptureBackend for FakeCaptureBackend {
        fn start(
            &self,
            fd: OwnedFd,
            targets: Vec<CaptureTarget>,
        ) -> Result<Box<dyn CaptureSession>, String> {
            if targets.len() != 1 || targets[0].stream_index != 0 {
                return Err("fake capture received wrong targets".into());
            }
            let mut file = std::fs::File::from(fd);
            let mut marker = [0_u8; 1];
            file.read_exact(&mut marker)
                .map_err(|error| format!("restricted fd was not handed to capture: {error}"))?;
            self.0.markers.lock().unwrap().push(marker[0]);
            let failure = Arc::new(StdMutex::new(None));
            self.0.failures.lock().unwrap().push(Arc::clone(&failure));
            Ok(Box::new(FakeCaptureSession {
                failure,
                drops: Arc::clone(&self.0),
            }))
        }
    }

    struct FakeCaptureSession {
        failure: Arc<StdMutex<Option<String>>>,
        drops: Arc<FakeCaptureState>,
    }

    impl CaptureSession for FakeCaptureSession {
        fn wait_ready(&mut self) -> CaptureFuture<'_, ()> {
            let result = self.failure.lock().unwrap().clone().map_or(Ok(()), Err);
            Box::pin(async move { result })
        }

        fn failure(&self) -> Option<String> {
            self.failure.lock().unwrap().clone()
        }

        fn latest_after(
            &mut self,
            stream_index: usize,
            after_generation: Option<u64>,
            _wait: Duration,
        ) -> CaptureFuture<'_, OwnedFrame> {
            let failure = self.failure();
            Box::pin(async move {
                if let Some(error) = failure {
                    return Err(error);
                }
                let generation = after_generation.unwrap_or(0) + 1;
                Ok(OwnedFrame {
                    stream_index,
                    generation,
                    format_generation: 1,
                    width: 2,
                    height: 2,
                    rgba: vec![255; 16],
                    crop: PixelRect {
                        x: 0,
                        y: 0,
                        width: 2,
                        height: 2,
                    },
                    transform: Transform::Normal,
                })
            })
        }
    }

    impl Drop for FakeCaptureSession {
        fn drop(&mut self) {
            self.drops.drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn test_capabilities() -> PortalCapabilities {
        PortalCapabilities {
            remote_desktop_version: 2,
            screencast_version: 6,
            available_device_types: 7,
            available_source_types: 3,
            available_cursor_modes: 7,
        }
    }

    fn test_connection(
        generation: u64,
        marker: u8,
    ) -> (PortalConnection, tokio::sync::watch::Sender<bool>) {
        let (read, mut write) = UnixStream::pair().unwrap();
        write.write_all(&[marker]).unwrap();
        let (session, closed) = PortalSessionLease::for_test("/session/test", generation, 3);
        (
            PortalConnection {
                fd: read.into(),
                session,
                stream: PortalStream {
                    stream_index: 0,
                    node_id: 10,
                    pipewire_serial: Some(20),
                    id: Some("stream".into()),
                    mapping_id: Some("mapping".into()),
                    position: Some((0, 0)),
                    logical_size: Some((2, 2)),
                },
                consent_interrupted_observation: false,
            },
            closed,
        )
    }

    fn test_snapshot() -> Snapshot {
        let object = ObjectId {
            bus_name: ":1.5".into(),
            path: "/window".into(),
        };
        Snapshot {
            app_query: "test".into(),
            app: AppInfo {
                object: ObjectId {
                    bus_name: ":1.5".into(),
                    path: "/app".into(),
                },
                name: "Test".into(),
                pid: 5,
                windows: Vec::new(),
            },
            window: WindowInfo {
                object,
                title: "Window".into(),
                states: ["active".into()].into_iter().collect(),
            },
            generation: 1,
            elements: Vec::new(),
            node_limit_reached: false,
            depth_limit_reached: false,
            text_limit: 10,
            max_nodes: 10,
            max_depth: 10,
            screenshot_mapping: None,
        }
    }

    #[tokio::test]
    async fn restricted_fd_failure_and_session_close_recreate_capture_cleanly() {
        let (first, first_closed) = test_connection(1, 11);
        let (second, second_closed) = test_connection(2, 22);
        let (third, _) = test_connection(3, 33);
        let capture_state = Arc::new(FakeCaptureState::default());
        let coordinator = ScreenshotCoordinator::new(
            FakePortal {
                connections: StdMutex::new(VecDeque::from([first, second, third])),
                establishes: AtomicUsize::new(0),
            },
            FakeCaptureBackend(Arc::clone(&capture_state)),
            SafeGeometryMapper,
            PngScreenshotEncoder,
        );

        coordinator.prepare().await.unwrap();
        assert_eq!(*capture_state.markers.lock().unwrap(), [11]);
        *capture_state.failures.lock().unwrap()[0].lock().unwrap() =
            Some("target node disappeared".into());
        coordinator.prepare().await.unwrap();
        assert_eq!(*capture_state.markers.lock().unwrap(), [11, 22]);
        assert_eq!(capture_state.drops.load(Ordering::Acquire), 1);

        second_closed.send_replace(true);
        coordinator.prepare().await.unwrap();
        assert_eq!(*capture_state.markers.lock().unwrap(), [11, 22, 33]);
        assert_eq!(capture_state.drops.load(Ordering::Acquire), 2);
        assert_eq!(coordinator.portal.establishes.load(Ordering::Acquire), 3);
        drop(first_closed);
    }

    #[tokio::test]
    async fn coordinator_binds_frame_and_exact_granted_devices() {
        let (connection, _) = test_connection(9, 44);
        let capture_state = Arc::new(FakeCaptureState::default());
        let coordinator = ScreenshotCoordinator::new(
            FakePortal {
                connections: StdMutex::new(VecDeque::from([connection])),
                establishes: AtomicUsize::new(0),
            },
            FakeCaptureBackend(capture_state),
            SafeGeometryMapper,
            PngScreenshotEncoder,
        );
        coordinator.prepare().await.unwrap();
        let observation = coordinator.capture(&test_snapshot()).await.unwrap();
        assert_eq!(observation.mapping.source_frame_generation, 2);
        assert_eq!(observation.mapping.portal_session_generation, 9);
        assert_eq!(
            observation.mapping.remote_desktop_devices,
            GrantedDevices::from_mask_for_mapping(3)
        );
        assert!(observation.mapping.remote_desktop_devices.keyboard());
        assert!(observation.mapping.remote_desktop_devices.pointer());

        let matching = OwnedFrame {
            stream_index: 0,
            generation: observation.mapping.source_frame_generation + 1,
            format_generation: observation.mapping.source_format_generation,
            width: 2,
            height: 2,
            rgba: vec![0; 16],
            crop: observation.mapping.original_frame_crop,
            transform: observation.mapping.transform,
        };
        assert!(verify_current_frame_metadata(&matching, &observation.mapping).is_ok());
        let mut same_geometry_new_format = matching.clone();
        same_geometry_new_format.format_generation += 1;
        assert!(
            verify_current_frame_metadata(&same_geometry_new_format, &observation.mapping)
                .unwrap_err()
                .contains("renegotiated")
        );
        let mut renegotiated = matching;
        renegotiated.width = 3;
        assert!(
            verify_current_frame_metadata(&renegotiated, &observation.mapping)
                .unwrap_err()
                .contains("renegotiated")
        );
    }
}
