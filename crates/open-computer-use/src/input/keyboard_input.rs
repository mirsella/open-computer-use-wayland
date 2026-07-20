use std::{sync::Arc, time::Duration};

use tokio::time::sleep;

use super::{
    backend::{HeldInput, HeldInputGuard, InputBackend, InputEvent, KeyboardKey},
    eis::{ReisInputBackend, ResolvedKey},
    keyboard::{KeyChord, parse_chord, unicode_token},
    pointer::button_code,
};
use crate::validation::MouseButton;

const FOCUS_SETTLE_DELAY: Duration = Duration::from_millis(50);

pub async fn press_key(
    backend: Arc<ReisInputBackend>,
    focus: (f64, f64),
    chord: &str,
) -> Result<(), String> {
    let chord = parse_chord(chord)?;
    resolve_chord(&backend, &chord)?;
    let resolver = Arc::clone(&backend);
    focused_tap_sequence(backend, focus, move || resolve_chord(&resolver, &chord)).await
}

pub async fn type_text(
    backend: Arc<ReisInputBackend>,
    focus: (f64, f64),
    text: &str,
) -> Result<(), String> {
    let tokens = text
        .chars()
        .map(unicode_token)
        .collect::<Result<Vec<_>, _>>()?;
    let keysyms = tokens.iter().map(|token| token.keysym).collect::<Vec<_>>();
    resolve_text(&backend, &keysyms)?;
    let resolver = Arc::clone(&backend);
    focused_tap_sequence(backend, focus, move || resolve_text(&resolver, &keysyms)).await
}

fn resolve_chord(
    backend: &ReisInputBackend,
    chord: &KeyChord,
) -> Result<Vec<(Vec<KeyboardKey>, KeyboardKey)>, String> {
    let keysyms = chord
        .modifiers
        .iter()
        .map(|token| token.keysym)
        .chain(std::iter::once(chord.key.keysym))
        .collect::<Vec<_>>();
    let mut resolved = backend.resolve_keysyms(&keysyms)?;
    let key = resolved.pop().ok_or("key chord is empty")?;
    let mut modifiers = Vec::new();
    for modifier in resolved {
        modifiers.extend(
            modifier
                .modifiers
                .into_iter()
                .flatten()
                .map(|keycode| keyboard_key(&modifier, keycode)),
        );
        modifiers.push(keyboard_key(&modifier, modifier.keycode));
    }
    modifiers.extend(
        key.modifiers
            .into_iter()
            .flatten()
            .map(|keycode| keyboard_key(&key, keycode)),
    );
    modifiers.sort_unstable_by_key(|key| key.keycode);
    modifiers.dedup();
    Ok(vec![(modifiers, keyboard_key(&key, key.keycode))])
}

fn resolve_text(
    backend: &ReisInputBackend,
    keysyms: &[u32],
) -> Result<Vec<(Vec<KeyboardKey>, KeyboardKey)>, String> {
    backend
        .resolve_keysyms(keysyms)?
        .into_iter()
        .map(|key| {
            let modifiers = key
                .modifiers
                .into_iter()
                .flatten()
                .map(|keycode| keyboard_key(&key, keycode))
                .collect();
            Ok((modifiers, keyboard_key(&key, key.keycode)))
        })
        .collect()
}

fn keyboard_key(resolved: &ResolvedKey, keycode: u32) -> KeyboardKey {
    KeyboardKey {
        device_id: resolved.device_id,
        resume_generation: resolved.resume_generation,
        keycode,
    }
}

async fn focused_tap_sequence<B, F>(
    backend: Arc<B>,
    focus: (f64, f64),
    resolve: F,
) -> Result<(), String>
where
    B: InputBackend,
    F: FnOnce() -> Result<Vec<(Vec<KeyboardKey>, KeyboardKey)>, String>,
{
    let backend: Arc<dyn InputBackend> = backend;
    let mut guard = HeldInputGuard::new(Arc::clone(&backend));
    guard.begin().await?;
    let result = async {
        backend
            .emit(InputEvent::Absolute {
                x: focus.0,
                y: focus.1,
            })
            .await?;
        let button = HeldInput::Button(button_code(MouseButton::Left));
        guard.press(button).await?;
        guard.release(button).await?;
        sleep(FOCUS_SETTLE_DELAY).await;
        let keys = resolve()?;
        for (modifiers, keycode) in keys {
            for &modifier in &modifiers {
                guard.press(HeldInput::Keycode(modifier)).await?;
            }
            let key = HeldInput::Keycode(keycode);
            guard.press(key).await?;
            guard.release(key).await?;
            for &modifier in modifiers.iter().rev() {
                guard.release(HeldInput::Keycode(modifier)).await?;
            }
        }
        Ok(())
    }
    .await;
    super::backend::finish_with_cleanup(result, &mut guard).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::backend::{InputCapabilities, test_support::FakeBackend};

    #[tokio::test(start_paused = true)]
    async fn keyboard_sequence_focuses_the_visible_point_before_typing() {
        let backend = FakeBackend::new(InputCapabilities {
            absolute_pointer: true,
            button: true,
            keyboard: true,
            ..Default::default()
        });

        let modifier = KeyboardKey {
            device_id: 7,
            resume_generation: 2,
            keycode: 42,
        };
        let key = KeyboardKey {
            keycode: 30,
            ..modifier
        };
        focused_tap_sequence(Arc::clone(&backend), (125.5, 80.25), || {
            Ok(vec![(vec![modifier], key)])
        })
        .await
        .unwrap();

        assert_eq!(
            *backend.events.lock().unwrap(),
            vec![
                InputEvent::Absolute { x: 125.5, y: 80.25 },
                InputEvent::Button {
                    code: 272,
                    pressed: true,
                },
                InputEvent::Button {
                    code: 272,
                    pressed: false,
                },
                InputEvent::Keycode {
                    key: modifier,
                    pressed: true,
                },
                InputEvent::Keycode { key, pressed: true },
                InputEvent::Keycode {
                    key,
                    pressed: false,
                },
                InputEvent::Keycode {
                    key: modifier,
                    pressed: false,
                },
            ]
        );
    }
}
