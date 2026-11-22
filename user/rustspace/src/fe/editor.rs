use super::text_buffer::TextBuffer;
use crate::fs::is_text_byte;
use crate::fs::path::FsPath;
use crate::gfx::text_grid::{CellStyle, TextGrid, is_printable};
use crate::{
    FD_FLAG_READ, FD_FLAG_WRITE, File, FileAttributes, StatFlat, SyscallError, sys_create_file,
    sys_delete,
};
use core::cmp::min;

pub const TAB_WIDTH_SPACES: usize = 4;
const NEWLINE: u8 = b'\n';
const TAB: u8 = b'\t';
const CARRIAGE_RETURN: u8 = b'\r';
const UNPRINTABLE_DISPLAY: u8 = b'.';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorAccess {
    Editable,
    ReadOnly,
}

impl EditorAccess {
    pub const fn is_editable(self) -> bool {
        matches!(self, Self::Editable)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Editable => "editable",
            Self::ReadOnly => "read only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditResult {
    Done,
    ReadOnly,
    OutOfMemory,
}

const fn display_byte(byte: u8) -> u8 {
    if is_printable(byte) {
        byte
    } else if byte == TAB || byte == CARRIAGE_RETURN {
        b' '
    } else {
        UNPRINTABLE_DISPLAY
    }
}

pub struct Editor {
    pub text: TextBuffer,
    pub path: FsPath,
    pub access: EditorAccess,
    pub cursor: usize,
    pub target_column: usize,
    pub top_line_start: usize,
    pub left_column: usize,
    pub is_modified: bool,
    pub saved_file_size: usize,
}

impl Editor {
    pub const fn new() -> Self {
        Self {
            text: TextBuffer::new(),
            path: FsPath::ROOT_DIR_PATH,
            access: EditorAccess::Editable,
            cursor: 0,
            target_column: 0,
            top_line_start: 0,
            left_column: 0,
            is_modified: false,
            saved_file_size: 0,
        }
    }

    pub fn load(&mut self, path: &FsPath, stat: &StatFlat) -> Result<(), SyscallError> {
        let file = File::open(path.as_str(), FD_FLAG_READ)?;
        let area = self.text.load_area_mut(stat.size as usize)?;

        let mut loaded = 0;
        while loaded < area.len() {
            let read = file.read_at(loaded, &mut area[loaded..])?;
            if read == 0 {
                break;
            }
            loaded += read;
        }

        let is_binary = !area[..loaded].iter().all(|&byte| is_text_byte(byte));
        let is_whole_file = loaded < area.len();
        self.text.set_loaded_len(loaded);
        let is_writable = stat.attributes().contains(FileAttributes::WRITE);
        self.access = if is_writable && !is_binary && is_whole_file {
            EditorAccess::Editable
        } else {
            EditorAccess::ReadOnly
        };
        self.path = *path;
        self.cursor = 0;
        self.target_column = 0;
        self.top_line_start = 0;
        self.left_column = 0;
        self.is_modified = false;
        self.saved_file_size = loaded;

        Ok(())
    }

    pub fn save(&mut self) -> Result<usize, SyscallError> {
        debug_assert!(self.access.is_editable());

        let len = self.text.len();
        if len < self.saved_file_size {
            sys_delete(self.path.as_str())?;
            sys_create_file(self.path.as_str())?;
        }

        let mut file = File::open(self.path.as_str(), FD_FLAG_WRITE)?;
        let (before_gap, after_gap) = self.text.segments();
        write_all(&mut file, before_gap)?;
        write_all(&mut file, after_gap)?;

        self.saved_file_size = len;
        self.is_modified = false;

        Ok(len)
    }

    pub fn column(&self) -> usize {
        self.cursor - self.text.line_start(self.cursor)
    }

    pub fn cursor_line_number(&self) -> usize {
        let (before_gap, after_gap) = self.text.segments();
        let in_before = min(self.cursor, before_gap.len());
        let in_after = self.cursor - in_before;
        let count_newlines = |bytes: &[u8]| bytes.iter().filter(|&&byte| byte == NEWLINE).count();

        count_newlines(&before_gap[..in_before]) + count_newlines(&after_gap[..in_after]) + 1
    }

    pub fn insert_byte(&mut self, byte: u8) -> EditResult {
        debug_assert!(is_text_byte(byte));
        if !self.access.is_editable() {
            return EditResult::ReadOnly;
        }
        if !self.text.insert(self.cursor, byte) {
            return EditResult::OutOfMemory;
        }

        self.cursor += 1;
        self.is_modified = true;
        self.target_column = self.column();

        EditResult::Done
    }

    pub fn insert_tab(&mut self) -> EditResult {
        let spaces = TAB_WIDTH_SPACES - self.column() % TAB_WIDTH_SPACES;
        for _ in 0..spaces {
            let result = self.insert_byte(b' ');
            if result != EditResult::Done {
                return result;
            }
        }

        EditResult::Done
    }

    pub fn backspace(&mut self) -> EditResult {
        if !self.access.is_editable() {
            return EditResult::ReadOnly;
        }
        if self.cursor == 0 {
            return EditResult::Done;
        }

        self.cursor -= 1;
        self.text.remove(self.cursor);
        self.is_modified = true;
        self.target_column = self.column();

        EditResult::Done
    }

    pub fn delete(&mut self) -> EditResult {
        if !self.access.is_editable() {
            return EditResult::ReadOnly;
        }
        if self.cursor == self.text.len() {
            return EditResult::Done;
        }

        self.text.remove(self.cursor);
        self.is_modified = true;

        EditResult::Done
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        self.target_column = self.column();
    }

    pub fn move_right(&mut self) {
        self.cursor = min(self.cursor + 1, self.text.len());
        self.target_column = self.column();
    }

    pub fn move_line_start(&mut self) {
        self.cursor = self.text.line_start(self.cursor);
        self.target_column = 0;
    }

    pub fn move_line_end(&mut self) {
        self.cursor = self.text.line_end(self.cursor);
        self.target_column = self.column();
    }

    pub fn move_up(&mut self, line_count: usize) {
        for _ in 0..line_count {
            let line_start = self.text.line_start(self.cursor);
            if line_start == 0 {
                return;
            }

            let previous_end = line_start - 1;
            let previous_start = self.text.line_start(previous_end);
            self.cursor = previous_start + min(self.target_column, previous_end - previous_start);
        }
    }

    pub fn move_down(&mut self, line_count: usize) {
        for _ in 0..line_count {
            let line_end = self.text.line_end(self.cursor);
            if line_end == self.text.len() {
                return;
            }

            let next_start = line_end + 1;
            let next_end = self.text.line_end(next_start);
            self.cursor = next_start + min(self.target_column, next_end - next_start);
        }
    }

    pub fn scroll_to_cursor(&mut self, visible_rows: usize, visible_columns: usize) {
        debug_assert!(visible_rows > 0 && visible_columns > 0);

        let cursor_line_start = self.text.line_start(self.cursor);
        if cursor_line_start < self.top_line_start {
            self.top_line_start = cursor_line_start;
        } else {
            let mut lines_below_top = 0;
            let mut line_start = self.top_line_start;
            while line_start < cursor_line_start {
                line_start = self.text.line_end(line_start) + 1;
                lines_below_top += 1;
            }

            for _ in visible_rows..=lines_below_top {
                self.top_line_start = self.text.line_end(self.top_line_start) + 1;
            }
        }

        let column = self.cursor - cursor_line_start;
        if column < self.left_column {
            self.left_column = column;
        } else if column >= self.left_column + visible_columns {
            self.left_column = column + 1 - visible_columns;
        }
    }

    pub fn write_visible_lines_to_grid(
        &self,
        grid: &mut TextGrid,
        first_row: usize,
        row_count: usize,
    ) {
        debug_assert!(self.top_line_start == self.text.line_start(self.top_line_start));

        let len = self.text.len();
        let mut line_start = self.top_line_start;
        let mut is_past_end = false;

        for row in first_row..first_row + row_count {
            let mut writer = grid.row_writer(CellStyle::Text);
            if is_past_end {
                grid.commit_row(row, &writer);
                continue;
            }

            let line_end = self.text.line_end(line_start);
            let first_visible = min(line_start + self.left_column, line_end);
            let last_visible = min(first_visible + grid.columns, line_end);
            for position in first_visible..last_visible {
                writer.put_byte(display_byte(self.text.byte_at(position)));
            }

            if (line_start..=line_end).contains(&self.cursor) {
                let cursor_column = self.cursor - line_start;
                if let Some(screen_column) = cursor_column.checked_sub(self.left_column) {
                    writer.set_cell_style(screen_column, CellStyle::Cursor);
                }
            }
            grid.commit_row(row, &writer);

            if line_end == len {
                is_past_end = true;
            } else {
                line_start = line_end + 1;
            }
        }
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

fn write_all(file: &mut File, mut bytes: &[u8]) -> Result<(), SyscallError> {
    while !bytes.is_empty() {
        let written = file.write(bytes)?;
        if written == 0 {
            return Err(SyscallError::IO_ERROR);
        }
        bytes = &bytes[written..];
    }

    Ok(())
}
