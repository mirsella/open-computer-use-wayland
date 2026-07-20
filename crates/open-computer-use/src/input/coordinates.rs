use crate::{
    accessibility::Snapshot,
    portal::{PortalSessionLease, PortalStream},
    screenshot::ScreenshotMapping,
};

#[derive(Debug, Clone, Copy)]
pub struct ValidatedMapping<'a> {
    mapping: &'a ScreenshotMapping,
}

impl<'a> ValidatedMapping<'a> {
    pub fn new(
        snapshot: &Snapshot,
        mapping: &'a ScreenshotMapping,
        session: &PortalSessionLease,
        stream: &PortalStream,
    ) -> Result<Self, String> {
        validate_mapping(snapshot, mapping, session, stream)?;
        Ok(Self { mapping })
    }

    pub fn eis_mapper(self, region: EisRegion) -> Result<AbsoluteMapper<'a>, String> {
        let position = self
            .mapping
            .stream
            .position
            .ok_or_else(|| "selected monitor stream has no compositor position".to_owned())?;
        let size = self
            .mapping
            .stream
            .logical_size
            .ok_or_else(|| "selected monitor stream has no logical size".to_owned())?;
        if region.position != position || region.size != size {
            return Err(
                "selected EIS region geometry does not exactly match the monitor stream".into(),
            );
        }
        if let Some(mapping_id) = self.mapping.stream.mapping_id.as_deref()
            && region.mapping_id.as_deref() != Some(mapping_id)
        {
            return Err("selected EIS region mapping_id does not match the monitor stream".into());
        }
        Ok(AbsoluteMapper {
            mapping: self.mapping,
            region,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EisRegion {
    pub position: (i32, i32),
    pub size: (i32, i32),
    pub mapping_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AbsoluteMapper<'a> {
    mapping: &'a ScreenshotMapping,
    region: EisRegion,
}

impl AbsoluteMapper<'_> {
    pub fn point(&self, png_x: f64, png_y: f64) -> Result<(f64, f64), String> {
        if !png_x.is_finite() || !png_y.is_finite() {
            return Err("screenshot coordinates must be finite".into());
        }
        let (png_width, png_height) = self.mapping.output.size;
        if png_x < 0.0
            || png_y < 0.0
            || png_x >= f64::from(png_width)
            || png_y >= f64::from(png_height)
        {
            return Err(format!(
                "screenshot coordinate ({png_x}, {png_y}) is outside exact PNG bounds [0, {png_width}) x [0, {png_height})"
            ));
        }

        let transformed_x = f64::from(self.mapping.monitor.transformed_crop.x)
            + png_x * self.mapping.output.png_to_transformed_x;
        let transformed_y = f64::from(self.mapping.monitor.transformed_crop.y)
            + png_y * self.mapping.output.png_to_transformed_y;
        let x = transformed_x / self.mapping.monitor.scale_x;
        let y = transformed_y / self.mapping.monitor.scale_y;
        let (logical_width, logical_height) =
            self.mapping.stream.logical_size.ok_or_else(|| {
                "selected stream has no logical size for absolute input mapping".to_owned()
            })?;
        if x < 0.0 || y < 0.0 || x >= f64::from(logical_width) || y >= f64::from(logical_height) {
            return Err("inverse screenshot mapping produced an out-of-bounds stream point".into());
        }
        let global_x = x + f64::from(self.region.position.0);
        let global_y = y + f64::from(self.region.position.1);
        let protocol_x = f64::from(global_x as f32);
        let protocol_y = f64::from(global_y as f32);
        let right = f64::from(self.region.position.0) + f64::from(self.region.size.0);
        let bottom = f64::from(self.region.position.1) + f64::from(self.region.size.1);
        if protocol_x < f64::from(self.region.position.0)
            || protocol_y < f64::from(self.region.position.1)
            || protocol_x >= right
            || protocol_y >= bottom
        {
            return Err("screenshot coordinate cannot be represented inside the EIS region".into());
        }
        Ok((protocol_x, protocol_y))
    }
}

fn validate_mapping(
    snapshot: &Snapshot,
    mapping: &ScreenshotMapping,
    session: &PortalSessionLease,
    stream: &PortalStream,
) -> Result<(), String> {
    if mapping.app_pid != snapshot.app.pid
        || mapping.app_identity != snapshot.app.object
        || mapping.window_identity != snapshot.window.object
        || mapping.accessibility_generation != snapshot.generation
    {
        return Err("screenshot mapping is stale for the current app/window generation".into());
    }
    if session.is_closed()
        || session.identity() != mapping.portal_session_identity
        || session.generation() != mapping.portal_session_generation
    {
        return Err("portal session identity is stale or closed".into());
    }
    if session.granted_devices() != mapping.remote_desktop_devices {
        return Err("portal session granted-device mask changed since screenshot capture".into());
    }
    if !mapping.monitor.scale_x.is_finite()
        || !mapping.monitor.scale_y.is_finite()
        || mapping.monitor.scale_x <= 0.0
        || mapping.monitor.scale_y <= 0.0
        || !mapping.output.png_to_transformed_x.is_finite()
        || !mapping.output.png_to_transformed_y.is_finite()
        || mapping.output.png_to_transformed_x <= 0.0
        || mapping.output.png_to_transformed_y <= 0.0
        || mapping.output.size.0 == 0
        || mapping.output.size.1 == 0
    {
        return Err("screenshot mapping has invalid scale or output bounds".into());
    }
    if stream != &mapping.stream {
        return Err("live portal stream metadata changed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{
        accessibility::{AppInfo, ObjectId, Snapshot, SnapshotLimits, WindowInfo},
        capture::FrameMetadata,
        encoder::{PngMapping, encode_with_limits},
        geometry::{MonitorMapping, PixelRect, Transform},
        portal::GrantedDevices,
    };

    fn fixture(transform: Transform) -> (Snapshot, ScreenshotMapping, PortalStream) {
        let window = ObjectId {
            bus_name: ":1.2".into(),
            path: "/window".into(),
        };
        let snapshot = Snapshot {
            app_query: "App".into(),
            app: AppInfo {
                object: ObjectId {
                    bus_name: ":1.2".into(),
                    path: "/app".into(),
                },
                name: "App".into(),
                pid: 9,
                windows: Vec::new(),
            },
            window: WindowInfo {
                object: window.clone(),
                title: "Window".into(),
                states: BTreeSet::from(["active".into()]),
            },
            generation: 7,
            elements: Vec::new(),
            node_limit_reached: false,
            depth_limit_reached: false,
            limits: SnapshotLimits {
                text: 20,
                nodes: 20,
                depth: 5,
            },
        };
        let mapping = ScreenshotMapping {
            app_pid: 9,
            app_identity: snapshot.app.object.clone(),
            window_identity: window,
            accessibility_generation: 7,
            portal_session_identity: "/session/test".into(),
            portal_session_generation: 4,
            remote_desktop_devices: GrantedDevices::from_mask_for_mapping(3),
            stream: PortalStream {
                stream_index: 0,
                node_id: 22,
                pipewire_serial: Some(33),
                id: Some("stream".into()),
                mapping_id: Some("map".into()),
                position: Some((-1600, 0)),
                logical_size: Some((400, 300)),
            },
            source: FrameMetadata {
                generation: 8,
                format_generation: 1,
                size: (600, 450),
                crop: PixelRect {
                    x: 0,
                    y: 0,
                    width: 600,
                    height: 450,
                },
                transform,
            },
            monitor: MonitorMapping {
                transformed_crop: PixelRect {
                    x: 0,
                    y: 0,
                    width: 600,
                    height: 450,
                },
                scale_x: 1.5,
                scale_y: 1.5,
            },
            output: PngMapping {
                size: (600, 450),
                png_to_transformed_x: 1.0,
                png_to_transformed_y: 1.0,
            },
        };
        let stream = PortalStream {
            stream_index: 0,
            node_id: 22,
            pipewire_serial: Some(33),
            id: Some("stream".into()),
            mapping_id: Some("map".into()),
            position: Some((-1600, 0)),
            logical_size: Some((400, 300)),
        };
        (snapshot, mapping, stream)
    }

    fn map_png_point(
        snapshot: &Snapshot,
        mapping: &ScreenshotMapping,
        session: &PortalSessionLease,
        stream: &PortalStream,
        x: f64,
        y: f64,
    ) -> Result<(f64, f64), String> {
        ValidatedMapping::new(snapshot, mapping, session, stream)?
            .eis_mapper(EisRegion {
                position: mapping.stream.position.unwrap(),
                size: mapping.stream.logical_size.unwrap(),
                mapping_id: mapping.stream.mapping_id.clone(),
            })?
            .point(x, y)
    }

    #[test]
    fn maps_all_transforms_negative_origins_and_fractional_scale() {
        for transform in [
            Transform::Normal,
            Transform::Rotate90,
            Transform::Rotate180,
            Transform::Rotate270,
            Transform::Flip,
            Transform::FlipRotate90,
            Transform::FlipRotate180,
            Transform::FlipRotate270,
        ] {
            let (snapshot, mapping, stream) = fixture(transform);
            let (session, _) = PortalSessionLease::for_test("/session/test", 4, 3);
            let point = map_png_point(&snapshot, &mapping, &session, &stream, 75.0, 30.0).unwrap();
            assert_eq!(point.0, -1550.0, "{transform:?}");
            assert_eq!(point.1, 20.0, "{transform:?}");
        }
    }

    #[test]
    fn rejects_exact_edges_stale_state_and_changed_streams() {
        let (snapshot, mapping, stream) = fixture(Transform::Normal);
        let (session, closed) = PortalSessionLease::for_test("/session/test", 4, 3);
        assert!(map_png_point(&snapshot, &mapping, &session, &stream, 599.999, 0.0).is_ok());
        assert!(
            map_png_point(&snapshot, &mapping, &session, &stream, 600.0, 0.0,)
                .unwrap_err()
                .contains("bounds")
        );
        assert!(map_png_point(&snapshot, &mapping, &session, &stream, -0.01, 0.0,).is_err());

        let mut stale = snapshot.clone();
        stale.generation += 1;
        assert!(
            map_png_point(&stale, &mapping, &session, &stream, 1.0, 1.0,)
                .unwrap_err()
                .contains("stale")
        );
        let mut changed = stream.clone();
        changed.logical_size = Some((401, 300));
        assert!(
            map_png_point(&snapshot, &mapping, &session, &changed, 1.0, 1.0)
                .unwrap_err()
                .contains("changed")
        );
        let (different_grants, _) = PortalSessionLease::for_test("/session/test", 4, 1);
        assert!(
            map_png_point(&snapshot, &mapping, &different_grants, &stream, 1.0, 1.0)
                .unwrap_err()
                .contains("granted-device mask")
        );
        closed.send_replace(true);
        assert!(
            map_png_point(&snapshot, &mapping, &session, &stream, 1.0, 1.0)
                .unwrap_err()
                .contains("closed")
        );
    }

    #[test]
    fn actual_encoder_resize_inverts_crop_transform_fractional_scale_and_byte_limit() {
        let width = 80_u32;
        let height = 60_u32;
        let mut rgba = vec![0_u8; (width * height * 4) as usize];
        for (index, byte) in rgba.iter_mut().enumerate() {
            *byte = ((index * 73 + index / 11) & 0xff) as u8;
        }
        let output_crop = PixelRect {
            x: 6,
            y: 10,
            width: 40,
            height: 50,
        };
        let encoded = encode_with_limits(
            rgba,
            (width, height),
            PixelRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            Transform::Rotate90,
            output_crop,
            30,
            1_000,
        )
        .unwrap();
        assert!(encoded.mapping.size.0.max(encoded.mapping.size.1) <= 30);
        assert!(encoded.bytes.len() <= 1_000);
        assert!(encoded.mapping.png_to_transformed_x > 1.0);
        assert!(encoded.mapping.png_to_transformed_y > 1.0);

        let (snapshot, mut mapping, mut stream) = fixture(Transform::Rotate90);
        mapping.monitor.transformed_crop = output_crop;
        mapping.output = encoded.mapping;
        mapping.monitor.scale_x = 1.25;
        mapping.monitor.scale_y = 1.5;
        mapping.stream.logical_size = Some((1000, 1000));
        stream.logical_size = Some((1000, 1000));
        let (session, _) = PortalSessionLease::for_test("/session/test", 4, 3);
        let png_x = f64::from(mapping.output.size.0) * 0.25;
        let png_y = f64::from(mapping.output.size.1) * 0.75;
        let point = map_png_point(&snapshot, &mapping, &session, &stream, png_x, png_y).unwrap();
        let expected_x =
            -1600.0 + (f64::from(output_crop.x) + f64::from(output_crop.width) * 0.25) / 1.25;
        let expected_y = (f64::from(output_crop.y) + f64::from(output_crop.height) * 0.75) / 1.5;
        assert!((point.0 - expected_x).abs() < 0.000_1);
        assert!((point.1 - expected_y).abs() < 0.000_1);
    }
}
