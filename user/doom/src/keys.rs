use rustspace::KeyCode;

pub const DOOM_KEY_UNMAPPED: u8 = 0;

const DOOM_KEY_RIGHTARROW: u8 = 0xae;
const DOOM_KEY_LEFTARROW: u8 = 0xac;
const DOOM_KEY_UPARROW: u8 = 0xad;
const DOOM_KEY_DOWNARROW: u8 = 0xaf;
const DOOM_KEY_STRAFE_L: u8 = 0xa0;
const DOOM_KEY_STRAFE_R: u8 = 0xa1;
const DOOM_KEY_ESCAPE: u8 = 27;
const DOOM_KEY_ENTER: u8 = 13;
const DOOM_KEY_TAB: u8 = 9;
const DOOM_KEY_BACKSPACE: u8 = 0x7f;
const DOOM_KEY_EQUALS: u8 = 0x3d;
const DOOM_KEY_MINUS: u8 = 0x2d;
const DOOM_KEY_USE: u8 = 0xa2;
const DOOM_KEY_FUNCTION_BASE: u8 = 0x80;
const DOOM_KEY_RSHIFT: u8 = DOOM_KEY_FUNCTION_BASE + 0x36;
const DOOM_KEY_F1_TO_F10_BASE: u8 = DOOM_KEY_FUNCTION_BASE + 0x3b;
const DOOM_KEY_F11: u8 = DOOM_KEY_FUNCTION_BASE + 0x57;
const DOOM_KEY_F12: u8 = DOOM_KEY_FUNCTION_BASE + 0x58;

pub const KEY_CODE_COUNT: usize = KeyCode::RAlt2 as usize + 1;

const LETTER_KEY_CODES: [KeyCode; 26] = [
    KeyCode::A,
    KeyCode::B,
    KeyCode::C,
    KeyCode::D,
    KeyCode::E,
    KeyCode::F,
    KeyCode::G,
    KeyCode::H,
    KeyCode::I,
    KeyCode::J,
    KeyCode::K,
    KeyCode::L,
    KeyCode::M,
    KeyCode::N,
    KeyCode::O,
    KeyCode::P,
    KeyCode::Q,
    KeyCode::R,
    KeyCode::S,
    KeyCode::T,
    KeyCode::U,
    KeyCode::V,
    KeyCode::W,
    KeyCode::X,
    KeyCode::Y,
    KeyCode::Z,
];

const DIGIT_KEY_CODES: [KeyCode; 10] = [
    KeyCode::Key0,
    KeyCode::Key1,
    KeyCode::Key2,
    KeyCode::Key3,
    KeyCode::Key4,
    KeyCode::Key5,
    KeyCode::Key6,
    KeyCode::Key7,
    KeyCode::Key8,
    KeyCode::Key9,
];

const FUNCTION_KEY_CODES: [KeyCode; 10] = [
    KeyCode::F1,
    KeyCode::F2,
    KeyCode::F3,
    KeyCode::F4,
    KeyCode::F5,
    KeyCode::F6,
    KeyCode::F7,
    KeyCode::F8,
    KeyCode::F9,
    KeyCode::F10,
];

const fn build_key_table() -> [u8; KEY_CODE_COUNT] {
    let mut table = [DOOM_KEY_UNMAPPED; KEY_CODE_COUNT];

    let mut index = 0;
    while index < LETTER_KEY_CODES.len() {
        table[LETTER_KEY_CODES[index] as usize] = b'a' + index as u8;
        index += 1;
    }

    index = 0;
    while index < DIGIT_KEY_CODES.len() {
        table[DIGIT_KEY_CODES[index] as usize] = b'0' + index as u8;
        index += 1;
    }

    index = 0;
    while index < FUNCTION_KEY_CODES.len() {
        table[FUNCTION_KEY_CODES[index] as usize] = DOOM_KEY_F1_TO_F10_BASE + index as u8;
        index += 1;
    }
    table[KeyCode::F11 as usize] = DOOM_KEY_F11;
    table[KeyCode::F12 as usize] = DOOM_KEY_F12;

    table[KeyCode::Spacebar as usize] = DOOM_KEY_USE;
    table[KeyCode::OemMinus as usize] = DOOM_KEY_MINUS;
    table[KeyCode::OemPlus as usize] = DOOM_KEY_EQUALS;
    table[KeyCode::Return as usize] = DOOM_KEY_ENTER;
    table[KeyCode::Escape as usize] = DOOM_KEY_ESCAPE;
    table[KeyCode::Tab as usize] = DOOM_KEY_TAB;
    table[KeyCode::Backspace as usize] = DOOM_KEY_BACKSPACE;

    table[KeyCode::ArrowUp as usize] = DOOM_KEY_UPARROW;
    table[KeyCode::ArrowDown as usize] = DOOM_KEY_DOWNARROW;
    table[KeyCode::ArrowLeft as usize] = DOOM_KEY_LEFTARROW;
    table[KeyCode::ArrowRight as usize] = DOOM_KEY_RIGHTARROW;

    table[KeyCode::LShift as usize] = DOOM_KEY_RSHIFT;
    table[KeyCode::RShift as usize] = DOOM_KEY_RSHIFT;

    table[KeyCode::OemComma as usize] = DOOM_KEY_STRAFE_L;
    table[KeyCode::OemPeriod as usize] = DOOM_KEY_STRAFE_R;

    table
}

pub static DOOM_KEY_BY_KEY_CODE: [u8; KEY_CODE_COUNT] = build_key_table();

pub fn doom_key_for_keycodeval(key_code_value: u32) -> u8 {
    DOOM_KEY_BY_KEY_CODE
        .get(key_code_value as usize)
        .copied()
        .unwrap_or(DOOM_KEY_UNMAPPED)
}
