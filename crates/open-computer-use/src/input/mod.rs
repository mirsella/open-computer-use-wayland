pub mod backend;
pub mod coordinates;
pub mod eis;
pub mod keyboard;
pub mod keyboard_input;
pub mod pointer;

use crate::{accessibility::Snapshot, screenshot::ScreenshotMapping, validation::MouseButton};

pub type GeneratedInputFuture<'a> = backend::InputFuture<'a>;

#[derive(Debug, Clone, PartialEq)]
pub enum GeneratedInputAction {
    MovePointer {
        x: f64,
        y: f64,
    },
    Click {
        x: f64,
        y: f64,
        button: MouseButton,
        count: usize,
    },
    Drag {
        from: (f64, f64),
        to: (f64, f64),
    },
    Scroll {
        x: f64,
        y: f64,
        delta_x: i32,
        delta_y: i32,
    },
    PressKey {
        focus: (f64, f64),
        key: String,
    },
    TypeText {
        focus: (f64, f64),
        text: String,
    },
}

pub trait GeneratedInputProvider: Send + Sync + 'static {
    fn prepare_input<'a>(
        &'a self,
        _snapshot: &'a Snapshot,
        _mapping: &'a ScreenshotMapping,
        _action: &'a GeneratedInputAction,
    ) -> GeneratedInputFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    fn perform_input<'a>(
        &'a self,
        snapshot: &'a Snapshot,
        mapping: &'a ScreenshotMapping,
        action: GeneratedInputAction,
    ) -> GeneratedInputFuture<'a>;
    fn cleanup_input(&self) -> backend::InputFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn shutdown_input(&self) -> backend::InputFuture<'_> {
        self.cleanup_input()
    }
}

impl GeneratedInputProvider for crate::screenshot::NoScreenshots {
    fn perform_input<'a>(
        &'a self,
        _snapshot: &'a Snapshot,
        _mapping: &'a ScreenshotMapping,
        _action: GeneratedInputAction,
    ) -> GeneratedInputFuture<'a> {
        Box::pin(async { Err("generated input requires a live screenshot provider".into()) })
    }
}
