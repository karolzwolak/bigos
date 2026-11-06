#![no_std]
#![no_main]
#![feature(portable_simd)]

extern crate alloc;
extern crate rustspace;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::min;
use core::fmt::Write;
use core::{arch::global_asm, sync::atomic::Ordering};
use embedded_graphics::{
    mono_font::{MonoFont, MonoTextStyle, ascii::FONT_8X13},
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, LineHeight, Text, TextStyle, TextStyleBuilder},
};
use rustspace::{
    AsciiChar, DirEntryFlat, EventReader, EventType, FD_FLAG_READ, InputEvent, KeyState, Keys,
    StatFlat,
    gfx::{color::Rgba8888UNORM, surface::UserSurface},
};
use rustspace::{KeyCode, WindowInfo, sys_get_window_info};
use rustspace::{sys_close_file, sys_list_dir, sys_open_file, sys_read_file, sys_stat_file};

const DEBUG_LOGS: bool = false;
macro_rules! serial_println {
    ($($arg:tt)*) => {
        if DEBUG_LOGS {
            rustspace::println!($($arg)*);
        }
    };
}

global_asm!(
    ".section .text.entry",
    ".global _start",
    "_start:",
    "    call main",
    "    ud2",
);

const CHARACTER_WIDTH: usize = 8;
const CHARACTER_HEIGHT: usize = 13;
const MARGIN_LEFT: i32 = 4;
const MARGIN_TOP: i32 = 16;
const MAX_LINES: usize = 40;
const LINE_SPACING: i32 = 15;
const MAX_CHARS_PER_LINE: usize = 100;
const MAX_PATH_LENGTH: usize = 256;
const MAX_FILE_PREVIEW_SIZE: usize = 16 * 1024;

const CHARACTER_STYLE: MonoTextStyle<Rgb888> = MonoTextStyle::new(&FONT_8X13, Rgb888::WHITE);
const SELECTED_STYLE: MonoTextStyle<Rgb888> = MonoTextStyle::new(&FONT_8X13, Rgb888::YELLOW);
const DIRECTORY_STYLE: MonoTextStyle<Rgb888> = MonoTextStyle::new(&FONT_8X13, Rgb888::CYAN);
const BACKGROUND_COLOR: Rgb888 = Rgb888::BLACK;
const FILE_PREVIEW_STYLE: MonoTextStyle<Rgb888> = MonoTextStyle::new(&FONT_8X13, Rgb888::GREEN);

const TEXT_STYLE: TextStyle = TextStyleBuilder::new()
    .alignment(Alignment::Left)
    .line_height(LineHeight::Percent(150))
    .build();

#[derive(Clone, Copy, PartialEq)]
enum ViewMode {
    Directory,
    FilePreview,
}

#[derive(Clone)]
struct FileEntry {
    name: String,
    is_dir: bool,
    size: u64,
}

pub struct Odys<D: DrawTarget<Color = Rgb888>> {
    needs_redraw: bool,
    window_id: u32,
    draw_target: D,
    current_path: String,
    entries: Vec<FileEntry>,
    selected_idx: usize,
    scroll_offset: usize,
    view_mode: ViewMode,
    file_content: String,
    max_visible_entries: usize,
}

impl<D: DrawTarget<Color = Rgb888>> Odys<D> {
    pub fn new(draw_target: D, window_id: u32) -> Self {
        let bounding_box = draw_target.bounding_box();
        let max_visible_entries = (bounding_box.size.height as usize - 40) / LINE_SPACING as usize;

        let mut odys = Self {
            needs_redraw: true,
            window_id,
            draw_target,
            current_path: String::from("/"),
            entries: Vec::new(),
            selected_idx: 0,
            scroll_offset: 0,
            view_mode: ViewMode::Directory,
            file_content: String::new(),
            max_visible_entries,
        };

        odys.load_directory("/");
        odys
    }

    fn load_directory(&mut self, path: &str) {
        self.current_path = String::from(path);
        self.entries.clear();
        self.selected_idx = 0;
        self.scroll_offset = 0;
        self.view_mode = ViewMode::Directory;

        const MAX_ENTRIES: usize = 128;
        let mut dir_entries = [DirEntryFlat::zeroed(); MAX_ENTRIES];

        let count = unsafe { sys_list_dir(path, &mut dir_entries) };

        if count != usize::MAX {
            // Add parent directory entry if not at root
            if path != "/" {
                self.entries.push(FileEntry {
                    name: String::from(".."),
                    is_dir: true,
                    size: 0,
                });
            }

            for i in 0..min(count, MAX_ENTRIES) {
                let entry = &dir_entries[i];
                let name = entry.name_str();
                if name != "." && name != ".." {
                    self.entries.push(FileEntry {
                        name: String::from(name),
                        is_dir: entry.is_dir == 1,
                        size: entry.size,
                    });
                }
            }

            self.entries.sort_by(|a, b| {
                if a.is_dir != b.is_dir {
                    b.is_dir.cmp(&a.is_dir)
                } else {
                    a.name.to_lowercase().cmp(&b.name.to_lowercase())
                }
            });
        }

        self.needs_redraw = true;
    }

    fn display_help(&mut self) {
        let mut text = String::from(
            "Odys - help:\nKeybinds:\n
arrows - cursor navigation\n
in file selection:\n
right arrow - enter directory/open file\n
    if opened file:\n
        contents display on the screen\n
left arrow - exit current directory/current file\n
\n
\n
(left arrow to exit)",
        );

        self.file_content = text;
        self.view_mode = ViewMode::FilePreview;
        self.needs_redraw = true;
    }

    fn open_file(&mut self, path: &str) {
        let fd = unsafe { sys_open_file(path, FD_FLAG_READ) };
        if fd != usize::MAX {
            let mut stat = StatFlat::zeroed();
            unsafe { sys_stat_file(path, &mut stat) };
            let file_size = stat.size;
            let mut read_buf = alloc::vec![0u8; MAX_FILE_PREVIEW_SIZE]; // TODO: reuse scratch buffer

            let n = unsafe { sys_read_file(fd, &mut read_buf) };
            if n != usize::MAX {
                let mut text = String::new();
                for &byte in &read_buf[..n] {
                    if byte == b'\n'
                        || byte == b'\r'
                        || byte == b'\t'
                        || (byte >= 0x20 && byte <= 0x7E)
                    {
                        text.push(byte as char);
                    } else {
                        text.push('.');
                    }
                }

                if n >= MAX_FILE_PREVIEW_SIZE {
                    text.push_str("\n\n[File truncated - too large to display]");
                }

                self.file_content = text;
                self.view_mode = ViewMode::FilePreview;
                self.needs_redraw = true;
            } else {
                rustspace::println!("Error: cant read file, fd: {}", fd)
            }
        } else {
            rustspace::println!("Error: cant open file, fd: {}", fd)
        }
        unsafe { sys_close_file(fd) };
    }

    fn navigate_to_selected(&mut self) {
        if self.selected_idx >= self.entries.len() {
            return;
        }

        let entry = &self.entries[self.selected_idx];

        if entry.name == ".." {
            // Go to parent directory
            let path = self.get_parent_path();
            self.load_directory(&path);
        } else if entry.is_dir {
            // Enter directory
            let new_path = if self.current_path == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", self.current_path, entry.name)
            };
            self.load_directory(&new_path);
        } else {
            // Open file
            let file_path = if self.current_path == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", self.current_path, entry.name)
            };
            self.open_file(&file_path);
        }
    }

    fn get_parent_path(&self) -> String {
        if self.current_path == "/" {
            return String::from("/");
        }

        let path = self.current_path.trim_end_matches('/');
        match path.rfind('/') {
            Some(pos) if pos > 0 => String::from(&path[..pos]),
            Some(_) => String::from("/"),
            None => String::from("/"),
        }
    }

    fn go_back(&mut self) {
        if self.view_mode == ViewMode::FilePreview {
            self.view_mode = ViewMode::Directory;
        } else {
            let path = self.get_parent_path();
            self.load_directory(&path);
        }
        self.needs_redraw = true;
    }

    fn move_selection(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }

        let new_idx = (self.selected_idx as i32 + delta).max(0) as usize;
        let new_idx = min(new_idx, self.entries.len() - 1);

        if new_idx != self.selected_idx {
            self.selected_idx = new_idx;

            // Adjust scroll offset
            if self.selected_idx < self.scroll_offset {
                self.scroll_offset = self.selected_idx;
            } else if self.selected_idx >= self.scroll_offset + self.max_visible_entries {
                self.scroll_offset = self.selected_idx - self.max_visible_entries + 1;
            }

            self.needs_redraw = true;
        }
    }

    pub fn render(&mut self) {
        self.redraw_all();
    }

    fn clear_screen(&mut self) {
        let surface = unsafe { &mut *(&mut self.draw_target as *mut D as *mut UserSurface) };
        surface.clear(Rgba8888UNORM::BLACK);
    }

    fn redraw_all(&mut self) {
        self.clear_screen();

        // Draw header with current path
        let header = format!(" d {}", self.current_path);
        let _ = Text::with_text_style(
            &header,
            Point::new(MARGIN_LEFT, MARGIN_TOP),
            CHARACTER_STYLE,
            TEXT_STYLE,
        )
        .draw(&mut self.draw_target);

        // Draw separator line
        let separator_y = MARGIN_TOP + CHARACTER_HEIGHT as i32 + 4;
        let separator = Rectangle::new(
            Point::new(MARGIN_LEFT, separator_y),
            Size::new(
                self.draw_target.bounding_box().size.width - MARGIN_LEFT as u32 * 2,
                1,
            ),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb888::WHITE, 1));
        let _ = separator.draw(&mut self.draw_target);

        match self.view_mode {
            ViewMode::Directory => self.draw_directory_list(separator_y + 8),
            ViewMode::FilePreview => self.draw_file_preview(separator_y + 8),
        }
    }

    fn draw_directory_list(&mut self, start_y: i32) {
        if self.entries.is_empty() {
            let _ = Text::with_text_style(
                "Empty directory",
                Point::new(MARGIN_LEFT, start_y),
                CHARACTER_STYLE,
                TEXT_STYLE,
            )
            .draw(&mut self.draw_target);
            return;
        }

        let end = min(
            self.scroll_offset + self.max_visible_entries,
            self.entries.len(),
        );

        for i in self.scroll_offset..end {
            let entry = &self.entries[i];
            let y_pos = start_y + (i - self.scroll_offset) as i32 * LINE_SPACING;
            let is_selected = i == self.selected_idx;

            let prefix = if is_selected { "> " } else { "  " };
            let suffix = if entry.is_dir { " /" } else { "" };

            let display = format!("{}{}{} ({} bytes)", prefix, entry.name, suffix, entry.size);

            let style = if is_selected {
                SELECTED_STYLE
            } else if entry.is_dir {
                DIRECTORY_STYLE
            } else {
                CHARACTER_STYLE
            };

            let _ =
                Text::with_text_style(&display, Point::new(MARGIN_LEFT, y_pos), style, TEXT_STYLE)
                    .draw(&mut self.draw_target);
        }
    }

    fn draw_file_preview(&mut self, start_y: i32) {
        let mut y_pos = start_y;
        for line in self.file_content.lines().take(35) {
            let _ = Text::with_text_style(
                line,
                Point::new(MARGIN_LEFT, y_pos),
                FILE_PREVIEW_STYLE,
                TEXT_STYLE,
            )
            .draw(&mut self.draw_target);
            y_pos += LINE_SPACING;
        }
    }

    pub fn handle_event(&mut self, event: InputEvent) {
        match event.event_type {
            EventType::KeyEvent => {
                let keycode = unsafe { core::mem::transmute::<u8, KeyCode>(event.value as u8) };
                let key_state = unsafe { core::mem::transmute::<u8, KeyState>(event.extra as u8) };

                if key_state == KeyState::Pressed {
                    self.handle_key(keycode);
                }
            }
            EventType::CharEvent => {
                if let Some(c) = char::from_u32(event.value) {
                    match c {
                        AsciiChar::QUESTION_MARK => self.display_help(),
                        // AsciiChar::BACKSPACE => self.backspace(),
                        // AsciiChar::NEWLINE | AsciiChar::CARRIAGE_RETURN => {
                        //     self.last_command = self.lines[self.curr_line_idx];
                        //     let cmd = self.last_command;
                        //     self.newline();
                        //     self.execute_command(&cmd);
                        // }
                        // c if !c.is_control() => {
                        //     self.write_bytes(&[c as u8]);
                        // }
                        _ => {}
                    }
                    self.needs_redraw = true;
                }
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyCode) {
        match self.view_mode {
            ViewMode::Directory => match key {
                KeyCode::ArrowUp => self.move_selection(-1),
                KeyCode::ArrowDown => self.move_selection(1),
                KeyCode::ArrowRight => self.navigate_to_selected(),
                KeyCode::ArrowLeft => self.go_back(),
                KeyCode::Home => {
                    self.selected_idx = 0;
                    self.scroll_offset = 0;
                    self.needs_redraw = true;
                }
                KeyCode::End => {
                    self.selected_idx = self.entries.len().saturating_sub(1);
                    self.scroll_offset = self
                        .selected_idx
                        .saturating_sub(self.max_visible_entries - 1);
                    self.needs_redraw = true;
                }
                KeyCode::PageUp => self.move_selection(-(self.max_visible_entries as i32)),
                KeyCode::PageDown => self.move_selection(self.max_visible_entries as i32),
                _ => {}
            },
            ViewMode::FilePreview => {
                match key {
                    KeyCode::ArrowLeft | KeyCode::Escape => {
                        self.view_mode = ViewMode::Directory;
                        self.needs_redraw = true;
                    }
                    KeyCode::ArrowUp => {
                        //TODO: Scroll file content
                        self.needs_redraw = true;
                    }
                    KeyCode::ArrowDown => {
                        //TODO: Scroll file content
                        self.needs_redraw = true;
                    }
                    _ => {}
                }
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    let window_id = unsafe { rustspace::sys_create_window(800, 600, 650, 50) };
    if window_id == u32::MAX {
        rustspace::println!("Odys: create_window failed");
        unsafe { rustspace::sys_exit(1) }
    }
    rustspace::println!("Odys: window id={}", window_id);

    let pixels = unsafe { rustspace::sys_map_window_buffer(window_id) };
    if pixels.is_null() {
        rustspace::println!("Odys: map_window_buffer failed");
        unsafe { rustspace::sys_exit(1) }
    }
    let mut window_info = WindowInfo::zeroed();
    if !unsafe { sys_get_window_info(window_id, &mut window_info) } {
        rustspace::println!("Odys: get_window_info failed");
        unsafe { rustspace::sys_exit(1) }
    }
    let pixels_second = ((pixels as u64) + window_info.front_buffer_offset) as *mut u32;

    let (width, height) = unsafe { rustspace::sys_get_window_size(window_id) };
    if width == 0 || height == 0 {
        rustspace::println!("Odys: get_window_size failed");
        unsafe { rustspace::sys_exit(1) }
    }

    rustspace::println!(
        "Odys: window {}x{} id={} mapped at {:p}, front: {:p}",
        width,
        height,
        window_id,
        pixels,
        pixels_second
    );

    unsafe { rustspace::syscall1(rustspace::SYS_FOCUS_WINDOW, window_id as u64) };

    let surface = unsafe { UserSurface::new(pixels, pixels_second, width, height) };
    let mut odys = Odys::new(surface, window_id);

    rustspace::println!("Odys: starting");

    let mut event_reader = unsafe { EventReader::new(window_info.event_buffer_vaddr as usize) };

    let mut frame: u32 = 0;
    loop {
        loop {
            let event = event_reader.try_read(window_id);
            match event {
                Some(event) => {
                    odys.handle_event(event);
                }
                None => break,
            }
        }

        let backbuffer_redraw_required = odys.needs_redraw;
        if odys.needs_redraw {
            odys.render();
            odys.needs_redraw = false;
        }
        unsafe { rustspace::sys_present_window(window_id) };
        unsafe {
            odys.draw_target.swap();
            odys.needs_redraw = backbuffer_redraw_required;
        }

        frame = frame.wrapping_add(1);
    }

    unsafe { rustspace::sys_exit(0) };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    match info.location() {
        Some(loc) => rustspace::println!("Odys: panic at {}: {}", loc, info.message()),
        None => rustspace::println!("Odys: panic: {}", info.message()),
    }
    unsafe { rustspace::sys_exit(1) }
}
