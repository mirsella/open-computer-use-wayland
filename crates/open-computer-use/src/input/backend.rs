use std::{future::Future, pin::Pin, sync::Arc};

pub type InputFuture<'a, T = ()> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputCapabilities {
    pub absolute_pointer: bool,
    pub button: bool,
    pub scroll: bool,
    pub keyboard: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    Absolute { x: f64, y: f64 },
    Button { code: u32, pressed: bool },
    ScrollDiscrete { x: i32, y: i32 },
    Keycode { key: KeyboardKey, pressed: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardKey {
    pub device_id: u64,
    pub resume_generation: u64,
    pub keycode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldInput {
    Button(u32),
    Keycode(KeyboardKey),
}

impl HeldInput {
    pub fn release_event(self) -> InputEvent {
        match self {
            Self::Button(code) => InputEvent::Button {
                code,
                pressed: false,
            },
            Self::Keycode(key) => InputEvent::Keycode {
                key,
                pressed: false,
            },
        }
    }
}

pub trait InputBackend: Send + Sync + 'static {
    fn capabilities(&self) -> InputCapabilities;
    fn begin_sequence(&self) -> InputFuture<'_>;
    fn emit(&self, event: InputEvent) -> InputFuture<'_>;
    fn queue_release(&self, held: Vec<HeldInput>);
    fn cleanup_barrier(&self) -> InputFuture<'_>;
}

pub struct HeldInputGuard {
    backend: Arc<dyn InputBackend>,
    held: Vec<HeldInput>,
    disarmed: bool,
    sequence_started: bool,
}

impl HeldInputGuard {
    pub fn new(backend: Arc<dyn InputBackend>) -> Self {
        Self {
            backend,
            held: Vec::new(),
            disarmed: false,
            sequence_started: false,
        }
    }

    pub async fn begin(&mut self) -> Result<(), String> {
        self.backend.begin_sequence().await?;
        self.sequence_started = true;
        Ok(())
    }

    pub async fn press(&mut self, held: HeldInput) -> Result<(), String> {
        self.held.push(held);
        let event = match held {
            HeldInput::Button(code) => InputEvent::Button {
                code,
                pressed: true,
            },
            HeldInput::Keycode(key) => InputEvent::Keycode { key, pressed: true },
        };
        self.backend.emit(event).await
    }

    pub async fn release(&mut self, held: HeldInput) -> Result<(), String> {
        self.backend.emit(held.release_event()).await?;
        if let Some(index) = self.held.iter().rposition(|candidate| *candidate == held) {
            self.held.remove(index);
        }
        Ok(())
    }

    pub async fn release_all(&mut self) -> Result<(), String> {
        let mut first_error = None;
        while let Some(held) = self.held.last().copied() {
            if let Err(error) = self.backend.emit(held.release_event()).await {
                eprintln!("open-computer-use: failed to release held input: {error}");
                first_error.get_or_insert(error);
                self.backend.queue_release(vec![held]);
            }
            self.held.pop();
        }
        if let Err(error) = self.backend.cleanup_barrier().await {
            eprintln!("open-computer-use: cleanup barrier failed: {error}");
            first_error.get_or_insert(error);
        }
        self.disarmed = true;
        self.sequence_started = false;
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for HeldInputGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        if self.held.is_empty() && !self.sequence_started {
            return;
        }
        let held = self.held.drain(..).rev().collect();
        self.backend.queue_release(held);
    }
}

pub async fn finish_with_cleanup<T>(
    result: Result<T, String>,
    guard: &mut HeldInputGuard,
) -> Result<T, String> {
    match result {
        Ok(value) => {
            guard.release_all().await?;
            Ok(value)
        }
        Err(original) => {
            if let Err(cleanup) = guard.release_all().await {
                eprintln!(
                    "open-computer-use: held-input cleanup also failed after {original}: {cleanup}"
                );
                return Err(format!(
                    "{original}; held-input cleanup also failed and the input session was invalidated: {cleanup}"
                ));
            }
            Err(original)
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    pub struct FakeBackend {
        pub capabilities: InputCapabilities,
        pub events: Mutex<Vec<InputEvent>>,
        pub emergency: Mutex<Vec<HeldInput>>,
        pub fail_at: AtomicUsize,
    }

    impl FakeBackend {
        pub fn new(capabilities: InputCapabilities) -> Arc<Self> {
            Arc::new(Self {
                capabilities,
                events: Mutex::new(Vec::new()),
                emergency: Mutex::new(Vec::new()),
                fail_at: AtomicUsize::new(usize::MAX),
            })
        }
    }

    impl InputBackend for FakeBackend {
        fn capabilities(&self) -> InputCapabilities {
            self.capabilities
        }

        fn emit(&self, event: InputEvent) -> InputFuture<'_> {
            let index = self.events.lock().unwrap().len();
            let result = if index == self.fail_at.load(Ordering::Acquire) {
                Err(format!("fake failure at event {index}"))
            } else {
                self.events.lock().unwrap().push(event);
                Ok(())
            };
            Box::pin(async move { result })
        }

        fn begin_sequence(&self) -> InputFuture<'_> {
            Box::pin(async { Ok(()) })
        }

        fn queue_release(&self, held: Vec<HeldInput>) {
            self.emergency.lock().unwrap().extend(held);
        }

        fn cleanup_barrier(&self) -> InputFuture<'_> {
            let held = std::mem::take(&mut *self.emergency.lock().unwrap());
            for input in held {
                self.events.lock().unwrap().push(input.release_event());
            }
            Box::pin(async { Ok(()) })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use super::{test_support::FakeBackend, *};

    struct PendingReleaseBackend {
        queued: Mutex<Vec<HeldInput>>,
    }

    struct SequenceBackend {
        cleanup_called: AtomicBool,
        queued_empty: AtomicBool,
    }

    impl InputBackend for SequenceBackend {
        fn capabilities(&self) -> InputCapabilities {
            InputCapabilities::default()
        }

        fn begin_sequence(&self) -> InputFuture<'_> {
            Box::pin(async { Ok(()) })
        }

        fn emit(&self, _event: InputEvent) -> InputFuture<'_> {
            Box::pin(async { Ok(()) })
        }

        fn queue_release(&self, held: Vec<HeldInput>) {
            self.queued_empty.store(held.is_empty(), Ordering::Release);
        }

        fn cleanup_barrier(&self) -> InputFuture<'_> {
            self.cleanup_called.store(true, Ordering::Release);
            Box::pin(async { Ok(()) })
        }
    }

    impl InputBackend for PendingReleaseBackend {
        fn capabilities(&self) -> InputCapabilities {
            InputCapabilities::default()
        }

        fn emit(&self, event: InputEvent) -> InputFuture<'_> {
            Box::pin(async move {
                match event {
                    InputEvent::Keycode { pressed: false, .. } => future::pending().await,
                    _ => Ok(()),
                }
            })
        }

        fn begin_sequence(&self) -> InputFuture<'_> {
            Box::pin(async { Ok(()) })
        }

        fn queue_release(&self, held: Vec<HeldInput>) {
            self.queued.lock().unwrap().extend(held);
        }

        fn cleanup_barrier(&self) -> InputFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn guard_releases_in_reverse_and_drop_covers_cancellation() {
        let fake = FakeBackend::new(InputCapabilities::default());
        let backend: Arc<dyn InputBackend> = fake.clone();
        let mut guard = HeldInputGuard::new(backend);
        guard.press(HeldInput::Keycode(test_key(29))).await.unwrap();
        guard.press(HeldInput::Button(272)).await.unwrap();
        guard.release_all().await.unwrap();
        assert_eq!(
            &fake.events.lock().unwrap()[2..],
            &[
                InputEvent::Button {
                    code: 272,
                    pressed: false
                },
                InputEvent::Keycode {
                    key: test_key(29),
                    pressed: false
                }
            ]
        );

        let backend: Arc<dyn InputBackend> = fake.clone();
        let mut cancelled = HeldInputGuard::new(backend);
        cancelled
            .press(HeldInput::Keycode(test_key(10)))
            .await
            .unwrap();
        drop(cancelled);
        assert_eq!(
            *fake.emergency.lock().unwrap(),
            [HeldInput::Keycode(test_key(10))]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_during_release_keeps_the_input_queued() {
        let backend = Arc::new(PendingReleaseBackend {
            queued: Mutex::new(Vec::new()),
        });
        let mut guard = HeldInputGuard::new(backend.clone());
        guard.press(HeldInput::Keycode(test_key(10))).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), guard.release_all())
                .await
                .is_err()
        );
        drop(guard);
        assert_eq!(
            *backend.queued.lock().unwrap(),
            [HeldInput::Keycode(test_key(10))]
        );
    }

    #[tokio::test]
    async fn dropping_started_sequence_queues_cleanup_without_held_input() {
        let backend = Arc::new(SequenceBackend {
            cleanup_called: AtomicBool::new(false),
            queued_empty: AtomicBool::new(false),
        });
        let dynamic: Arc<dyn InputBackend> = backend.clone();
        let mut guard = HeldInputGuard::new(dynamic);
        guard.begin().await.unwrap();
        drop(guard);

        assert!(backend.queued_empty.load(Ordering::Acquire));
        backend.cleanup_barrier().await.unwrap();
        assert!(backend.cleanup_called.load(Ordering::Acquire));
    }

    fn test_key(keycode: u32) -> KeyboardKey {
        KeyboardKey {
            device_id: 1,
            resume_generation: 1,
            keycode,
        }
    }
}
