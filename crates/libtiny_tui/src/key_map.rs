use std::collections::HashMap;
use std::fmt::{self, Display};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use serde::de::{MapAccess, Unexpected, Visitor};
use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Binding {
    Key(KeyCode, KeyModifiers),
    Mouse(MouseEventKind),
}

#[derive(Debug, PartialEq)]
pub(crate) struct KeyMap(HashMap<Binding, KeyAction>);

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KeyAction {
    Cancel,
    Disable,
    Exit,

    RunEditor,

    TabNext,
    TabPrev,
    TabMoveLeft,
    TabMoveRight,
    TabGoto(char),

    MessagesPageUp,
    MessagesPageDown,
    MessagesScrollUp,
    MessagesScrollDown,
    MessagesScrollTop,
    MessagesScrollBottom,

    Input(char),
    Command(String),
    InputAutoComplete,
    InputNextEntry,
    InputPrevEntry,
    InputSend,
    InputDeletePrevChar,
    InputDeleteNextChar,
    InputDeleteToStart,
    InputDeleteToEnd,
    InputDeletePrevWord,
    InputMoveCursEnd,
    InputMoveCursStart,
    InputMoveCursLeft,
    InputMoveCursRight,
    InputMoveWordLeft,
    InputMoveWordRight,
}

impl Default for KeyMap {
    fn default() -> Self {
        use KeyAction::*;
        use KeyCode::*;
        let none = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        let alt = KeyModifiers::ALT;
        let shift = KeyModifiers::SHIFT;
        let mut map = HashMap::new();
        for (code, modifiers, action) in [
            (Esc, none, Cancel),
            (Char('c'), ctrl, Exit),
            (Char('x'), ctrl, RunEditor),
            (Char('n'), ctrl, TabNext),
            (Char('p'), ctrl, TabPrev),
            (Left, alt, TabMoveLeft),
            (Right, alt, TabMoveRight),
            (Char('u'), ctrl, MessagesPageUp),
            (Char('d'), ctrl, MessagesPageDown),
            (PageUp, none, MessagesPageUp),
            (PageDown, none, MessagesPageDown),
            (Up, shift, MessagesScrollUp),
            (Down, shift, MessagesScrollDown),
            (Home, none, MessagesScrollTop),
            (End, none, MessagesScrollBottom),
            (Tab, none, InputAutoComplete),
            (Up, none, InputPrevEntry),
            (Down, none, InputNextEntry),
            (Enter, none, InputSend),
            (Backspace, none, InputDeletePrevChar),
            (Char('h'), ctrl, InputDeletePrevChar),
            (Delete, none, InputDeleteNextChar),
            (Char('a'), ctrl, InputMoveCursStart),
            (Char('e'), ctrl, InputMoveCursEnd),
            (Char('k'), ctrl, InputDeleteToEnd),
            (Char('w'), ctrl, InputDeletePrevWord),
            (Left, none, InputMoveCursLeft),
            (Right, none, InputMoveCursRight),
            (Left, ctrl, InputMoveWordLeft),
            (Right, ctrl, InputMoveWordRight),
        ] {
            map.insert(Binding::Key(code, modifiers), action);
        }
        for c in '0'..='9' {
            map.insert(Binding::Key(Char(c), alt), TabGoto(c));
        }
        map.insert(Binding::Mouse(MouseEventKind::ScrollUp), MessagesScrollUp);
        map.insert(
            Binding::Mouse(MouseEventKind::ScrollDown),
            MessagesScrollDown,
        );
        Self(map)
    }
}

impl KeyMap {
    pub(crate) fn get(&self, key: &KeyEvent) -> Option<KeyAction> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        // Character case is part of KeyCode, and traditional terminals report
        // uppercase characters with SHIFT even though legacy bindings do not.
        let modifiers = if matches!(key.code, KeyCode::Char(_)) {
            key.modifiers - KeyModifiers::SHIFT
        } else {
            key.modifiers
        };
        self.0.get(&Binding::Key(key.code, modifiers)).cloned()
    }

    pub(crate) fn get_mouse(&self, kind: MouseEventKind) -> Option<KeyAction> {
        self.0.get(&Binding::Mouse(kind)).cloned()
    }

    pub(crate) fn load(&mut self, key_map: &KeyMap) {
        self.0.extend(key_map.0.clone())
    }
}

impl<'de> Deserialize<'de> for KeyMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct KeyMapVisitor;

        impl<'de> Visitor<'de> for KeyMapVisitor {
            type Value = KeyMap;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(
                    formatter,
                    "a map of key bindings. ex. 'ctrl_a: input_move_curs_start'"
                )?;
                write!(formatter, "defaults:\n{}", KeyMap::default())
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut key_map = HashMap::new();
                while let Some((key, action)) = map.next_entry::<MappedKey, KeyAction>()? {
                    key_map.insert(key.0, action);
                }
                Ok(KeyMap(key_map))
            }
        }
        deserializer.deserialize_map(KeyMapVisitor)
    }
}

#[derive(Debug)]
pub(crate) struct MappedKey(Binding);

impl<'de> Deserialize<'de> for MappedKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MappedKeyVisitor;
        impl Visitor<'_> for MappedKeyVisitor {
            type Value = MappedKey;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(
                    f,
                    "a key name, optionally prefixed with alt_, ctrl_, or shift_"
                )
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                let mouse = match value {
                    "mouse_wheel_up" => Some(MouseEventKind::ScrollUp),
                    "mouse_wheel_down" => Some(MouseEventKind::ScrollDown),
                    _ => None,
                };
                if let Some(kind) = mouse {
                    return Ok(MappedKey(Binding::Mouse(kind)));
                }
                let (modifiers, name) = match value.split_once('_') {
                    Some(("alt", name)) => (KeyModifiers::ALT, name),
                    Some(("ctrl", name)) => (KeyModifiers::CONTROL, name),
                    Some(("shift", name)) => (KeyModifiers::SHIFT, name),
                    Some(_) => return Err(E::invalid_value(Unexpected::Str(value), &self)),
                    None => (KeyModifiers::NONE, value),
                };
                let code = match name {
                    "up" => KeyCode::Up,
                    "down" => KeyCode::Down,
                    "left" => KeyCode::Left,
                    "right" => KeyCode::Right,
                    "backspace" if modifiers.is_empty() => KeyCode::Backspace,
                    "del" if modifiers.is_empty() => KeyCode::Delete,
                    "end" if modifiers.is_empty() => KeyCode::End,
                    "esc" if modifiers.is_empty() => KeyCode::Esc,
                    "home" if modifiers.is_empty() => KeyCode::Home,
                    "pgdown" if modifiers.is_empty() => KeyCode::PageDown,
                    "pgup" if modifiers.is_empty() => KeyCode::PageUp,
                    "tab" if modifiers.is_empty() => KeyCode::Tab,
                    "enter" if modifiers.is_empty() => KeyCode::Enter,
                    _ => {
                        if let Some(n) = name
                            .strip_prefix('f')
                            .and_then(|n| n.parse::<u8>().ok())
                            .filter(|n| (1..=12).contains(n))
                        {
                            KeyCode::F(n)
                        } else {
                            let mut chars = name.chars();
                            match (chars.next(), chars.next()) {
                                (Some(c), None) if modifiers != KeyModifiers::SHIFT => {
                                    KeyCode::Char(c)
                                }
                                _ => return Err(E::invalid_value(Unexpected::Str(value), &self)),
                            }
                        }
                    }
                };
                // Traditional terminal encodings cannot distinguish these control keys.
                let binding = match (code, modifiers) {
                    (KeyCode::Char('i'), m) if m == KeyModifiers::CONTROL => {
                        Binding::Key(KeyCode::Tab, KeyModifiers::NONE)
                    }
                    (KeyCode::Char('['), m) if m == KeyModifiers::CONTROL => {
                        Binding::Key(KeyCode::Esc, KeyModifiers::NONE)
                    }
                    (KeyCode::Char('\\'), m) if m == KeyModifiers::CONTROL => {
                        Binding::Key(KeyCode::Char('4'), m)
                    }
                    (KeyCode::Char(']'), m) if m == KeyModifiers::CONTROL => {
                        Binding::Key(KeyCode::Char('5'), m)
                    }
                    (KeyCode::Char('^'), m) if m == KeyModifiers::CONTROL => {
                        Binding::Key(KeyCode::Char('6'), m)
                    }
                    (KeyCode::Char('/' | '_'), m) if m == KeyModifiers::CONTROL => {
                        Binding::Key(KeyCode::Char('7'), m)
                    }
                    _ => Binding::Key(code, modifiers),
                };
                Ok(MappedKey(binding))
            }
        }
        deserializer.deserialize_str(MappedKeyVisitor)
    }
}

//
// Display implementations, used in error messages.
//

impl Display for KeyAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            KeyAction::Cancel => "cancel",
            KeyAction::Disable => "disable",
            KeyAction::Exit => "exit",
            KeyAction::RunEditor => "run_editor",
            KeyAction::TabNext => "tab_next",
            KeyAction::TabPrev => "tab_prev",
            KeyAction::TabMoveLeft => "tab_move_left",
            KeyAction::TabMoveRight => "tab_move_right",
            KeyAction::TabGoto(c) => return writeln!(f, "tab_goto: {c}"),
            KeyAction::MessagesPageUp => "messages_page_up",
            KeyAction::MessagesPageDown => "messages_page_down",
            KeyAction::MessagesScrollUp => "messages_scroll_up",
            KeyAction::MessagesScrollDown => "messages_scroll_down",
            KeyAction::MessagesScrollTop => "messages_scroll_top",
            KeyAction::MessagesScrollBottom => "messages_scroll_bottom",
            KeyAction::Input(c) => return writeln!(f, "input_{c}"),
            KeyAction::Command(string) => return writeln!(f, "command_{string}"),
            KeyAction::InputAutoComplete => "input_auto_complete",
            KeyAction::InputNextEntry => "input_next_entry",
            KeyAction::InputPrevEntry => "input_prev_entry",
            KeyAction::InputSend => "input_send",
            KeyAction::InputDeletePrevChar => "input_delete_prev_char",
            KeyAction::InputDeleteNextChar => "input_delete_next_char",
            KeyAction::InputDeleteToStart => "input_delete_to_start",
            KeyAction::InputDeleteToEnd => "input_delete_to_end",
            KeyAction::InputDeletePrevWord => "input_delete_prev_word",
            KeyAction::InputMoveCursEnd => "input_move_curs_end",
            KeyAction::InputMoveCursStart => "input_move_curs_start",
            KeyAction::InputMoveCursLeft => "input_move_curs_left",
            KeyAction::InputMoveCursRight => "input_move_curs_right",
            KeyAction::InputMoveWordLeft => "input_move_word_left",
            KeyAction::InputMoveWordRight => "input_move_word_right",
        };
        writeln!(f, "{s}")
    }
}

struct KeyDisplay(Binding);

impl Display for KeyDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (code, modifiers) = match self.0 {
            Binding::Mouse(MouseEventKind::ScrollUp) => return write!(f, "mouse_wheel_up"),
            Binding::Mouse(MouseEventKind::ScrollDown) => return write!(f, "mouse_wheel_down"),
            Binding::Mouse(_) => return Err(fmt::Error),
            Binding::Key(code, modifiers) => (code, modifiers),
        };
        if modifiers == KeyModifiers::CONTROL {
            write!(f, "ctrl_")?;
        }
        if modifiers == KeyModifiers::ALT {
            write!(f, "alt_")?;
        }
        if modifiers == KeyModifiers::SHIFT {
            write!(f, "shift_")?;
        }
        match code {
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "f{n}"),
            KeyCode::Up => write!(f, "up"),
            KeyCode::Down => write!(f, "down"),
            KeyCode::Left => write!(f, "left"),
            KeyCode::Right => write!(f, "right"),
            KeyCode::Backspace => write!(f, "backspace"),
            KeyCode::Delete => write!(f, "del"),
            KeyCode::End => write!(f, "end"),
            KeyCode::Esc => write!(f, "esc"),
            KeyCode::Home => write!(f, "home"),
            KeyCode::PageDown => write!(f, "pgdown"),
            KeyCode::PageUp => write!(f, "pgup"),
            KeyCode::Tab => write!(f, "tab"),
            KeyCode::Enter => write!(f, "enter"),
            _ => Err(fmt::Error),
        }
    }
}

impl Display for KeyMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (key, action) in self.0.iter() {
            if let KeyAction::TabGoto(_) = action {
                writeln!(f, "{}:", KeyDisplay(*key))?;
                writeln!(f, "  {action}")?;
            } else {
                writeln!(f, "{}: {}", KeyDisplay(*key), action)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_override_defaults_and_preserve_aliases() {
        let overrides: KeyMap = serde_yaml::from_str(
            "ctrl_i: exit\nctrl_[: cancel\nctrl_h: tab_next\nbackspace: tab_prev\nmouse_wheel_up:\n  command: clear\n"
        ).unwrap();
        let mut map = KeyMap::default();
        map.load(&overrides);
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        assert_eq!(
            map.get(&key(KeyCode::Tab, KeyModifiers::NONE)),
            Some(KeyAction::Exit)
        );
        assert_eq!(
            map.get(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Cancel)
        );
        assert_eq!(
            map.get(&key(KeyCode::Char('h'), KeyModifiers::CONTROL)),
            Some(KeyAction::TabNext)
        );
        assert_eq!(
            map.get(&key(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(KeyAction::TabPrev)
        );
        assert_eq!(
            map.get_mouse(MouseEventKind::ScrollUp),
            Some(KeyAction::Command("clear".into()))
        );
        assert_eq!(
            map.get_mouse(MouseEventKind::ScrollDown),
            Some(KeyAction::MessagesScrollDown)
        );
    }

    #[test]
    fn modifiers_and_event_kinds() {
        let map: KeyMap = serde_yaml::from_str(
            "alt_è: exit\nalt__: cancel\nctrl_f2: tab_next\nalt_f2: tab_prev\nshift_f2: exit\nshift_left: tab_move_left\nenter: input_send\n"
        ).unwrap();
        for (code, modifiers, expected) in [
            (KeyCode::Char('è'), KeyModifiers::ALT, KeyAction::Exit),
            (KeyCode::Char('_'), KeyModifiers::ALT, KeyAction::Cancel),
            (KeyCode::F(2), KeyModifiers::CONTROL, KeyAction::TabNext),
            (KeyCode::F(2), KeyModifiers::ALT, KeyAction::TabPrev),
            (KeyCode::F(2), KeyModifiers::SHIFT, KeyAction::Exit),
            (KeyCode::Left, KeyModifiers::SHIFT, KeyAction::TabMoveLeft),
            (KeyCode::Enter, KeyModifiers::NONE, KeyAction::InputSend),
        ] {
            assert_eq!(
                map.get(&KeyEvent::new_with_kind(
                    code,
                    modifiers,
                    KeyEventKind::Press
                )),
                Some(expected.clone())
            );
            assert_eq!(
                map.get(&KeyEvent::new_with_kind(
                    code,
                    modifiers,
                    KeyEventKind::Repeat
                )),
                Some(expected)
            );
            assert_eq!(
                map.get(&KeyEvent::new_with_kind(
                    code,
                    modifiers,
                    KeyEventKind::Release
                )),
                None
            );
        }
        assert!(serde_yaml::from_str::<MappedKey>("ctrl_enter").is_err());
    }

    #[test]
    fn defaults_display_round_trips() {
        let defaults = KeyMap::default();
        let decoded: KeyMap = serde_yaml::from_str(&defaults.to_string()).unwrap();
        assert_eq!(defaults, decoded);
    }
}
