use xkbcommon::xkb;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyToken {
    pub name: String,
    pub keysym: u32,
    pub modifier: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord {
    pub modifiers: Vec<KeyToken>,
    pub key: KeyToken,
}

pub fn parse_chord(value: &str) -> Result<KeyChord, String> {
    let parts = value.split('+').map(str::trim).collect::<Vec<_>>();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err("key chord contains an empty key name".into());
    }
    let mut tokens = parts
        .iter()
        .map(|part| parse_token(part))
        .collect::<Result<Vec<_>, _>>()?;
    let key = tokens
        .pop()
        .ok_or_else(|| "key chord is empty".to_owned())?;
    if key.modifier {
        return Err("key chord must end with a non-modifier key".into());
    }
    if let Some(token) = tokens.iter().find(|token| !token.modifier) {
        return Err(format!(
            "non-modifier key {:?} appears before the end of the chord",
            token.name
        ));
    }
    Ok(KeyChord {
        modifiers: tokens,
        key,
    })
}

pub fn unicode_token(character: char) -> Result<KeyToken, String> {
    if character == '\0' {
        return Err("NUL text cannot be represented as keyboard input".into());
    }
    let keysym = xkb::utf32_to_keysym(character.into()).raw();
    if keysym == 0 {
        return Err(format!(
            "Unicode character U+{:04X} has no XKB keysym",
            u32::from(character)
        ));
    }
    Ok(KeyToken {
        name: character.to_string(),
        keysym,
        modifier: false,
    })
}

fn parse_token(value: &str) -> Result<KeyToken, String> {
    let normalized = value.to_ascii_lowercase();
    let (xkb_name, modifier) = match normalized.as_str() {
        "ctrl" | "control" => ("Control_L", true),
        "alt" => ("Alt_L", true),
        "shift" => ("Shift_L", true),
        "super" | "meta" | "win" | "windows" => ("Super_L", true),
        "return" | "enter" => ("Return", false),
        "tab" => ("Tab", false),
        "escape" | "esc" => ("Escape", false),
        "space" => ("space", false),
        "backspace" => ("BackSpace", false),
        "delete" | "del" => ("Delete", false),
        "insert" | "ins" => ("Insert", false),
        "home" => ("Home", false),
        "end" => ("End", false),
        "pageup" | "page_up" | "prior" => ("Page_Up", false),
        "pagedown" | "page_down" | "next" => ("Page_Down", false),
        "left" => ("Left", false),
        "right" => ("Right", false),
        "up" => ("Up", false),
        "down" => ("Down", false),
        "kp_0" | "kp0" => ("KP_0", false),
        "kp_1" | "kp1" => ("KP_1", false),
        "kp_2" | "kp2" => ("KP_2", false),
        "kp_3" | "kp3" => ("KP_3", false),
        "kp_4" | "kp4" => ("KP_4", false),
        "kp_5" | "kp5" => ("KP_5", false),
        "kp_6" | "kp6" => ("KP_6", false),
        "kp_7" | "kp7" => ("KP_7", false),
        "kp_8" | "kp8" => ("KP_8", false),
        "kp_9" | "kp9" => ("KP_9", false),
        "kp_enter" => ("KP_Enter", false),
        "kp_add" | "kp_plus" => ("KP_Add", false),
        "kp_subtract" | "kp_minus" => ("KP_Subtract", false),
        "kp_multiply" => ("KP_Multiply", false),
        "kp_divide" => ("KP_Divide", false),
        "kp_decimal" => ("KP_Decimal", false),
        name if function_key(name).is_some() => (function_key(name).expect("checked above"), false),
        _ => {
            let mut characters = value.chars();
            let Some(character) = characters.next() else {
                return Err("key name is empty".into());
            };
            if characters.next().is_none() {
                // Chord key names are case-insensitive. Literal text preserves case through
                // `unicode_token` directly and must request Shift when the keymap requires it.
                return unicode_token(character.to_ascii_lowercase());
            }
            return Err(format!("unknown key name {value:?}"));
        }
    };
    let keysym = xkb::keysym_from_name(xkb_name, xkb::KEYSYM_CASE_INSENSITIVE).raw();
    if keysym == 0 {
        return Err(format!("XKB does not recognize key name {xkb_name:?}"));
    }
    Ok(KeyToken {
        name: value.to_owned(),
        keysym,
        modifier,
    })
}

fn function_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "f1" => "F1",
        "f2" => "F2",
        "f3" => "F3",
        "f4" => "F4",
        "f5" => "F5",
        "f6" => "F6",
        "f7" => "F7",
        "f8" => "F8",
        "f9" => "F9",
        "f10" => "F10",
        "f11" => "F11",
        "f12" => "F12",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aliases_keypad_and_modifier_order() {
        let chord = parse_chord("alt+Control+KP_0").unwrap();
        assert_eq!(chord.modifiers[0].keysym, xkb::Keysym::Alt_L.raw());
        assert_eq!(chord.modifiers[1].keysym, xkb::Keysym::Control_L.raw());
        assert_eq!(chord.key.keysym, xkb::Keysym::KP_0.raw());
        assert!(parse_chord("ctrl+wat").unwrap_err().contains("unknown"));
        assert_eq!(
            parse_chord("Ctrl+L").unwrap().key.keysym,
            xkb::Keysym::l.raw()
        );
        assert!(parse_chord("a+ctrl").is_err());
        for name in [
            "Return",
            "Tab",
            "Escape",
            "Space",
            "F1",
            "F12",
            "Left",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Insert",
            "Delete",
            "Backspace",
            "KP_Enter",
        ] {
            parse_chord(name).unwrap();
        }
    }

    #[test]
    fn unicode_tokens_are_xkb_keysyms() {
        assert_ne!(unicode_token('é').unwrap().keysym, 0);
        assert_ne!(unicode_token('🙂').unwrap().keysym, 0);
        assert!(unicode_token('\0').is_err());
    }
}
