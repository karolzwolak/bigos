pub mod input;
pub mod line_index;
pub mod scrollback;

use alloc::vec::Vec;
use core::cmp::{max, min};
use embedded_graphics::{
    mono_font::{MonoFont, MonoTextStyle, ascii::FONT_8X13},
    pixelcolor::Rgb888,
    prelude::*,
    text::{Alignment, LineHeight, Text, TextStyle, TextStyleBuilder},
};

use crate::gfx::{color::Rgba8888UNORM, surface::UserSurface};
use line_index::LineIndex;
use scrollback::Scrollback;

const MARGIN_LEFT: i32 = 4;
const MARGIN_TOP: i32 = 16;
const LINE_SPACING: i32 = 15;

const CHARACTER_STYLE: MonoTextStyle<Rgb888> = MonoTextStyle::new(&FONT_8X13, Rgb888::WHITE);
const TEXT_STYLE: TextStyle = TextStyleBuilder::new()
    .alignment(Alignment::Left)
    .line_height(LineHeight::Percent(150))
    .build();

const DEFAULT_MAX_LINES: usize = 4096;

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

    pub const FONT_8X13: Self = Self {
        char_width: 8,
        char_height: 13,
    };
}

pub fn get_max_chars_per_line(font_metrics: FontMetrics, available_width: u32) -> usize {
    (available_width / font_metrics.char_width) as usize
}

pub struct Terminal<D: DrawTarget<Color = Rgb888>> {
    pub draw_target: D,
    pub window_id: u32,
    pub needs_redraw: bool,
    pub scrollback: Scrollback,
    pub lines: LineIndex,
    pub max_chars_per_line: usize,
    pub viewport_rows: usize,
    pub font_metrics: FontMetrics,
    pub last_command_abs: Option<(u64, u64)>,
    pub last_command_buf: Vec<u8>,
    pub input_state: input::InputState,
    pub scroll_offset: usize,
}

impl<D: DrawTarget<Color = Rgb888>> Terminal<D> {
    pub fn new(draw_target: D, window_id: u32, scrollback: Scrollback) -> Self {
        let bounding_box = draw_target.bounding_box();
        let font_metrics = FontMetrics::from_font(&FONT_8X13);
        let available_width = bounding_box.size.width.saturating_sub(MARGIN_LEFT as u32);
        let max_chars_per_line = get_max_chars_per_line(font_metrics, available_width);
        let max_chars_per_line = max(max_chars_per_line, 1);
        let available_height = bounding_box.size.height.saturating_sub(MARGIN_TOP as u32);
        let viewport_rows = if LINE_SPACING > 0 {
            (available_height / LINE_SPACING as u32) as usize
        } else {
            1
        };
        let viewport_rows = max(viewport_rows, 1);
        let start_abs = scrollback.current_absolute();
        let mut lines = LineIndex::new(DEFAULT_MAX_LINES);
        lines.clear(start_abs);

        Self {
            draw_target,
            window_id,
            needs_redraw: true,
            scrollback,
            lines,
            max_chars_per_line,
            viewport_rows,
            font_metrics,
            last_command_abs: None,
            last_command_buf: Vec::new(),
            input_state: input::InputState::new(),
            scroll_offset: 0,
        }
    }

    pub fn new_without_scrollback(draw_target: D, window_id: u32) -> Self {
        Self::new(draw_target, window_id, Scrollback::empty())
    }

    pub fn set_scrollback(&mut self, scrollback: Scrollback) {
        let abs = scrollback.current_absolute();
        self.scrollback = scrollback;
        self.lines.clear(abs);
        self.needs_redraw = true;
    }

    pub fn recompute_viewport(&mut self) {
        let bounding_box = self.draw_target.bounding_box();
        let available_width = bounding_box.size.width.saturating_sub(MARGIN_LEFT as u32);
        let max_chars = get_max_chars_per_line(self.font_metrics, available_width);
        self.max_chars_per_line = max(max_chars, 1);
        let available_height = bounding_box.size.height.saturating_sub(MARGIN_TOP as u32);
        let rows = if LINE_SPACING > 0 {
            (available_height / LINE_SPACING as u32) as usize
        } else {
            1
        };
        self.viewport_rows = max(rows, 1);
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let max_scroll = self.lines.count.saturating_sub(self.viewport_rows);
        self.scroll_offset = min(self.scroll_offset, max_scroll);
    }

    fn scroll_up_lines(&mut self, n: usize) {
        let max_scroll = self.lines.count.saturating_sub(self.viewport_rows);
        self.scroll_offset = min(self.scroll_offset + n, max_scroll);
        self.needs_redraw = true;
    }

    fn scroll_down_lines(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        self.needs_redraw = true;
    }

    pub fn render(&mut self) {
        self.redraw_all();
    }

    fn current_line_len(&self) -> usize {
        self.lines.current_ref().len()
    }

    fn current_line_remaining_space(&self) -> usize {
        let len = self.current_line_len();
        if len >= self.max_chars_per_line {
            0
        } else {
            self.max_chars_per_line - len
        }
    }

    fn ensure_current_line_has_space(&mut self) {
        if self.current_line_remaining_space() == 0 {
            let next = self.scrollback.current_absolute();
            self.lines.line_feed(next);
        }
    }

    fn append_chunk_with_wrap(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }

        let mut offset = 0;
        let mut remaining = chunk.len();

        while remaining > 0 {
            self.ensure_current_line_has_space();
            let space = self.current_line_remaining_space();
            debug_assert!(space > 0);
            debug_assert!(space <= self.max_chars_per_line);

            if remaining <= space {
                let start_abs = self
                    .scrollback
                    .write_bytes(&chunk[offset..offset + remaining]);
                let end_abs = start_abs + remaining as u64;
                self.lines.update_end(end_abs);
                self.lines.force_line_feed_if_needed(end_abs);
                break;
            }

            let mut split = min(space, remaining);
            for i in (0..split).rev() {
                if chunk[offset + i] == b' ' {
                    split = i + 1;
                    break;
                }
            }
            if split == 0 {
                split = space;
            }

            let start_abs = self.scrollback.write_bytes(&chunk[offset..offset + split]);
            let end_abs = start_abs + split as u64;
            self.lines.update_end(end_abs);
            let next_abs = end_abs;
            self.lines.line_feed(next_abs);

            offset += split;
            remaining -= split;
        }
    }

    fn write_bytes_internal(&mut self, bytes: &[u8]) {
        let mut seg_start = 0;

        for i in 0..bytes.len() {
            if bytes[i] == b'\n' {
                if i > seg_start {
                    self.append_chunk_with_wrap(&bytes[seg_start..i]);
                }
                let next_abs = self.scrollback.current_absolute();
                self.lines.line_feed(next_abs);
                seg_start = i + 1;
            } else if bytes[i] == b'\r' {
                if i > seg_start {
                    self.append_chunk_with_wrap(&bytes[seg_start..i]);
                }
                seg_start = i + 1;
            }
        }
        if seg_start < bytes.len() {
            self.append_chunk_with_wrap(&bytes[seg_start..]);
        }
        self.needs_redraw = true;
    }

    pub fn write_str(&mut self, text: &str) {
        self.write_bytes_internal(text.as_bytes());
    }

    pub fn write_line(&mut self, text: &str) {
        self.write_bytes_internal(text.as_bytes());
        let next_abs = self.scrollback.current_absolute();
        self.lines.line_feed(next_abs);
        self.needs_redraw = true;
    }

    fn newline(&mut self) {
        let next_abs = self.scrollback.current_absolute();
        self.lines.line_feed(next_abs);
        self.needs_redraw = true;
    }

    pub fn clear(&mut self) {
        let pos = self.scrollback.current_absolute();
        self.lines.clear(pos);
        self.scroll_offset = 0;
        self.clear_screen();
        self.needs_redraw = true;
    }

    fn clear_screen(&mut self) {
        let surface = unsafe { &mut *(&mut self.draw_target as *mut D as *mut UserSurface) };
        surface.clear(Rgba8888UNORM::BLACK);
    }

    fn redraw_all(&mut self) {
        self.clear_screen();
        let rows = self.viewport_rows;
        let total = self.lines.count;
        let visible = core::cmp::min(rows, total);
        let max_scroll = total.saturating_sub(visible);
        let offset = core::cmp::min(self.scroll_offset, max_scroll);
        let start_logical = total.saturating_sub(visible + offset);

        for vis_idx in 0..visible {
            let logical_idx = start_logical + vis_idx;
            let Some(meta) = self.lines.logical_line_for_index(logical_idx) else {
                continue;
            };
            if meta.is_empty() {
                continue;
            }
            let len = meta.len();
            let bytes = self.scrollback.read_at(meta.first_pos, len);
            if bytes.is_empty() {
                continue;
            }
            let s = unsafe { core::str::from_utf8_unchecked(bytes) };
            let pos = Point::new(MARGIN_LEFT, MARGIN_TOP + vis_idx as i32 * LINE_SPACING);
            let _ = Text::with_text_style(s, pos, CHARACTER_STYLE, TEXT_STYLE)
                .draw(&mut self.draw_target);
        }
    }

    pub fn backspace(&mut self) {
        let cur = *self.lines.current_ref();
        if cur.is_empty() {
            return;
        }
        let new_end = cur.one_past_last_pos - 1;
        self.lines.truncate_last(new_end);
        self.scrollback.remove_last_byte();
        self.needs_redraw = true;
    }

    fn capture_last_command(&mut self) {
        let cur = *self.lines.current_ref();
        if cur.is_empty() {
            self.last_command_abs = None;
            self.last_command_buf.clear();
            return;
        }

        let len = cur.len();
        let bytes = self.scrollback.read_at(cur.first_pos, len);
        self.last_command_buf.clear();
        self.last_command_buf.extend_from_slice(bytes);
        self.last_command_abs = Some((cur.first_pos, cur.one_past_last_pos));
    }

    fn recall_last_command(&mut self) {
        if self.last_command_buf.is_empty() {
            return;
        }
        let buf = self.last_command_buf.clone();
        self.write_bytes_internal(&buf);
        self.needs_redraw = true;
    }

    fn det_flow_demo(&mut self, count: usize) {
        const LINE: &str = "AAAAA BBBB CCC ";
        let total = count.saturating_mul(5);
        let t0 = unsafe { crate::tsc_read().0 };
        for _ in 0..total {
            self.write_str(LINE);
        }
        let t1 = unsafe { crate::tsc_read().0 };
        let delta = t1.wrapping_sub(t0);
        let mut msg_buf = [0u8; 64];
        let mut pos = 0;
        msg_buf[pos..pos + 6].copy_from_slice(b"flow: ");
        pos += 6;
        let mut tmp = total;
        let mut rev = [0u8; 20];
        let mut rev_len = 0;
        if tmp == 0 {
            rev[0] = b'0';
            rev_len = 1;
        } else {
            while tmp > 0 {
                rev[rev_len] = b'0' + (tmp % 10) as u8;
                rev_len += 1;
                tmp /= 10;
            }
        }
        for i in (0..rev_len).rev() {
            msg_buf[pos] = rev[i];
            pos += 1;
        }
        msg_buf[pos..pos + 15].copy_from_slice(b" lines, cycles=");
        pos += 15;
        let mut v = delta;
        let mut rev2 = [0u8; 20];
        let mut rev2_len = 0;
        if v == 0 {
            rev2[0] = b'0';
            rev2_len = 1;
        } else {
            while v > 0 {
                rev2[rev2_len] = b'0' + (v % 10) as u8;
                rev2_len += 1;
                v /= 10;
            }
        }
        for i in (0..rev2_len).rev() {
            if pos >= msg_buf.len() {
                break;
            }
            msg_buf[pos] = rev2[i];
            pos += 1;
        }
        let s = unsafe { core::str::from_utf8_unchecked(&msg_buf[..pos]) };
        self.write_line(s);
    }

    fn flow_demo(&mut self, count: usize) {
        const LOREM: &[u8] = include_bytes!("../../utils/lorem_ipsum.txt");
        if LOREM.is_empty() {
            self.write_line("flow: no example text data");
            return;
        }
        let mut rng_state = unsafe { crate::tsc_read().0 };
        if rng_state == 0 {
            rng_state = 0x9E3779B97F4A7C15;
        }
        let t0 = unsafe { crate::tsc_read().0 };
        let mut line_buf = [0u8; 128];
        for _ in 0..count {
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 7;
            rng_state ^= rng_state << 17;
            let r1 = rng_state;
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 7;
            rng_state ^= rng_state << 17;
            let r2 = rng_state;
            let take = 20 + (r1 as usize % 80);
            let start = (r2 as usize) % LOREM.len();
            let available = LOREM.len() - start;
            let mut len = if take < available { take } else { available };
            if len > line_buf.len() {
                len = line_buf.len();
            }
            line_buf[..len].copy_from_slice(&LOREM[start..start + len]);
            let mut s_start = 0;
            while s_start < len && line_buf[s_start] == b' ' {
                s_start += 1;
            }
            let slice = if s_start < len {
                unsafe { core::str::from_utf8_unchecked(&line_buf[s_start..len]) }
            } else {
                ""
            };
            if !slice.is_empty() {
                self.write_line(slice);
            }
        }
        let t1 = unsafe { crate::tsc_read().0 };
        let delta = t1.wrapping_sub(t0);
        let mut msg_buf = [0u8; 64];
        let mut pos = 0;
        msg_buf[pos..pos + 6].copy_from_slice(b"flow: ");
        pos += 6;
        let mut tmp = count;
        let mut rev = [0u8; 20];
        let mut rev_len = 0;
        if tmp == 0 {
            rev[0] = b'0';
            rev_len = 1;
        } else {
            while tmp > 0 {
                rev[rev_len] = b'0' + (tmp % 10) as u8;
                rev_len += 1;
                tmp /= 10;
            }
        }
        for i in (0..rev_len).rev() {
            msg_buf[pos] = rev[i];
            pos += 1;
        }
        msg_buf[pos..pos + 15].copy_from_slice(b" lines, cycles=");
        pos += 15;
        let mut v = delta;
        let mut rev2 = [0u8; 20];
        let mut rev2_len = 0;
        if v == 0 {
            rev2[0] = b'0';
            rev2_len = 1;
        } else {
            while v > 0 {
                rev2[rev2_len] = b'0' + (v % 10) as u8;
                rev2_len += 1;
                v /= 10;
            }
        }
        for i in (0..rev2_len).rev() {
            if pos >= msg_buf.len() {
                break;
            }
            msg_buf[pos] = rev2[i];
            pos += 1;
        }
        let s = unsafe { core::str::from_utf8_unchecked(&msg_buf[..pos]) };
        self.write_line(s);
    }

    fn execute_command(&mut self) {
        let cmd_buf = self.last_command_buf.clone();
        let s_trimmed = core::str::from_utf8(&cmd_buf).unwrap_or("").trim();
        if s_trimmed.is_empty() {
            return;
        }
        let (cmd, args_raw) = match s_trimmed.find(' ') {
            Some(i) => s_trimmed.split_at(i),
            None => (s_trimmed, ""),
        };
        let args = args_raw.trim();
        match cmd {
            "deb" => {
                self.write_line("deb!");
            }
            "clear" => {
                self.clear();
            }
            "flow" => {
                let count = if args.is_empty() {
                    1600
                } else {
                    let mut v: usize = 0;
                    let mut valid = true;
                    for b in args.bytes() {
                        if b.is_ascii_digit() {
                            v = v * 10 + (b - b'0') as usize;
                        } else {
                            valid = false;
                            break;
                        }
                    }
                    if !valid || v == 0 || v > 20000 {
                        self.write_line("flow: usage: flow [lines 1..20000] (default 1600)");
                        return;
                    }
                    v
                };
                self.flow_demo(count);
            }
            "abc" => {
                let count = if args.is_empty() {
                    1600
                } else {
                    let mut v: usize = 0;
                    let mut valid = true;
                    for b in args.bytes() {
                        if b.is_ascii_digit() {
                            v = v * 10 + (b - b'0') as usize;
                        } else {
                            valid = false;
                            break;
                        }
                    }
                    if !valid || v == 0 || v > 20000 {
                        self.write_line("flow: usage: flow [lines 1..20000] (default 1600)");
                        return;
                    }
                    v
                };
                self.det_flow_demo(count);
            }
            "odys" => unsafe {
                match crate::lookup_program(cmd) {
                    Some(elf_path) => {
                        let chars: Vec<char> = elf_path.chars().collect();
                        let name = "odys";
                        let name_chars = name.chars().collect::<Vec<char>>();
                        let args = [0u8; 0];
                        crate::sys_create_process(chars.as_slice(), name_chars.as_slice(), &args);
                    }
                    None => {
                        self.write_line("Command not found");
                    }
                }
            },
            _ => {}
        }
    }

    fn handle_special_key(&mut self, key: crate::KeyCode, key_state: crate::KeyState) {
        let mut tmp_event = crate::InputEvent {
            event_type: crate::EventType::KeyEvent,
            _pad: [0; 3],
            value: key as u32,
            extra: key_state as u32,
            modifiers: 0,
        };
        if let Some(action) = input::translate(tmp_event, &mut self.input_state) {
            self.apply_action(action);
        }
    }

    fn apply_action(&mut self, action: input::InputAction) {
        match action {
            input::InputAction::ToggleCommandMode => {
                self.needs_redraw = true;
            }
            input::InputAction::Scroll { dir, amount } => match (dir, amount) {
                (input::ScrollDir::Up, input::ScrollAmount::Page) => {
                    self.scroll_up_lines(self.viewport_rows)
                }
                (input::ScrollDir::Down, input::ScrollAmount::Page) => {
                    self.scroll_down_lines(self.viewport_rows)
                }
                (input::ScrollDir::Up, input::ScrollAmount::Line) => self.scroll_up_lines(1),
                (input::ScrollDir::Down, input::ScrollAmount::Line) => self.scroll_down_lines(1),
            },
            input::InputAction::Backspace => self.backspace(),
            input::InputAction::Enter => {
                self.capture_last_command();
                let last = self.last_command_buf.clone();
                self.newline();
                if !last.is_empty() {
                    let s_trimmed = core::str::from_utf8(&last).unwrap_or("").trim();
                    let (cmd, _) = match s_trimmed.find(' ') {
                        Some(i) => s_trimmed.split_at(i),
                        None => (s_trimmed, ""),
                    };
                    match cmd {
                        "deb" | "odys" | "flow" | "abc" | "clear" => self.execute_command(),
                        _ => {}
                    }
                }
                self.clamp_scroll();
            }
            input::InputAction::Char(c) => {
                if self.scroll_offset != 0 {
                    self.scroll_offset = 0;
                }
                let b = c as u8;
                self.write_bytes_internal(&[b]);
            }
            input::InputAction::ArrowUp => {
                if !self.input_state.command_mode {
                    self.recall_last_command();
                }
            }
            input::InputAction::ArrowDown
            | input::InputAction::ArrowLeft
            | input::InputAction::ArrowRight
            | input::InputAction::Home
            | input::InputAction::End
            | input::InputAction::PageUp
            | input::InputAction::PageDown
            | input::InputAction::Escape
            | input::InputAction::Ignore => {}
        }
        self.needs_redraw = true;
    }

    pub fn handle_event(&mut self, event: crate::InputEvent) {
        if let Some(action) = input::translate(event, &mut self.input_state) {
            self.apply_action(action);
        }
    }
}

impl<D: DrawTarget<Color = Rgb888>> core::fmt::Write for Terminal<D> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_str(s);
        Ok(())
    }
}
