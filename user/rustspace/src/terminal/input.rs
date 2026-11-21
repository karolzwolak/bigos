use crate::{InputEvent, KeyCode, KeyModifiers, KeyState};

pub const SCROLL_LINE_UP_KEY: char = 'u';
pub const SCROLL_PAGE_UP_KEY: char = 'i';
pub const SCROLL_LINE_DOWN_KEY: char = 'j';
pub const SCROLL_PAGE_DOWN_KEY: char = 'k';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAmount {
    Line,
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDir {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    Scroll {
        dir: ScrollDir,
        amount: ScrollAmount,
    },
    Char(char),
    Backspace,
    Enter,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Escape,
    Ignore,
}

pub fn translate(event: InputEvent) -> Option<InputAction> {
    match event.event_type {
        crate::EventType::KeyEvent => {
            let keycode = unsafe { core::mem::transmute::<u8, KeyCode>(event.value as u8) };
            let key_state = unsafe { core::mem::transmute::<u8, KeyState>(event.extra as u8) };

            if key_state == KeyState::Pressed {
                match keycode {
                    KeyCode::ArrowUp => return Some(InputAction::ArrowUp),
                    KeyCode::ArrowDown => return Some(InputAction::ArrowDown),
                    KeyCode::ArrowLeft => return Some(InputAction::ArrowLeft),
                    KeyCode::ArrowRight => return Some(InputAction::ArrowRight),
                    KeyCode::Home => return Some(InputAction::Home),
                    KeyCode::End => return Some(InputAction::End),
                    KeyCode::PageUp => return Some(InputAction::PageUp),
                    KeyCode::PageDown => return Some(InputAction::PageDown),
                    KeyCode::Escape => return Some(InputAction::Escape),
                    _ => {}
                }
            }

            None
        }
        crate::EventType::CharEvent => {
            let v = event.value;
            let c = char::from_u32(v)?;

            if event.key_modifiers().contains(KeyModifiers::LCTRL) {
                return Some(match c.to_ascii_lowercase() {
                    SCROLL_LINE_UP_KEY => InputAction::Scroll {
                        dir: ScrollDir::Up,
                        amount: ScrollAmount::Line,
                    },
                    SCROLL_PAGE_UP_KEY => InputAction::Scroll {
                        dir: ScrollDir::Up,
                        amount: ScrollAmount::Page,
                    },
                    SCROLL_LINE_DOWN_KEY => InputAction::Scroll {
                        dir: ScrollDir::Down,
                        amount: ScrollAmount::Line,
                    },
                    SCROLL_PAGE_DOWN_KEY => InputAction::Scroll {
                        dir: ScrollDir::Down,
                        amount: ScrollAmount::Page,
                    },
                    _ => InputAction::Ignore,
                });
            }

            match c {
                crate::AsciiChar::BACKSPACE => Some(InputAction::Backspace),
                crate::AsciiChar::NEWLINE | crate::AsciiChar::CARRIAGE_RETURN => {
                    Some(InputAction::Enter)
                }
                c if !c.is_control() => Some(InputAction::Char(c)),
                _ => Some(InputAction::Ignore),
            }
        }
        _ => None,
    }
}
