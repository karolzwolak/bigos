use crate::{
    events::event_buffer::{AsciiChar, EVENT_BUFFER, Keys},
    serial_println,
};

use alloc::format;
use core::{cmp::min};
use embedded_graphics::{
    mono_font::{MonoFont, MonoTextStyle, ascii::FONT_8X13},
    pixelcolor::Rgb888,
    prelude::*,
    text::{Alignment, LineHeight, Text, TextStyle, TextStyleBuilder},
};

const DEBUG_PRINT: bool = false;

const CHARACTER_WIDTH: usize = 8; // FONT_8X13 width
const CHARACTER_HEIGHT: usize = 13;
const MARGIN_LEFT: i32 = 8;
const MARGIN_TOP: i32 = 0;
const MAX_LINES: usize = 30;
const LINE_SPACING: i32 = 15;
const MAX_CHARS_PER_LINE: usize = 80;

const CHARACTER_STYLE: MonoTextStyle<Rgb888> = MonoTextStyle::new(&FONT_8X13, Rgb888::WHITE);
const BACKGROUND_COLOR: Rgb888 = Rgb888::BLACK;

const TEXT_STYLE: TextStyle = TextStyleBuilder::new()
    .alignment(Alignment::Left)
    .line_height(LineHeight::Percent(150))
    .build();

#[derive(Debug, Clone, Copy)]
pub struct FontMetrics {
    pub char_width: u32,
    pub char_height: u32,
}

impl FontMetrics {
    pub const fn from_font(font: &MonoFont) -> Self {
        Self {
            char_width: font.character_size.width,
            char_height: font.character_size.height,
        }
    }

    pub const FONT_4X6: Self = Self {
        char_width: 4,
        char_height: 6,
    };
    pub const FONT_5X7: Self = Self {
        char_width: 5,
        char_height: 7,
    };
    pub const FONT_5X8: Self = Self {
        char_width: 5,
        char_height: 8,
    };
    pub const FONT_6X9: Self = Self {
        char_width: 6,
        char_height: 9,
    };
    pub const FONT_6X10: Self = Self {
        char_width: 6,
        char_height: 10,
    };
    pub const FONT_6X12: Self = Self {
        char_width: 6,
        char_height: 12,
    };
    pub const FONT_6X13: Self = Self {
        char_width: 6,
        char_height: 13,
    };
    pub const FONT_7X13: Self = Self {
        char_width: 7,
        char_height: 13,
    };
    pub const FONT_7X14: Self = Self {
        char_width: 7,
        char_height: 14,
    };
    pub const FONT_8X13: Self = Self {
        char_width: 8,
        char_height: 13,
    };
    pub const FONT_9X15: Self = Self {
        char_width: 9,
        char_height: 15,
    };
    pub const FONT_9X18: Self = Self {
        char_width: 9,
        char_height: 18,
    };
    pub const FONT_10X20: Self = Self {
        char_width: 10,
        char_height: 20,
    };
}

pub fn get_max_chars_per_line(font_metrics: FontMetrics, available_width: u32) -> usize {
    (available_width / font_metrics.char_width) as usize
}

//TODO: do something more efficient instead of a line buffer, for now we have this just for simplicity
// also this may be good cause i can just render it with embedded-graphics crate
#[derive(Clone, Copy)]
struct Line {
    chars: [u8; MAX_CHARS_PER_LINE],
    length: usize,
}

impl Line {
    const fn new() -> Self {
        Self {
            chars: [0; MAX_CHARS_PER_LINE],
            length: 0,
        }
    }

    fn clear(&mut self) {
        self.length = 0;
    }

    fn is_empty(&self) -> bool {
        self.length == 0
    }

    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.chars[..self.length]) }
    }

    fn write_slice(&mut self, slice: &[u8]) -> usize {
        let copied_len = min(slice.len(), MAX_CHARS_PER_LINE - self.length);
        self.chars[self.length..self.length + copied_len].copy_from_slice(&slice[..copied_len]);
        self.length += copied_len;
        copied_len
    }
}

//TODO: should add buffering, and not clear the whole screen for each render
//TODO: <'a> for now, when we get a compositor it will have its own buffer
pub struct Theophe<D: DrawTarget<Color = Rgb888>> {
    draw_target: D,
    curr_line_idx: usize,
    max_chars_per_line: usize,
    lines: [Line; MAX_LINES],
    last_command: Line,
}

impl<D: DrawTarget<Color = Rgb888>> Theophe<D> {
    pub fn new(draw_target: D) -> Self {
        let font_metrics = FontMetrics::from_font(&FONT_8X13);
        let bounding_box = draw_target.bounding_box();
        let max_chars_per_line = get_max_chars_per_line(font_metrics, bounding_box.size.width);

        Self {
            draw_target,
            curr_line_idx: 0,
            max_chars_per_line,
            lines: [Line::new(); MAX_LINES],
            last_command: Line::new(),
        }
    }

    pub fn render(&mut self) {
        self.redraw_all();
    }

    pub fn update(&mut self) {
        let mut dirty = false;
        loop {
            let event = EVENT_BUFFER.read();
            match event {
                Some(event) => {
                    let v = event.value;
                    if v == Keys::ArrowUp as u32 {
                        self.recall_last_command();
                    } else if let Some(c) = char::from_u32(v) {
                        match c {
                            AsciiChar::BACKSPACE => self.backspace(),
                            AsciiChar::NEWLINE | AsciiChar::CARRIAGE_RETURN => {
                                self.last_command = self.lines[self.curr_line_idx];
                                let cmd = self.last_command;
                                self.newline();
                                self.execute_command(&cmd);
                            }
                            c if !c.is_control() => {
                                self.write_bytes(&[c as u8]);
                            }
                            _ => {}
                        }
                    }
                    dirty = true;
                }
                None => break,
            }
        }
        if dirty {
            self.render();
        }
    }

    fn execute_command(&mut self, line: &Line) {
        let s = line.as_str().trim();
        if s.is_empty() {
            return;
        }

        let (cmd, args) = match s.find(' ') {
            Some(i) => s.split_at(i),
            None => (s, ""),
        };
        let args = args.trim_matches(' ');

        match cmd {
            "ls" => {
                let path = if args.is_empty() { "/" } else { args };
                match crate::filesystem::get_sirius().list_directory(path) {
                    Ok(entries) => {
                        for entry in &entries {
                            self.write_line(entry.name.as_str());
                        }
                    }
                    Err(_) => self.write_line(&format!("ls: no such directory: {}", path)),
                }
            }
            "cat" => {
                if args.is_empty() {
                    self.write_line("usage: cat <path>");
                } else {
                    let mut buf = [0u8; 2048];
                    match crate::filesystem::get_sirius().read_file(args, 0, &mut buf) {
                        Ok(n) => {
                            let s = unsafe { core::str::from_utf8_unchecked(&buf[..n]) };
                            self.write_line(s);
                        }
                        Err(_) => self.write_line(&format!("cat: file not found: {}", args)),
                    }
                }
            }
            "demo" => match args {
                "start" | "start -uv" => {
                    let uv = args == "start -uv";
                    crate::DEMO_UV_MODE.store(uv, core::sync::atomic::Ordering::Relaxed);
                    crate::DEMO_ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);
                    self.write_line(if uv { "demo started (uv mode)" } else { "demo started" });
                }
                "stop" => {
                    crate::DEMO_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
                    self.write_line("demo stopped");
                }
                _ => self.write_line("usage: demo start [-uv] | stop"),
            },
            "help" => {
                self.write_str(
                    "Available commands: \n - ls <dir>\n - cat <path>\n - clear\n - demo start|stop",
                );
            }
            "clear" => {
                self.clear();
            }
            _ => {}
        }
    }

    fn recall_last_command(&mut self) {
        if !self.last_command.is_empty() {
            self.lines[self.curr_line_idx] = self.last_command;
        }
    }

    fn backspace(&mut self) {
        let line = &mut self.lines[self.curr_line_idx];
        if line.length > 0 {
            line.length -= 1;
        }
    }

    fn get_last_line(&mut self) -> &mut Line {
        if self.lines[self.curr_line_idx].length < self.max_chars_per_line {
            &mut self.lines[self.curr_line_idx]
        } else {
            self.curr_line_idx = min(self.curr_line_idx + 1, MAX_LINES - 1);
            &mut self.lines[self.curr_line_idx]
        }
    }

    fn _write_bytes(&mut self, bytes: &[u8]) {
        let mut bytes_start = 0;
        let bytes_len = bytes.len();
        let max_chars_per_line = self.max_chars_per_line;

        for i in 0..bytes_len {
            if bytes[i] == b'\n' && i > bytes_start {
                let line = self.get_last_line();
                let written = line.write_slice(&bytes[bytes_start..i]);
                if DEBUG_PRINT {
                    serial_println!(
                        "Found newline, written: {}, space left now: {}",
                        written,
                        max_chars_per_line - line.length
                    );
                }
            }
        }

        while bytes_start < bytes_len {
            let remaining = bytes_len - bytes_start;
            let line = self.get_last_line();
            let space_left = max_chars_per_line - line.length;
            if DEBUG_PRINT {
                serial_println!("Remaining bytes: {}", remaining);
            }

            if remaining <= space_left {
                let written = line.write_slice(&bytes[bytes_start..]);
                if DEBUG_PRINT {
                    serial_println!(
                        "Fit in last line, written: {}, space left now: {}",
                        written,
                        max_chars_per_line - line.length
                    );
                }
                assert!(written == remaining);
                break;
            } else {
                //let line_start = line.length;

                // Find a good breaking point (a space)
                let mut split_point = min(space_left, remaining);
                for i in (0..split_point).rev() {
                    if bytes[bytes_start + i] == b' ' {
                        split_point = i + 1; // Include the space
                        break;
                    }
                }

                // If no space found, split at line end
                if split_point == 0 {
                    split_point = space_left;
                }

                let slice = &bytes[bytes_start..bytes_start + split_point];

                let line = self.get_last_line();

                let written = line.write_slice(slice);
                self.newline();
                bytes_start += written;
            }
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        let bytes_len = bytes.len();
        let mut bytes_start = 0;

        for i in 0..bytes_len {
            if bytes[i] == b'\n' {
                if i > bytes_start {
                    self._write_bytes(&bytes[bytes_start..i]);
                }
                self.newline();
                bytes_start = i + 1;
            }
        }

        if bytes_start < bytes_len {
            self._write_bytes(&bytes[bytes_start..]);
        }
    }

    pub fn write_line(&mut self, text: &str) {
        self.write_bytes(text.as_bytes());
        self.newline();
    }

    pub fn write_str(&mut self, text: &str) {
        self.write_bytes(text.as_bytes());
    }

    fn newline(&mut self) {
        if self.curr_line_idx < MAX_LINES - 1 {
            self.curr_line_idx += 1;
        } else {
            for i in 1..MAX_LINES {
                self.lines[i - 1] = core::mem::replace(&mut self.lines[i], Line::new());
            }
        }
    }

    pub fn clear(&mut self) {
        self.curr_line_idx = 0;
        for line in &mut self.lines {
            line.clear();
        }
        self.clear_screen();
    }

    fn clear_screen(&mut self) {
        let terminal_height = (MAX_LINES * CHARACTER_HEIGHT
            + (MAX_LINES - 1) * (LINE_SPACING as usize - CHARACTER_HEIGHT))
            as u32;
        let clear_rect = embedded_graphics::primitives::Rectangle::new(
            Point::new(MARGIN_LEFT, MARGIN_TOP),
            Size::new(
                (self.max_chars_per_line * CHARACTER_WIDTH) as u32,
                terminal_height,
            ),
        );

        let _ = clear_rect
            .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_fill(
                BACKGROUND_COLOR,
            ))
            .draw(&mut self.draw_target);
    }

    fn redraw_all(&mut self) {
        self.clear_screen();

        if DEBUG_PRINT {
            serial_println!("Theophe: redraw_all");
        }

        for i in 0..=self.curr_line_idx {
            let is_input_line = i == self.curr_line_idx;
            let text = if !is_input_line {
                alloc::format!("{}", self.lines[i].as_str())
            } else {
                alloc::format!("> {}", self.lines[i].as_str())
            };
            let _ = Text::with_text_style(
                &text,
                self.get_pos(i),
                CHARACTER_STYLE,
                TEXT_STYLE,
            )
            .draw(&mut self.draw_target);
        }
    }

    fn get_pos(&self, line_index: usize) -> Point {
        Point::new(MARGIN_LEFT, MARGIN_TOP + (line_index as i32 * LINE_SPACING))
    }
}

impl<D: DrawTarget<Color = Rgb888>> core::fmt::Write for Theophe<D> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_str(s);
        Ok(())
    }
}
