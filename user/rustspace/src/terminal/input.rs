use crate::{InputEvent, KeyCode, KeyState};

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
    ToggleCommandMode,
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

#[derive(Debug, Clone, Copy)]
pub struct InputState {
    pub command_mode: bool,
}

impl InputState {
    pub const fn new() -> Self {
        Self {
            command_mode: false,
        }
    }

    pub fn is_command_mode(&self) -> bool {
        self.command_mode
    }

    pub fn toggle_command_mode(&mut self) {
        self.command_mode = !self.command_mode;
    }
}

pub fn translate(event: InputEvent, state: &mut InputState) -> Option<InputAction> {
    match event.event_type {
        crate::EventType::KeyEvent => {
            let keycode = unsafe { core::mem::transmute::<u8, KeyCode>(event.value as u8) };
            let key_state = unsafe { core::mem::transmute::<u8, KeyState>(event.extra as u8) };

            if (keycode == KeyCode::LControl || keycode == KeyCode::RControl)
                && key_state == KeyState::Pressed
            {
                state.toggle_command_mode();
                return Some(InputAction::ToggleCommandMode);
            }

            if state.command_mode && key_state == KeyState::Pressed {
                match keycode {
                    KeyCode::Q => {
                        return Some(InputAction::Scroll {
                            dir: ScrollDir::Up,
                            amount: ScrollAmount::Page,
                        });
                    }
                    KeyCode::E => {
                        return Some(InputAction::Scroll {
                            dir: ScrollDir::Down,
                            amount: ScrollAmount::Page,
                        });
                    }
                    KeyCode::A => {
                        return Some(InputAction::Scroll {
                            dir: ScrollDir::Up,
                            amount: ScrollAmount::Line,
                        });
                    }
                    KeyCode::D => {
                        return Some(InputAction::Scroll {
                            dir: ScrollDir::Down,
                            amount: ScrollAmount::Line,
                        });
                    }
                    _ => {}
                }
            }

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

            // In command mode, q/e/a/d via char
            if state.command_mode {
                let lower = c.to_ascii_lowercase();
                match lower {
                    'q' => {
                        return Some(InputAction::Scroll {
                            dir: ScrollDir::Up,
                            amount: ScrollAmount::Page,
                        });
                    }
                    'e' => {
                        return Some(InputAction::Scroll {
                            dir: ScrollDir::Down,
                            amount: ScrollAmount::Page,
                        });
                    }
                    'a' => {
                        return Some(InputAction::Scroll {
                            dir: ScrollDir::Up,
                            amount: ScrollAmount::Line,
                        });
                    }
                    'd' => {
                        return Some(InputAction::Scroll {
                            dir: ScrollDir::Down,
                            amount: ScrollAmount::Line,
                        });
                    }
                    _ => {}
                }
                if c == '\u{11}' || c == '\u{03}' {
                    return Some(InputAction::Ignore);
                }
                // Block other typing in command mode
                return Some(InputAction::Ignore);
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
