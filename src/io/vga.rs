use core::fmt;
use volatile::Volatile;

lazy_static::lazy_static! {
    pub static ref WRITER: spin::Mutex<Writer> = spin::Mutex::default();
}

#[macro_export]
macro_rules! vga_print {
    ($($arg:tt)*) => ($crate::io::vga::_vga_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! vga_println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::vga_print!("{}\n", format_args!($($arg)*)));
}

#[macro_export]
macro_rules! vga_eprintln {
    ($($arg:tt)*) => ($crate::io::vga::colors::push_style($crate::io::vga::colors::ERROR.0, $crate::io::vga::colors::ERROR.1);
    $crate::vga_print!("[ERR]: {}\n", format_args!($($arg)*)); $crate::io::vga::colors::pop_style());
}
#[macro_export]
macro_rules! vga_wprintln {
    ($($arg:tt)*) => ($crate::io::vga::colors::push_style($crate::io::vga::colors::WARNING.0, $crate::io::vga::colors::WARNING.1);
    $crate::vga_print!("[WAR]: {}\n", format_args!($($arg)*)); $crate::io::vga::colors::pop_style());
}
#[macro_export]
macro_rules! vga_sprintln {
    ($($arg:tt)*) => ($crate::io::vga::colors::push_style($crate::io::vga::colors::SUCCESS.0, $crate::io::vga::colors::SUCCESS.1);
    $crate::vga_print!("[SUC]: {}\n", format_args!($($arg)*)); $crate::io::vga::colors::pop_style());
}

#[doc(hidden)]
pub fn _vga_print(args: fmt::Arguments) {
    use core::fmt::Write;
    WRITER.lock().write_fmt(args).unwrap();
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }

    fn decode(&self) -> colors::ColorPair {
        let foreground: Color = unsafe { core::mem::transmute(self.0 & 0x0F) };
        let background: Color = unsafe { core::mem::transmute((self.0 & 0xF0) >> 4) };
        (foreground, background)
    }
}

impl Default for ColorCode {
    fn default() -> Self {
        ColorCode::new(Color::White, Color::Black)
    }
}

const STYLE_STACK_SIZE: usize = 32;
struct StyleStack {
    stack: [ColorCode; STYLE_STACK_SIZE],
    count: usize,
}
impl StyleStack {
    fn new() -> Self {
        Self {
            stack: [ColorCode::default(); STYLE_STACK_SIZE],
            count: 0,
        }
    }

    // TODO: idk if we want error handling everywhere
    /*
    fn push(&mut self, style: ColorCode) -> Result<(), ()> {
        if self.count < STYLE_STACK_SIZE {
            self.stack[self.count] = style;
            self.count += 1;
            Ok(())
        }
        else { Err(()) }
    }
    */
    fn push(&mut self, fg_color: Color, bg_color: Color) -> ColorCode {
        let color_code = ColorCode::new(fg_color, bg_color);
        if self.count < STYLE_STACK_SIZE {
            self.stack[self.count] = color_code;
            self.count += 1;
            color_code
        } else {
            color_code // return the new color if the stack overflowed
        }
    }

    fn pop(&mut self) -> ColorCode {
        if self.count > 1 {
            self.count -= 1;
            self.stack[self.count - 1]
        } else {
            ColorCode::default()
        }
    }
}

lazy_static::lazy_static! {
    static ref STYLE_STACK: spin::Mutex<StyleStack> = spin::Mutex::new(StyleStack::new());
}

pub mod colors {
    use crate::io::vga::{Color, STYLE_STACK, WRITER};

    pub type ColorPair = (Color, Color);
    pub const ERROR: ColorPair = (Color::Red, Color::Black);
    pub const WARNING: ColorPair = (Color::Yellow, Color::Black);
    pub const SUCCESS: ColorPair = (Color::Green, Color::Black);

    pub fn push_style(fg: Color, bg: Color) {
        let mut writer = WRITER.lock();
        let color_code = STYLE_STACK.lock().push(fg, bg);
        writer.color_code = color_code;
    }

    pub fn pop_style() {
        WRITER.lock().color_code = STYLE_STACK.lock().pop();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;
const BUFFER_ADDR: usize = 0xb8000;

#[repr(transparent)]
struct Buffer {
    chars: [[Volatile<ScreenChar>; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

/// A writer that can write to the VGA text buffer.
/// Writes from the bottom up. New lines push existing lines up.
pub struct Writer {
    column_position: usize,
    color_code: ColorCode,
    buffer: &'static mut Buffer,
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

impl Default for Writer {
    fn default() -> Self {
        Writer::new(ColorCode::default())
    }
}

impl Writer {
    const fn new(color_code: ColorCode) -> Writer {
        Writer {
            column_position: 0,
            color_code,
            // SAFETY: The VGA text buffer is located at a fixed memory address.
            buffer: unsafe { &mut *(BUFFER_ADDR as *mut Buffer) },
        }
    }

    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            0x20..=0x7e => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }
                let row = BUFFER_HEIGHT - 1;
                let col = self.column_position;
                self.buffer.chars[row][col].write(ScreenChar {
                    ascii_character: byte,
                    color_code: self.color_code,
                });
                self.column_position += 1;
            }
            // for non-printable byte, we print a `■` character
            _ => self.write_byte(0xfe),
        }
    }

    fn new_line(&mut self) {
        for row in 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let character = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(character);
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.column_position = 0;
    }

    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: ColorCode::default(), // if set as self.color_code, it will print out empty lines below with the background color, which doesn't look good
        };
        for col in 0..BUFFER_WIDTH {
            self.buffer.chars[row][col].write(blank);
        }
    }

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
    }

    pub fn change_color(&mut self, fg_color: Color, bg_color: Color) {
        self.color_code = ColorCode::new(fg_color, bg_color);
    }
}

pub fn init() {
    let init_colors = WRITER.lock().color_code.decode();
    colors::push_style(init_colors.0, init_colors.1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn println_singular_line() {
        vga_println!("singular line");
    }

    #[test_case]
    fn println_many() {
        for _ in 0..200 {
            vga_println!("many lines");
        }
    }

    #[test_case]
    fn println_output() {
        let s = "I should fit on a single line";
        assert!(s.len() < BUFFER_WIDTH);
        vga_println!("{}", s);
        for (col, char) in s.chars().enumerate() {
            // the second to last row, since println! adds a new line at the end
            let row = BUFFER_HEIGHT - 2;
            let screen_char = WRITER.lock().buffer.chars[row][col].read();
            assert_eq!(char::from(screen_char.ascii_character), char);
        }
    }
}
