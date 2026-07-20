#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn is_valid_within(self, bounds: (u32, u32)) -> bool {
        self.width > 0
            && self.height > 0
            && self
                .x
                .checked_add(self.width)
                .is_some_and(|right| right <= bounds.0)
            && self
                .y
                .checked_add(self.height)
                .is_some_and(|bottom| bottom <= bounds.1)
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
pub struct MonitorGeometry {
    pub position: Option<(i32, i32)>,
    pub logical_size: Option<(i32, i32)>,
    pub frame_size: (u32, u32),
    pub frame_crop: PixelRect,
    pub transform: Transform,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorMapping {
    pub transformed_crop: PixelRect,
    pub scale_x: f64,
    pub scale_y: f64,
}

pub fn map_monitor(monitor: &MonitorGeometry) -> Result<MonitorMapping, String> {
    if !monitor.frame_crop.is_valid_within(monitor.frame_size) {
        return Err("monitor stream has out-of-bounds crop metadata".into());
    }
    monitor
        .position
        .ok_or_else(|| "monitor stream has no global compositor position".to_owned())?;
    let (logical_width, logical_height) = monitor
        .logical_size
        .ok_or_else(|| "monitor stream has no compositor logical size".to_owned())?;
    if logical_width <= 0 || logical_height <= 0 {
        return Err("monitor stream has invalid compositor logical size".into());
    }

    let (display_width, display_height) = if monitor.transform.swaps_axes() {
        (monitor.frame_crop.height, monitor.frame_crop.width)
    } else {
        (monitor.frame_crop.width, monitor.frame_crop.height)
    };
    let scale_x = f64::from(display_width) / f64::from(logical_width);
    let scale_y = f64::from(display_height) / f64::from(logical_height);
    if !scale_x.is_finite() || !scale_y.is_finite() || scale_x <= 0.0 || scale_y <= 0.0 {
        return Err("monitor stream has an invalid scale".into());
    }

    Ok(MonitorMapping {
        transformed_crop: PixelRect {
            x: 0,
            y: 0,
            width: display_width,
            height: display_height,
        },
        scale_x,
        scale_y,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(position: (i32, i32), logical: (i32, i32)) -> MonitorGeometry {
        MonitorGeometry {
            position: Some(position),
            logical_size: Some(logical),
            frame_size: (2400, 1600),
            frame_crop: PixelRect {
                x: 0,
                y: 0,
                width: 2400,
                height: 1600,
            },
            transform: Transform::Normal,
        }
    }

    #[test]
    fn maps_the_complete_monitor_with_fractional_scale() {
        let mapping = map_monitor(&monitor((-1600, 0), (1600, 1067))).unwrap();
        assert_eq!(mapping.transformed_crop.x, 0);
        assert_eq!(mapping.transformed_crop.width, 2400);
        assert!((mapping.scale_y - (1600.0 / 1067.0)).abs() < 0.0001);
    }

    #[test]
    fn rejects_missing_or_invalid_metadata() {
        let mut missing = monitor((0, 0), (1000, 800));
        missing.logical_size = None;
        assert!(map_monitor(&missing).unwrap_err().contains("logical size"));

        let mut invalid_crop = monitor((0, 0), (1000, 800));
        invalid_crop.frame_crop.width = 2401;
        assert!(
            map_monitor(&invalid_crop)
                .unwrap_err()
                .contains("crop metadata")
        );
    }

    #[test]
    fn rotation_swaps_pixel_axes() {
        let mut monitor = monitor((0, 0), (800, 1200));
        monitor.transform = Transform::Rotate90;
        let mapping = map_monitor(&monitor).unwrap();
        assert_eq!(mapping.transformed_crop.width, 1600);
        assert_eq!(mapping.transformed_crop.height, 2400);
    }

    #[test]
    fn pixel_rect_validity_rejects_empty_overflow_and_out_of_bounds_rects() {
        let valid = PixelRect {
            x: 2,
            y: 3,
            width: 4,
            height: 5,
        };
        assert!(valid.is_valid_within((6, 8)));
        assert!(!PixelRect { width: 0, ..valid }.is_valid_within((6, 8)));
        assert!(
            !PixelRect {
                x: u32::MAX,
                ..valid
            }
            .is_valid_within((u32::MAX, 8))
        );
        assert!(!PixelRect { width: 5, ..valid }.is_valid_within((6, 8)));
    }
}
