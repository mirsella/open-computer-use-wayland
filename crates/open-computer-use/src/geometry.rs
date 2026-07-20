#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn right(self) -> Option<u32> {
        self.x.checked_add(self.width)
    }

    pub fn bottom(self) -> Option<u32> {
        self.y.checked_add(self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
    Flip,
    FlipRotate90,
    FlipRotate180,
    FlipRotate270,
}

impl Transform {
    pub fn swaps_axes(self) -> bool {
        matches!(
            self,
            Self::Rotate90 | Self::Rotate270 | Self::FlipRotate90 | Self::FlipRotate270
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamGeometry {
    pub stream_index: usize,
    pub position: Option<(i32, i32)>,
    pub logical_size: Option<(i32, i32)>,
    pub frame_size: (u32, u32),
    pub frame_crop: Option<PixelRect>,
    pub transform: Transform,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CropMapping {
    pub stream_index: usize,
    pub transformed_crop: PixelRect,
    pub source_frame_crop: PixelRect,
    pub scale_x: f64,
    pub scale_y: f64,
    pub transform: Transform,
}

pub trait GeometryMapper: Send + Sync + 'static {
    fn map(&self, streams: &[StreamGeometry]) -> Result<CropMapping, String>;
}

#[derive(Debug, Default)]
pub struct SafeGeometryMapper;

impl GeometryMapper for SafeGeometryMapper {
    fn map(&self, streams: &[StreamGeometry]) -> Result<CropMapping, String> {
        map_monitor(streams)
    }
}

pub fn map_monitor(streams: &[StreamGeometry]) -> Result<CropMapping, String> {
    let [stream] = streams else {
        return Err(format!(
            "expected exactly one approved monitor stream, received {}",
            streams.len()
        ));
    };
    map_one(stream)
}

fn map_one(stream: &StreamGeometry) -> Result<CropMapping, String> {
    let crop = stream
        .frame_crop
        .ok_or_else(|| format!("stream {} has no valid crop metadata", stream.stream_index))?;
    validate_crop(crop, stream.frame_size, stream.stream_index)?;
    stream.position.ok_or_else(|| {
        format!(
            "stream {} has no global compositor position",
            stream.stream_index
        )
    })?;
    let (logical_width, logical_height) = stream.logical_size.ok_or_else(|| {
        format!(
            "stream {} has no compositor logical size",
            stream.stream_index
        )
    })?;
    if logical_width <= 0 || logical_height <= 0 {
        return Err(format!(
            "stream {} has invalid compositor logical size",
            stream.stream_index
        ));
    }

    let (display_width, display_height) = if stream.transform.swaps_axes() {
        (crop.height, crop.width)
    } else {
        (crop.width, crop.height)
    };
    let scale_x = f64::from(display_width) / f64::from(logical_width);
    let scale_y = f64::from(display_height) / f64::from(logical_height);
    if !scale_x.is_finite() || !scale_y.is_finite() || scale_x <= 0.0 || scale_y <= 0.0 {
        return Err(format!(
            "stream {} has an invalid scale",
            stream.stream_index
        ));
    }

    Ok(CropMapping {
        stream_index: stream.stream_index,
        transformed_crop: PixelRect {
            x: 0,
            y: 0,
            width: display_width,
            height: display_height,
        },
        source_frame_crop: crop,
        scale_x,
        scale_y,
        transform: stream.transform,
    })
}

fn validate_crop(crop: PixelRect, frame: (u32, u32), index: usize) -> Result<(), String> {
    if crop.width == 0
        || crop.height == 0
        || crop.right().is_none_or(|right| right > frame.0)
        || crop.bottom().is_none_or(|bottom| bottom > frame.1)
    {
        return Err(format!("stream {index} has out-of-bounds crop metadata"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(index: usize, position: (i32, i32), logical: (i32, i32)) -> StreamGeometry {
        StreamGeometry {
            stream_index: index,
            position: Some(position),
            logical_size: Some(logical),
            frame_size: (2400, 1600),
            frame_crop: Some(PixelRect {
                x: 0,
                y: 0,
                width: 2400,
                height: 1600,
            }),
            transform: Transform::Normal,
        }
    }

    #[test]
    fn maps_the_complete_monitor_with_fractional_scale() {
        let mapping = map_monitor(&[monitor(0, (-1600, 0), (1600, 1067))]).unwrap();
        assert_eq!(mapping.transformed_crop.x, 0);
        assert_eq!(mapping.transformed_crop.width, 2400);
        assert!((mapping.scale_y - (1600.0 / 1067.0)).abs() < 0.0001);
    }

    #[test]
    fn rejects_multiple_streams_and_missing_metadata() {
        assert!(
            map_monitor(&[
                monitor(0, (0, 0), (1000, 800)),
                monitor(1, (0, 0), (1000, 800))
            ])
            .unwrap_err()
            .contains("exactly one")
        );
        let mut missing = monitor(0, (0, 0), (1000, 800));
        missing.frame_crop = None;
        assert!(
            map_monitor(&[missing])
                .unwrap_err()
                .contains("crop metadata")
        );
    }

    #[test]
    fn rotation_swaps_pixel_axes() {
        let mut stream = monitor(0, (0, 0), (800, 1200));
        stream.transform = Transform::Rotate90;
        let mapping = map_monitor(&[stream]).unwrap();
        assert_eq!(mapping.transformed_crop.width, 1600);
        assert_eq!(mapping.transformed_crop.height, 2400);
    }
}
