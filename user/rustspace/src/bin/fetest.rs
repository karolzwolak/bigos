#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use core::sync::atomic::Ordering;
use rustspace::fe::editor::EditorAccess;
use rustspace::fe::text_buffer::{GAP_MIN_GROWTH_BYTES, TextBuffer};
use rustspace::fe::{FeView, FileExplorer, StatusKind};
use rustspace::fs::EntryMode;
use rustspace::fs::path::FsPath;
use rustspace::fs::time::DateTime;
use rustspace::gfx::surface::UserSurface;
use rustspace::gfx::text_grid::{CELL_HEIGHT_PX, CellStyle, STYLE_COLORS, TextGrid};
use rustspace::terminal::input::{InputAction, ScrollAmount, ScrollDir, translate};
use rustspace::{
    AsciiChar, EventType, FD_FLAG_READ, FD_FLAG_WRITE, File, FileAttributes, InputEvent, KeyCode,
    KeyModifiers, KeyState, StatFlat, SyscallError, program_shared_data, sys_create_dir,
    sys_create_file, sys_delete, sys_exit, sys_open_file, sys_stat_file,
};

// Indexes into the shared test counters, test_user_fe uses the same values
const COUNTER_CHECKS_PASSED: usize = 0;
const COUNTER_CHECKS_FAILED: usize = 1;
const COUNTER_CHECKS_DONE: usize = 2;

const EXIT_CODE_PANIC: i32 = 255;

const WIDTH_PX: u32 = 640;
const HEIGHT_PX: u32 = 450;
const GRID_COLUMNS: usize = 80;
const GRID_ROWS: usize = 30;
const BODY_ROWS: usize = GRID_ROWS - 2;
const HEADER_ROW: usize = 0;
const FIRST_BODY_ROW: usize = 1;
const STATUS_ROW: usize = GRID_ROWS - 1;

const TEST_DIR: &str = "/FET";
const SUB_DIR: &str = "/FET/SUB";
const INNER_FILE: &str = "/FET/SUB/INNER.TXT";
const NOTES_FILE: &str = "/FET/NOTES.TXT";
const SHORT_FILE: &str = "/FET/A.TXT";
const BLOB_FILE: &str = "/FET/BLOB.BIN";
const LINES_FILE: &str = "/FET/LINES.TXT";
const WIDE_FILE: &str = "/FET/WIDE.TXT";
const BIN_DIR: &str = "/BIN";
const DISK_PROGRAM: &str = "/BIN/fetest";
const DISK_PROGRAM_NAME: &str = "FETEST";
const DISK_WAD: &str = "/DOOM1.WAD";
const BIG_FILE: &str = "/FET/BIG.TXT";
/// more than the text buffer starts with and more than the old fixed 64 KiB buffer
const BIG_FILE_BYTES: usize = 70_000;
const BIG_LINE_BYTES: usize = 64;
const MISSING_DIR: &str = "/NOPE";
const BLOB_BYTES: [u8; 4] = [0, 1, 2, 0xFF];
const LINE_COUNT: usize = 100;
const WIDE_LINE_BYTES: usize = 100;
const READ_BACK_BYTES: usize = 256;
/// text buffer growth rounds the growth check forces
const GROWTH_TEST_STEPS: usize = 5;
const ALPHABET_LEN: usize = 26;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    rustspace::println!("fetest: panic: {}", info);
    sys_exit(EXIT_CODE_PANIC)
}

// the kernel enters with a 16 byte aligned rsp, the ABI wants it 8 off at function entry
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {fetest_main}",
        "ud2",
        fetest_main = sym fetest_main,
    )
}

fn check(name: &str, condition: bool) {
    let counters = &program_shared_data().test_counters;
    if condition {
        counters[COUNTER_CHECKS_PASSED].fetch_add(1, Ordering::AcqRel);
        return;
    }

    counters[COUNTER_CHECKS_FAILED].fetch_add(1, Ordering::AcqRel);
    rustspace::println!("fetest: FAILED: {}", name);
}

fn path(text: &str) -> FsPath {
    FsPath::resolve_from_cwd(&FsPath::ROOT_DIR_PATH, text).unwrap_or(FsPath::ROOT_DIR_PATH)
}

fn char_event(c: char, modifiers: KeyModifiers) -> InputEvent {
    InputEvent {
        event_type: EventType::CharEvent,
        _pad: [0; 3],
        value: c as u32,
        extra: KeyState::Pressed as u32,
        modifiers: modifiers.bits(),
    }
}

fn key_event(key: KeyCode) -> InputEvent {
    InputEvent {
        event_type: EventType::KeyEvent,
        _pad: [0; 3],
        value: key as u32,
        extra: KeyState::Pressed as u32,
        modifiers: KeyModifiers::empty().bits(),
    }
}

fn press(explorer: &mut FileExplorer, key: KeyCode, times: usize) {
    for _ in 0..times {
        explorer.handle_event(key_event(key));
    }
    explorer.update_grid();
}

fn type_char(explorer: &mut FileExplorer, c: char) {
    explorer.handle_event(char_event(c, KeyModifiers::empty()));
    explorer.update_grid();
}

fn control(explorer: &mut FileExplorer, c: char) {
    explorer.handle_event(char_event(c, KeyModifiers::LCTRL));
    explorer.update_grid();
}

fn write_file(path: &str, bytes: &[u8]) -> bool {
    if sys_create_file(path).is_err() {
        return false;
    }
    let Ok(mut file) = File::open(path, FD_FLAG_WRITE) else {
        return false;
    };

    file.write(bytes) == Ok(bytes.len())
}

/// Lines of 'a', each `BIG_LINE_BYTES` long with its newline
fn write_big_text(path: &str) -> bool {
    let mut chunk = [b'a'; BIG_LINE_BYTES * 64];
    for line_end in chunk
        .iter_mut()
        .skip(BIG_LINE_BYTES - 1)
        .step_by(BIG_LINE_BYTES)
    {
        *line_end = b'\n';
    }
    if sys_create_file(path).is_err() {
        return false;
    }
    let Ok(mut file) = File::open(path, FD_FLAG_WRITE) else {
        return false;
    };

    let mut written = 0;
    while written < BIG_FILE_BYTES {
        let chunk_len = (BIG_FILE_BYTES - written).min(chunk.len());
        match file.write(&chunk[..chunk_len]) {
            Ok(0) | Err(_) => return false,
            Ok(bytes) => written += bytes,
        }
    }

    true
}

/// The first `READ_BACK_BYTES` of the file and how many there were
fn read_file(path: &str) -> ([u8; READ_BACK_BYTES], usize) {
    let mut bytes = [0u8; READ_BACK_BYTES];
    let Ok(file) = File::open(path, FD_FLAG_READ) else {
        return (bytes, 0);
    };
    let len = file.read_at(0, &mut bytes).unwrap_or(0);

    (bytes, len)
}

fn file_size(path: &str) -> u64 {
    let mut stat = StatFlat::zeroed();
    match sys_stat_file(path, &mut stat) {
        Ok(()) => stat.size,
        Err(_) => u64::MAX,
    }
}

fn select_by_name(explorer: &mut FileExplorer, name: &str) -> bool {
    let Some(index) = explorer.listing.find(name) else {
        return false;
    };
    while explorer.selected_index < index {
        press(explorer, KeyCode::ArrowDown, 1);
    }
    while explorer.selected_index > index {
        press(explorer, KeyCode::ArrowUp, 1);
    }

    true
}

fn check_date_time() {
    let cases: [(u32, DateTime); 5] = [
        (
            0,
            DateTime {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            },
        ),
        (
            951_782_400,
            DateTime {
                year: 2000,
                month: 2,
                day: 29,
                hour: 0,
                minute: 0,
                second: 0,
            },
        ),
        (
            1_700_000_000,
            DateTime {
                year: 2023,
                month: 11,
                day: 14,
                hour: 22,
                minute: 13,
                second: 20,
            },
        ),
        (
            1_709_251_199,
            DateTime {
                year: 2024,
                month: 2,
                day: 29,
                hour: 23,
                minute: 59,
                second: 59,
            },
        ),
        (
            u32::MAX,
            DateTime {
                year: 2106,
                month: 2,
                day: 7,
                hour: 6,
                minute: 28,
                second: 15,
            },
        ),
    ];
    for (unix_seconds, expected) in cases {
        check(
            "unix time to date",
            DateTime::from_unix_seconds(unix_seconds) == expected,
        );
    }

    let mut text = [0u8; DateTime::TEXT_LEN];
    let mut writer = SliceWriter {
        bytes: &mut text,
        len: 0,
    };
    let _ = core::fmt::write(
        &mut writer,
        format_args!("{}", DateTime::from_unix_seconds(1_700_000_000)),
    );
    check("date text", &text[..] == b"2023-11-14 22:13:20");
}

fn check_entry_mode() {
    let file_mode = EntryMode::new(false, FileAttributes::READ | FileAttributes::WRITE);
    check("file mode", file_mode.chars() == *b"-rw-");
    let dir_mode = EntryMode::new(true, FileAttributes::DIR_DEFAULT);
    check("directory mode", dir_mode.chars() == *b"dr--");
}

fn text_buffer_bytes(buffer: &TextBuffer) -> ([u8; READ_BACK_BYTES], usize) {
    let mut text = [0u8; READ_BACK_BYTES];
    for (position, byte) in text[..buffer.len()].iter_mut().enumerate() {
        *byte = buffer.byte_at(position);
    }

    (text, buffer.len())
}

fn check_text_buffer() {
    let mut buffer = TextBuffer::new();
    check("text buffer starts empty", buffer.is_empty());

    for (position, &byte) in b"ac\nef".iter().enumerate() {
        buffer.insert(position, byte);
    }
    buffer.insert(1, b'b');
    buffer.insert(4, b'd');
    let (text, len) = text_buffer_bytes(&buffer);
    check("inserts in the middle", &text[..len] == b"abc\ndef");

    buffer.remove(0);
    buffer.remove(5);
    let (text, len) = text_buffer_bytes(&buffer);
    check("removes at both ends", &text[..len] == b"bc\nde");

    let (before_gap, after_gap) = buffer.segments();
    check(
        "segments cover the text",
        before_gap.len() + after_gap.len() == len,
    );
    check("line start of the first line", buffer.line_start(1) == 0);
    check("line start after a newline", buffer.line_start(4) == 3);
    check("line end stops at the newline", buffer.line_end(0) == 2);
    check("line end of the last line", buffer.line_end(3) == len);

    check_text_buffer_growth();
}

/// Inserts at the front so every growth has text behind the gap to move
fn check_text_buffer_growth() {
    let mut buffer = TextBuffer::new();
    let byte_count = GAP_MIN_GROWTH_BYTES * GROWTH_TEST_STEPS;
    let pattern_byte = |index: usize| b'a' + (index % ALPHABET_LEN) as u8;

    let mut all_inserted = true;
    for index in (0..byte_count).rev() {
        all_inserted &= buffer.insert(0, pattern_byte(index));
    }
    check("text buffer grows on demand", all_inserted);
    check("grown buffer holds every byte", buffer.len() == byte_count);
    check(
        "grown buffer keeps the order",
        (0..byte_count).all(|index| buffer.byte_at(index) == pattern_byte(index)),
    );
}

fn check_input_translation() {
    let none = KeyModifiers::empty();
    check(
        "escape char",
        translate(char_event(AsciiChar::ESCAPE, none)) == Some(InputAction::Escape),
    );
    check(
        "tab char",
        translate(char_event(AsciiChar::TAB, none)) == Some(InputAction::Tab),
    );
    check(
        "delete char",
        translate(char_event(AsciiChar::DELETE, none)) == Some(InputAction::Delete),
    );
    check(
        "unbound ctrl chord",
        translate(char_event('S', KeyModifiers::LCTRL)) == Some(InputAction::Control('s')),
    );
    check(
        "terminal scroll chord",
        translate(char_event('u', KeyModifiers::LCTRL))
            == Some(InputAction::Scroll {
                dir: ScrollDir::Up,
                amount: ScrollAmount::Line,
            }),
    );
}

fn check_grid_render() {
    let pixel_count = (WIDTH_PX * HEIGHT_PX) as usize;
    let mut pixels = vec![0u32; pixel_count];
    let mut pixels_second = vec![0u32; pixel_count];
    let mut surface = unsafe {
        UserSurface::new(
            pixels.as_mut_ptr(),
            pixels_second.as_mut_ptr(),
            WIDTH_PX,
            HEIGHT_PX,
        )
    };

    let mut grid = TextGrid::new(WIDTH_PX, HEIGHT_PX);
    check("grid columns", grid.columns == GRID_COLUMNS);
    check("grid rows", grid.rows == GRID_ROWS);
    check(
        "space glyph is empty",
        grid.font.glyph(b' ').iter().all(|&bits| bits == 0),
    );
    check(
        "letter glyph has pixels",
        grid.font.glyph(b'A').iter().any(|&bits| bits != 0),
    );

    let mut writer = grid.row_writer(CellStyle::Header);
    writer.put_bytes(b"title\x01");
    grid.commit_row(0, &writer);
    check("row text", grid.row_text(0) == "title?");

    check("first frame draws", grid.render(&mut surface));
    surface.swap();
    check("second buffer draws", grid.render(&mut surface));
    surface.swap();
    check("nothing left to draw", !grid.render(&mut surface));
    check("no stale rows", !grid.has_stale_rows());

    let header_background = STYLE_COLORS[CellStyle::Header as usize].background_xrgb;
    let gap_pixel = ((CELL_HEIGHT_PX - 1) * WIDTH_PX) as usize;
    check(
        "header background in buffer 0",
        pixels[gap_pixel] == header_background,
    );
    check(
        "header background in buffer 1",
        pixels_second[gap_pixel] == header_background,
    );

    let mut same = grid.row_writer(CellStyle::Header);
    same.put_bytes(b"title\x01");
    grid.commit_row(0, &same);
    check("an unchanged row stays clean", !grid.has_stale_rows());

    let mut changed = grid.row_writer(CellStyle::Text);
    changed.put_bytes(b"other");
    grid.commit_row(0, &changed);
    check("a changed row is stale", grid.has_stale_rows());
    check("changed row draws", grid.render(&mut surface));
    surface.swap();
    check("changed row draws again", grid.render(&mut surface));
    check("changed row is done", !grid.has_stale_rows());
    check(
        "text background replaced the header",
        pixels[gap_pixel] == STYLE_COLORS[0].background_xrgb,
    );
}

fn setup_files() -> bool {
    let mut lines = [0u8; LINE_COUNT * 4];
    let mut lines_len = 0;
    for line in 0..LINE_COUNT {
        let mut writer = SliceWriter {
            bytes: &mut lines[lines_len..],
            len: 0,
        };
        let _ = core::fmt::write(&mut writer, format_args!("L{}\n", line));
        lines_len += writer.len;
    }

    let wide = [b'w'; WIDE_LINE_BYTES];

    sys_create_dir(TEST_DIR).is_ok()
        && sys_create_dir(SUB_DIR).is_ok()
        && write_file(INNER_FILE, b"x")
        && write_file(NOTES_FILE, b"hello\nworld\n")
        && write_file(SHORT_FILE, b"a")
        && write_file(BLOB_FILE, &BLOB_BYTES)
        && write_file(LINES_FILE, &lines[..lines_len])
        && write_file(WIDE_FILE, &wide)
        && write_big_text(BIG_FILE)
}

fn check_listing(explorer: &mut FileExplorer) {
    explorer.update_grid();
    let names = [
        "..",
        "SUB",
        "A.TXT",
        "BIG.TXT",
        "BLOB.BIN",
        "LINES.TXT",
        "NOTES.TXT",
        "WIDE.TXT",
    ];
    check("entry count", explorer.listing.len() == names.len());
    for (index, name) in names.iter().enumerate() {
        check(name, explorer.listing.entries[index].name_str() == *name);
    }

    check(
        "header shows the path",
        explorer.grid.row_text(HEADER_ROW) == " fe  /FET",
    );
    check(
        "parent row",
        explorer.grid.row_text(FIRST_BODY_ROW) == " ..",
    );
    check(
        "parent row is selected",
        explorer.grid.cell_style(FIRST_BODY_ROW, GRID_COLUMNS - 1) == CellStyle::Selected,
    );
    check(
        "directory row",
        explorer
            .grid
            .row_text(FIRST_BODY_ROW + 1)
            .starts_with(" SUB/"),
    );
    check(
        "directory row style",
        explorer.grid.cell_style(FIRST_BODY_ROW + 1, 1) == CellStyle::Directory,
    );
    check(
        "directory size column",
        explorer.grid.row_text(FIRST_BODY_ROW + 1).contains("<DIR>"),
    );
    check(
        "file row size",
        explorer
            .grid
            .row_text(FIRST_BODY_ROW + 2)
            .contains("-rw-           1  "),
    );
    check(
        "status shows the keys",
        explorer
            .grid
            .row_text(STATUS_ROW)
            .starts_with(" Enter open"),
    );
    check(
        "rows below the list are empty",
        explorer.grid.row_text(GRID_ROWS - 2).is_empty(),
    );
}

fn check_navigation(explorer: &mut FileExplorer) {
    select_by_name(explorer, "SUB");
    press(explorer, KeyCode::ArrowRight, 1);
    check("entered the subdirectory", explorer.cwd.as_str() == SUB_DIR);
    check("subdirectory listing", explorer.listing.len() == 2);
    check(
        "subdirectory header",
        explorer.grid.row_text(HEADER_ROW) == " fe  /FET/SUB",
    );

    press(explorer, KeyCode::ArrowLeft, 1);
    check("back in the parent", explorer.cwd.as_str() == TEST_DIR);
    check(
        "the left directory is selected",
        explorer.selected_entry_name() == "SUB",
    );

    press(explorer, KeyCode::End, 1);
    check(
        "end selects the last entry",
        explorer.selected_entry_name() == "WIDE.TXT",
    );
    press(explorer, KeyCode::Home, 1);
    check("home selects the first entry", explorer.selected_index == 0);
    press(explorer, KeyCode::ArrowUp, 1);
    check("selection stops at the top", explorer.selected_index == 0);

    let missing = FileExplorer::new(WIDTH_PX, HEIGHT_PX, &path(MISSING_DIR));
    check(
        "missing start falls back to the root",
        missing.cwd.is_root(),
    );
    check(
        "missing start is reported",
        missing.status.kind == StatusKind::Error,
    );
}

fn check_editing(explorer: &mut FileExplorer) {
    select_by_name(explorer, "NOTES.TXT");
    press(explorer, KeyCode::ArrowRight, 1);
    check("file opened in the editor", explorer.view == FeView::Editor);
    check(
        "text is editable",
        explorer.editor.access == EditorAccess::Editable,
    );
    check(
        "first line",
        explorer.grid.row_text(FIRST_BODY_ROW) == "hello",
    );
    check(
        "second line",
        explorer.grid.row_text(FIRST_BODY_ROW + 1) == "world",
    );
    check(
        "line after the last newline",
        explorer.grid.row_text(FIRST_BODY_ROW + 2).is_empty(),
    );
    check(
        "rows past the end are empty",
        explorer.grid.row_text(FIRST_BODY_ROW + 3).is_empty(),
    );
    check(
        "cursor cell",
        explorer.grid.cell_style(FIRST_BODY_ROW, 0) == CellStyle::Cursor,
    );
    check(
        "editor header",
        explorer.grid.row_text(HEADER_ROW) == " fe  /FET/NOTES.TXT  line 1 col 1",
    );

    press(explorer, KeyCode::ArrowDown, 1);
    press(explorer, KeyCode::End, 1);
    type_char(explorer, '!');
    check(
        "typed at the line end",
        explorer.grid.row_text(FIRST_BODY_ROW + 1) == "world!",
    );
    check("text is modified", explorer.editor.is_modified);
    check(
        "header shows the change",
        explorer.grid.row_text(HEADER_ROW).contains("[modified]"),
    );
    check(
        "cursor after the typed char",
        explorer.grid.cell_style(FIRST_BODY_ROW + 1, 6) == CellStyle::Cursor,
    );

    explorer.handle_event(char_event(AsciiChar::ESCAPE, KeyModifiers::empty()));
    explorer.update_grid();
    check(
        "escape asks before dropping changes",
        explorer.view == FeView::Editor,
    );
    check(
        "the question is on the status line",
        explorer.status.kind == StatusKind::Error,
    );

    control(explorer, 's');
    check("saved", !explorer.editor.is_modified);
    check(
        "save is reported",
        explorer.grid.row_text(STATUS_ROW) == " saved 13 bytes",
    );
    let (bytes, len) = read_file(NOTES_FILE);
    check("saved text on disk", &bytes[..len] == b"hello\nworld!\n");

    press(explorer, KeyCode::ArrowUp, 1);
    press(explorer, KeyCode::End, 1);
    for _ in 0.."hello".len() {
        explorer.handle_event(char_event(AsciiChar::BACKSPACE, KeyModifiers::empty()));
    }
    control(explorer, 's');
    let (bytes, len) = read_file(NOTES_FILE);
    check(
        "shorter text replaced the file",
        &bytes[..len] == b"\nworld!\n",
    );
    check("shorter file size", file_size(NOTES_FILE) == 8);

    explorer.handle_event(char_event(AsciiChar::DELETE, KeyModifiers::empty()));
    explorer.update_grid();
    check(
        "delete joined the lines",
        explorer.grid.row_text(FIRST_BODY_ROW) == "world!",
    );
    explorer.handle_event(char_event(AsciiChar::ESCAPE, KeyModifiers::empty()));
    explorer.handle_event(char_event(AsciiChar::ESCAPE, KeyModifiers::empty()));
    explorer.update_grid();
    check(
        "second escape drops the changes",
        explorer.view == FeView::Directory,
    );
    check(
        "the closed file stays selected",
        explorer.selected_entry_name() == "NOTES.TXT",
    );
    let (bytes, len) = read_file(NOTES_FILE);
    check(
        "dropped changes are not saved",
        &bytes[..len] == b"\nworld!\n",
    );

    select_by_name(explorer, "A.TXT");
    press(explorer, KeyCode::ArrowRight, 1);
    explorer.handle_event(char_event(AsciiChar::TAB, KeyModifiers::empty()));
    explorer.update_grid();
    check(
        "tab inserts spaces",
        explorer.grid.row_text(FIRST_BODY_ROW) == "    a",
    );
    explorer.handle_event(char_event(AsciiChar::ESCAPE, KeyModifiers::empty()));
    explorer.handle_event(char_event(AsciiChar::ESCAPE, KeyModifiers::empty()));
    let (bytes, len) = read_file(SHORT_FILE);
    check("unsaved tab left the file alone", &bytes[..len] == b"a");
}

fn check_binary_read_only(explorer: &mut FileExplorer) {
    select_by_name(explorer, "BLOB.BIN");
    press(explorer, KeyCode::ArrowRight, 1);
    check("a binary file opens", explorer.view == FeView::Editor);
    check(
        "a binary file is read only",
        explorer.editor.access == EditorAccess::ReadOnly,
    );
    check(
        "binary bytes are dots",
        explorer.grid.row_text(FIRST_BODY_ROW) == "....",
    );

    type_char(explorer, 'x');
    check(
        "typing into a binary file is refused",
        !explorer.editor.is_modified,
    );
    control(explorer, 's');
    let (bytes, len) = read_file(BLOB_FILE);
    check("the binary file is unchanged", bytes[..len] == BLOB_BYTES);

    explorer.handle_event(char_event(AsciiChar::ESCAPE, KeyModifiers::empty()));
    explorer.update_grid();
}

fn check_big_text(explorer: &mut FileExplorer) {
    select_by_name(explorer, "BIG.TXT");
    press(explorer, KeyCode::ArrowRight, 1);
    check("a big text opens", explorer.view == FeView::Editor);
    check(
        "a big text is editable",
        explorer.editor.access == EditorAccess::Editable,
    );
    check(
        "the whole big text is loaded",
        explorer.editor.text.len() == BIG_FILE_BYTES,
    );
    check(
        "the start of the text is shown",
        explorer.grid.row_text(FIRST_BODY_ROW).len() == BIG_LINE_BYTES - 1,
    );

    type_char(explorer, 'x');
    check("typing into a big text works", explorer.editor.is_modified);
    control(explorer, 's');
    check(
        "the saved big text grew by one byte",
        file_size(BIG_FILE) == BIG_FILE_BYTES as u64 + 1,
    );
    let (bytes, _) = read_file(BIG_FILE);
    check("the typed byte is saved first", bytes[..2] == *b"xa");

    explorer.handle_event(char_event(AsciiChar::ESCAPE, KeyModifiers::empty()));
    explorer.update_grid();
    check(
        "a saved file closes at once",
        explorer.view == FeView::Directory,
    );
}

fn attributes_of(path: &str) -> FileAttributes {
    let mut stat = StatFlat::zeroed();
    match sys_stat_file(path, &mut stat) {
        Ok(()) => stat.attributes(),
        Err(_) => FileAttributes::empty(),
    }
}

/// Programs in /BIN are r-x, DOOM1.WAD is r--, files a program creates are rw-
fn check_permissions(explorer: &mut FileExplorer) {
    let program_attributes = attributes_of(DISK_PROGRAM);
    check(
        "a program is r-x",
        program_attributes == FileAttributes::READ | FileAttributes::EXECUTE,
    );
    check(
        "a created file is rw-",
        attributes_of(NOTES_FILE) == FileAttributes::READ | FileAttributes::WRITE,
    );
    check(
        "a program cannot be opened for writing",
        sys_open_file(DISK_PROGRAM, FD_FLAG_WRITE) == Err(SyscallError::PERMISSION_DENIED),
    );
    check(
        "a program cannot be deleted",
        sys_delete(DISK_PROGRAM) == Err(SyscallError::PERMISSION_DENIED),
    );
    check(
        "a program can be read",
        read_file(DISK_PROGRAM).1 == READ_BACK_BYTES,
    );

    let bin_dir = path(BIN_DIR);
    check("open /BIN", explorer.open_directory(&bin_dir).is_ok());
    check(
        "/BIN lists the program",
        select_by_name(explorer, DISK_PROGRAM_NAME),
    );
    check(
        "the program row shows r-x",
        explorer
            .grid
            .row_text(FIRST_BODY_ROW + explorer.selected_index - explorer.list_scroll)
            .contains("-r-x"),
    );

    press(explorer, KeyCode::ArrowRight, 1);
    check("a program opens", explorer.view == FeView::Editor);
    check(
        "a program opens read only",
        explorer.editor.access == EditorAccess::ReadOnly,
    );
    type_char(explorer, 'x');
    check(
        "typing into a program is refused",
        !explorer.editor.is_modified,
    );
    check(
        "the refusal says read only",
        explorer.grid.row_text(STATUS_ROW).ends_with("read only"),
    );
    explorer.handle_event(char_event(AsciiChar::ESCAPE, KeyModifiers::empty()));
    explorer.update_grid();
    check("the program closes", explorer.view == FeView::Directory);

    // staged only when storage/doom1.wad exists
    if file_size(DISK_WAD) != u64::MAX {
        check(
            "DOOM1.WAD is r--",
            attributes_of(DISK_WAD) == FileAttributes::READ,
        );
        explorer.open_file(&path(DISK_WAD));
        check(
            "DOOM1.WAD opens read only",
            explorer.editor.access == EditorAccess::ReadOnly,
        );
        explorer.handle_event(char_event(AsciiChar::ESCAPE, KeyModifiers::empty()));
    }

    check(
        "back to the test directory",
        explorer.open_directory(&path(TEST_DIR)).is_ok(),
    );
}

fn check_scrolling(explorer: &mut FileExplorer) {
    select_by_name(explorer, "LINES.TXT");
    press(explorer, KeyCode::ArrowRight, 1);
    check(
        "lines file first row",
        explorer.grid.row_text(FIRST_BODY_ROW) == "L0",
    );

    press(explorer, KeyCode::PageDown, 1);
    check(
        "page down moved the cursor",
        explorer.editor.cursor_line_number() == BODY_ROWS + 1,
    );
    check(
        "scrolled by one line",
        explorer.grid.row_text(FIRST_BODY_ROW) == "L1",
    );
    check(
        "cursor line is the last row",
        explorer.grid.row_text(GRID_ROWS - 2) == "L28",
    );

    press(explorer, KeyCode::PageUp, 2);
    check("page up reached the top", explorer.editor.cursor == 0);
    check(
        "scrolled back to the top",
        explorer.grid.row_text(FIRST_BODY_ROW) == "L0",
    );
    explorer.handle_event(char_event(AsciiChar::ESCAPE, KeyModifiers::empty()));

    select_by_name(explorer, "WIDE.TXT");
    press(explorer, KeyCode::ArrowRight, 1);
    press(explorer, KeyCode::End, 1);
    let scrolled_columns = WIDE_LINE_BYTES + 1 - GRID_COLUMNS;
    check(
        "scrolled right",
        explorer.editor.left_column == scrolled_columns,
    );
    check(
        "cursor in the last column",
        explorer.grid.cell_style(FIRST_BODY_ROW, GRID_COLUMNS - 1) == CellStyle::Cursor,
    );
    press(explorer, KeyCode::Home, 1);
    check("scrolled back left", explorer.editor.left_column == 0);
    explorer.handle_event(char_event(AsciiChar::ESCAPE, KeyModifiers::empty()));
    explorer.update_grid();
}

fn check_help_and_quit(explorer: &mut FileExplorer) {
    type_char(explorer, '?');
    check("help view", explorer.view == FeView::Help);
    check(
        "help text",
        explorer.grid.row_text(FIRST_BODY_ROW) == " directory",
    );
    type_char(explorer, 'x');
    check("any key leaves help", explorer.view == FeView::Directory);

    type_char(explorer, 'q');
    check("q quits", explorer.is_quit_requested);

    explorer.is_quit_requested = false;
    select_by_name(explorer, "A.TXT");
    press(explorer, KeyCode::ArrowRight, 1);
    type_char(explorer, 'z');
    control(explorer, 'c');
    check(
        "ctrl+c quits from a modified file",
        explorer.is_quit_requested,
    );
    let (bytes, len) = read_file(SHORT_FILE);
    check("ctrl+c saved nothing", &bytes[..len] == b"a");
}

fn cleanup() {
    for file in [
        BIG_FILE, INNER_FILE, NOTES_FILE, SHORT_FILE, BLOB_FILE, LINES_FILE, WIDE_FILE,
    ] {
        check("delete a test file", sys_delete(file).is_ok());
    }
    check("delete the subdirectory", sys_delete(SUB_DIR).is_ok());
    check("delete the test directory", sys_delete(TEST_DIR).is_ok());
}

struct SliceWriter<'a> {
    bytes: &'a mut [u8],
    len: usize,
}

impl core::fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let end = self.len + text.len();
        if end > self.bytes.len() {
            return Err(core::fmt::Error);
        }
        self.bytes[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;

        Ok(())
    }
}

extern "C" fn fetest_main() -> ! {
    check_date_time();
    check_entry_mode();
    check_text_buffer();
    check_input_translation();
    check_grid_render();

    if setup_files() {
        let mut explorer = FileExplorer::new(WIDTH_PX, HEIGHT_PX, &path(TEST_DIR));
        check_listing(&mut explorer);
        check_navigation(&mut explorer);
        check_editing(&mut explorer);
        check_binary_read_only(&mut explorer);
        check_big_text(&mut explorer);
        check_scrolling(&mut explorer);
        check_permissions(&mut explorer);
        check_help_and_quit(&mut explorer);
    } else {
        check("create the test files", false);
    }
    cleanup();

    program_shared_data().test_counters[COUNTER_CHECKS_DONE].store(1, Ordering::Release);
    sys_exit(0)
}
