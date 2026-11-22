pub mod editor;
pub mod text_buffer;

use crate::fs::dir_listing::{DirListing, ParentEntry};
use crate::fs::path::{FsPath, PARENT_DIR_NAME};
use crate::fs::time::DateTime;
use crate::fs::{EntryMode, stat_path};
use crate::gfx::text_grid::{CellStyle, MAX_GRID_COLUMNS, TextGrid};
use crate::terminal::input::{self, InputAction};
use crate::{InputEvent, SyscallError};
use core::cmp::min;
use core::fmt::Write;
use editor::{EditResult, Editor};

const HEADER_ROW: usize = 0;
const BODY_FIRST_ROW: usize = 1;
const CHROME_ROWS: usize = 2;
const NAME_COLUMN_WIDTH: usize = 14;
const SIZE_COLUMN_WIDTH: usize = 10;
const ROW_INDENT: &[u8] = b" ";
const COLUMN_GAP: &str = "  ";
const DIR_SIZE_TEXT: &str = "<DIR>";
const DIR_SUFFIX: &[u8] = b"/";
const HEADER_TITLE: &[u8] = b" fe  ";
const EMPTY_DIRECTORY_TEXT: &[u8] = b" (empty)";

const QUIT_KEY: char = 'q';
const HELP_KEY: char = '?';
const RELOAD_KEY: char = 'r';
const SAVE_CONTROL_KEY: char = 's';
const QUIT_CONTROL_KEY: char = 'c';

const DIRECTORY_HINT: &str = " Enter open  Left back  r reload  ? help  q quit";
const EDITOR_HINT: &str = " Ctrl+S save  Esc close  Ctrl+C quit";
const READ_ONLY_HINT: &str = " Esc close";
const HELP_HINT: &str = " Esc back";
const HELP_LINES: [&str; 12] = [
    " directory",
    "   Up Down PgUp PgDn Home End  move the selection",
    "   Enter Right                 open the directory or file",
    "   Left Backspace              go to the parent directory",
    "   r                           read the directory again",
    "   q Esc Ctrl+C                quit",
    "",
    " file",
    "   arrows PgUp PgDn Home End   move the cursor",
    "   Ctrl+S                      save",
    "   Esc                         close, twice drops unsaved changes",
    "   Ctrl+C                      quit, unsaved changes are dropped",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeView {
    Directory,
    Editor,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// nothing to report, the line shows the keys of the view
    Hint,
    Info,
    Error,
}

/// A message for the status line, kept until the next key press
pub struct StatusLine {
    pub text: [u8; MAX_GRID_COLUMNS],
    pub len: usize,
    pub kind: StatusKind,
}

impl StatusLine {
    pub const fn new() -> Self {
        Self {
            text: [0; MAX_GRID_COLUMNS],
            len: 0,
            kind: StatusKind::Hint,
        }
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.kind = StatusKind::Hint;
    }

    pub fn set(&mut self, kind: StatusKind, message: core::fmt::Arguments) {
        self.len = 0;
        self.kind = kind;
        let _ = self.write_fmt(message);
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.text[..self.len]
    }
}

impl Default for StatusLine {
    fn default() -> Self {
        Self::new()
    }
}

impl Write for StatusLine {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let copy_len = min(text.len(), MAX_GRID_COLUMNS - self.len);
        self.text[self.len..self.len + copy_len].copy_from_slice(&text.as_bytes()[..copy_len]);
        self.len += copy_len;

        Ok(())
    }
}

pub struct FileExplorer {
    pub view: FeView,
    pub cwd: FsPath,
    pub listing: DirListing,
    pub selected_index: usize,
    pub list_scroll: usize,
    pub editor: Editor,
    pub status: StatusLine,
    pub is_discard_armed: bool,
    pub is_quit_requested: bool,
    pub needs_layout: bool,
    pub grid: TextGrid,
}

impl FileExplorer {
    pub fn new(width_px: u32, height_px: u32, start_dir: &FsPath) -> Self {
        let grid = TextGrid::new(width_px, height_px);
        debug_assert!(grid.rows > CHROME_ROWS);

        let mut explorer = Self {
            view: FeView::Directory,
            cwd: FsPath::ROOT_DIR_PATH,
            listing: DirListing::new(),
            selected_index: 0,
            list_scroll: 0,
            editor: Editor::new(),
            status: StatusLine::new(),
            is_discard_armed: false,
            is_quit_requested: false,
            needs_layout: true,
            grid,
        };

        if let Err(error) = explorer.open_directory(start_dir) {
            let _ = explorer.open_directory(&FsPath::ROOT_DIR_PATH);
            explorer
                .status
                .set(StatusKind::Error, format_args!(" {}: {}", start_dir, error));
        }

        explorer
    }

    pub fn body_rows(&self) -> usize {
        self.grid.rows - CHROME_ROWS
    }

    pub fn selected_entry_name(&self) -> &str {
        self.listing.entries[self.selected_index].name_str()
    }

    pub fn open_directory(&mut self, path: &FsPath) -> Result<(), SyscallError> {
        if let Err(error) = self.listing.load(path, ParentEntry::Include) {
            let _ = self.listing.load(&self.cwd, ParentEntry::Include);
            self.select(min(
                self.selected_index,
                self.listing.len().saturating_sub(1),
            ));
            return Err(error);
        }

        self.cwd = *path;
        self.view = FeView::Directory;
        self.list_scroll = 0;
        self.select(0);

        Ok(())
    }

    pub fn open_file(&mut self, path: &FsPath) {
        let loaded = stat_path(path).and_then(|stat| self.editor.load(path, &stat));
        if let Err(error) = loaded {
            self.status
                .set(StatusKind::Error, format_args!(" {}: {}", path, error));
            return;
        }

        self.view = FeView::Editor;
        self.is_discard_armed = false;
        if !self.editor.access.is_editable() {
            self.status.set(
                StatusKind::Info,
                format_args!(" {}", self.editor.access.label()),
            );
        }
        self.needs_layout = true;
    }

    pub fn handle_event(&mut self, event: InputEvent) {
        if let Some(action) = input::translate(event) {
            self.apply_action(action);
        }
    }

    pub fn apply_action(&mut self, action: InputAction) {
        if action == InputAction::Ignore {
            return;
        }

        if action == InputAction::Control(QUIT_CONTROL_KEY) {
            self.is_quit_requested = true;
            return;
        }

        self.status.clear();
        match self.view {
            FeView::Directory => self.apply_directory_action(action),
            FeView::Editor => self.apply_editor_action(action),
            FeView::Help => self.view = FeView::Directory,
        }
        self.needs_layout = true;
    }

    fn apply_directory_action(&mut self, action: InputAction) {
        let page_rows = self.body_rows() as isize;

        match action {
            InputAction::ArrowDown => self.move_selection(1),
            InputAction::ArrowUp => self.move_selection(-1),
            InputAction::PageDown => self.move_selection(page_rows),
            InputAction::PageUp => self.move_selection(-page_rows),
            InputAction::Home => self.select(0),
            InputAction::End => self.select(self.listing.len().saturating_sub(1)),
            InputAction::Enter | InputAction::ArrowRight => self.open_selected(),
            InputAction::ArrowLeft | InputAction::Backspace => self.go_to_parent(),
            InputAction::Escape | InputAction::Char(QUIT_KEY) => self.is_quit_requested = true,
            InputAction::Char(HELP_KEY) => self.view = FeView::Help,
            InputAction::Char(RELOAD_KEY) => self.reload_directory(),
            _ => {}
        }
    }

    fn apply_editor_action(&mut self, action: InputAction) {
        let page_rows = self.body_rows();
        let is_escape = action == InputAction::Escape;

        let edit_result = match action {
            InputAction::Escape => {
                self.close_editor();
                EditResult::Done
            }
            InputAction::Control(SAVE_CONTROL_KEY) => {
                self.save_editor();
                EditResult::Done
            }
            InputAction::ArrowLeft | InputAction::WordLeft => {
                self.editor.move_left();
                EditResult::Done
            }
            InputAction::ArrowRight | InputAction::WordRight => {
                self.editor.move_right();
                EditResult::Done
            }
            InputAction::ArrowUp => {
                self.editor.move_up(1);
                EditResult::Done
            }
            InputAction::ArrowDown => {
                self.editor.move_down(1);
                EditResult::Done
            }
            InputAction::PageUp => {
                self.editor.move_up(page_rows);
                EditResult::Done
            }
            InputAction::PageDown => {
                self.editor.move_down(page_rows);
                EditResult::Done
            }
            InputAction::Home => {
                self.editor.move_line_start();
                EditResult::Done
            }
            InputAction::End => {
                self.editor.move_line_end();
                EditResult::Done
            }
            InputAction::Char(character) if character.is_ascii() => {
                self.editor.insert_byte(character as u8)
            }
            InputAction::Enter => self.editor.insert_byte(b'\n'),
            InputAction::Tab => self.editor.insert_tab(),
            InputAction::Backspace | InputAction::DeleteWord => self.editor.backspace(),
            InputAction::Delete => self.editor.delete(),
            _ => EditResult::Done,
        };

        match edit_result {
            EditResult::Done => {}
            EditResult::ReadOnly => self.status.set(
                StatusKind::Error,
                format_args!(" {}", self.editor.access.label()),
            ),
            EditResult::OutOfMemory => self.status.set(
                StatusKind::Error,
                format_args!(
                    " out of memory, the text stays at {} bytes",
                    self.editor.text.len()
                ),
            ),
        }
        if !is_escape {
            self.is_discard_armed = false;
        }
    }

    fn close_editor(&mut self) {
        if self.editor.is_modified && !self.is_discard_armed {
            self.is_discard_armed = true;
            self.status.set(
                StatusKind::Error,
                format_args!(" unsaved changes: Esc again drops them"),
            );
            return;
        }

        self.is_discard_armed = false;
        self.view = FeView::Directory;
        // the size and time of the file may have changed
        self.reload_directory();
    }

    fn save_editor(&mut self) {
        if !self.editor.access.is_editable() {
            self.status.set(
                StatusKind::Error,
                format_args!(" {}", self.editor.access.label()),
            );
            return;
        }

        match self.editor.save() {
            Ok(written) => self
                .status
                .set(StatusKind::Info, format_args!(" saved {} bytes", written)),
            Err(error) => self.status.set(
                StatusKind::Error,
                format_args!(" {}: {}", self.editor.path, error),
            ),
        }
    }

    fn select(&mut self, index: usize) {
        debug_assert!(index < self.listing.len() || self.listing.is_empty());

        self.selected_index = index;
        let body_rows = self.body_rows();
        if index < self.list_scroll {
            self.list_scroll = index;
        } else if index >= self.list_scroll + body_rows {
            self.list_scroll = index + 1 - body_rows;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.listing.is_empty() {
            return;
        }

        let last_index = self.listing.len() as isize - 1;
        let index = (self.selected_index as isize + delta).clamp(0, last_index);
        self.select(index as usize);
    }

    fn open_selected(&mut self) {
        if self.listing.is_empty() {
            return;
        }

        let entry = self.listing.entries[self.selected_index];
        let name = entry.name_str();
        if name == PARENT_DIR_NAME {
            self.go_to_parent();
            return;
        }

        let child = match self.cwd.child_absolute_path(name) {
            Ok(child) => child,
            Err(error) => {
                self.status
                    .set(StatusKind::Error, format_args!(" {}: {}", name, error));
                return;
            }
        };

        if entry.is_dir == 0 {
            self.open_file(&child);
        } else if let Err(error) = self.open_directory(&child) {
            self.status
                .set(StatusKind::Error, format_args!(" {}: {}", child, error));
        }
    }

    fn go_to_parent(&mut self) {
        if self.cwd.is_root() {
            return;
        }

        let left_dir = self.cwd;
        let mut parent = self.cwd;
        parent.pop_component();
        if let Err(error) = self.open_directory(&parent) {
            self.status
                .set(StatusKind::Error, format_args!(" {}: {}", parent, error));
            return;
        }

        if let Some(index) = self.listing.find(left_dir.file_name()) {
            self.select(index);
        }
    }

    /// Keeps the selection on the same name when it still exists
    fn reload_directory(&mut self) {
        let mut selected_name = [0u8; crate::FS_NAME_LEN];
        let selected_name_len = if self.listing.is_empty() {
            0
        } else {
            let name = self.selected_entry_name().as_bytes();
            selected_name[..name.len()].copy_from_slice(name);
            name.len()
        };

        let cwd = self.cwd;
        if let Err(error) = self.open_directory(&cwd) {
            self.status
                .set(StatusKind::Error, format_args!(" {}: {}", cwd, error));
            return;
        }

        // SAFETY: copied from a name_str
        let name = unsafe { core::str::from_utf8_unchecked(&selected_name[..selected_name_len]) };
        if let Some(index) = self.listing.find(name) {
            self.select(index);
        }
    }

    pub fn update_grid(&mut self) {
        if !self.needs_layout {
            return;
        }
        self.needs_layout = false;

        let body_rows = self.body_rows();
        if self.view == FeView::Editor {
            self.editor.scroll_to_cursor(body_rows, self.grid.columns);
        }

        self.write_header_row();
        match self.view {
            FeView::Directory => self.write_directory_rows(body_rows),
            FeView::Editor => {
                self.editor
                    .write_visible_lines_to_grid(&mut self.grid, BODY_FIRST_ROW, body_rows)
            }
            FeView::Help => self.write_help_rows(body_rows),
        }
        self.write_status_row();
    }

    fn write_header_row(&mut self) {
        let mut writer = self.grid.row_writer(CellStyle::Header);
        writer.put_bytes(HEADER_TITLE);

        if self.view == FeView::Editor {
            let editor = &self.editor;
            let _ = write!(writer, "{}", editor.path);
            if editor.is_modified {
                let _ = write!(writer, " [modified]");
            }
            if !editor.access.is_editable() {
                let _ = write!(writer, " [{}]", editor.access.label());
            }
            let _ = write!(
                writer,
                "  line {} col {}",
                editor.cursor_line_number(),
                editor.column() + 1
            );
        } else {
            let _ = write!(writer, "{}", self.cwd);
        }

        self.grid.commit_row(HEADER_ROW, &writer);
    }

    fn write_directory_rows(&mut self, body_rows: usize) {
        for screen_row in 0..body_rows {
            let index = self.list_scroll + screen_row;
            let row = BODY_FIRST_ROW + screen_row;

            if index >= self.listing.len() {
                let mut writer = self.grid.row_writer(CellStyle::Text);
                if index == 0 {
                    writer.set_style(CellStyle::Dim);
                    writer.put_bytes(EMPTY_DIRECTORY_TEXT);
                }
                self.grid.commit_row(row, &writer);
                continue;
            }

            let entry = &self.listing.entries[index];
            let is_selected = index == self.selected_index;
            let is_dir = entry.is_dir != 0;
            let name = entry.name_str();
            let (name_style, detail_style) = if !is_selected {
                let name_style = if is_dir {
                    CellStyle::Directory
                } else {
                    CellStyle::Text
                };
                (name_style, CellStyle::Dim)
            } else {
                (CellStyle::Selected, CellStyle::Selected)
            };

            let fill_style = if is_selected {
                CellStyle::Selected
            } else {
                CellStyle::Text
            };
            let mut writer = self.grid.row_writer(fill_style);
            writer.put_bytes(ROW_INDENT);
            writer.set_style(name_style);
            writer.put_bytes(name.as_bytes());

            if name != PARENT_DIR_NAME {
                if is_dir {
                    writer.put_bytes(DIR_SUFFIX);
                }
                writer.pad_to_column(ROW_INDENT.len() + NAME_COLUMN_WIDTH);
                writer.set_style(detail_style);

                let mode = EntryMode::new(is_dir, entry.attributes());
                let _ = if is_dir {
                    write!(
                        writer,
                        "{}{}{:>width$}",
                        mode,
                        COLUMN_GAP,
                        DIR_SIZE_TEXT,
                        width = SIZE_COLUMN_WIDTH
                    )
                } else {
                    write!(
                        writer,
                        "{}{}{:>width$}",
                        mode,
                        COLUMN_GAP,
                        entry.size,
                        width = SIZE_COLUMN_WIDTH
                    )
                };
                let _ = write!(
                    writer,
                    "{}{}",
                    COLUMN_GAP,
                    DateTime::from_unix_seconds(entry.modified_time)
                );
            }

            self.grid.commit_row(row, &writer);
        }
    }

    fn write_help_rows(&mut self, body_rows: usize) {
        for screen_row in 0..body_rows {
            let mut writer = self.grid.row_writer(CellStyle::Text);
            if let Some(line) = HELP_LINES.get(screen_row) {
                writer.put_bytes(line.as_bytes());
            }
            self.grid.commit_row(BODY_FIRST_ROW + screen_row, &writer);
        }
    }

    fn write_status_row(&mut self) {
        let style = match self.status.kind {
            StatusKind::Hint | StatusKind::Info => CellStyle::Status,
            StatusKind::Error => CellStyle::Error,
        };
        let mut writer = self.grid.row_writer(style);

        if self.status.kind == StatusKind::Hint {
            let hint = match self.view {
                FeView::Directory => DIRECTORY_HINT,
                FeView::Editor if self.editor.access.is_editable() => EDITOR_HINT,
                FeView::Editor => READ_ONLY_HINT,
                FeView::Help => HELP_HINT,
            };
            writer.put_bytes(hint.as_bytes());
        } else {
            writer.put_bytes(self.status.as_bytes());
        }

        let status_row = self.grid.rows - 1;
        self.grid.commit_row(status_row, &writer);
    }
}
