#![no_std]
#![no_main]

use core::fmt::Write;
use core::sync::atomic::Ordering;
use rustspace::fs::path::FsPath;
use rustspace::shell::history::{CommandHistory, HISTORY_CAPACITY, HISTORY_ENTRY_MAX_BYTES};
use rustspace::shell::{
    CommandAction, EXIT_CODE_FAILURE, EXIT_CODE_SUCCESS, EXIT_CODE_TERMINATED, ShellOutput,
    ShellState, run_command_line,
};
extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use rustspace::gfx::surface::UserSurface;
use rustspace::terminal::input::{InputAction, translate, word_end_after, word_start_before};
use rustspace::terminal::scrollback::Scrollback;
use rustspace::terminal::{FontMetrics, Terminal};
use rustspace::{
    ArgSlice, Args, AsciiChar, BIGPROC_PID, EventType, FD_FLAG_READ, FD_FLAG_WRITE, File,
    InputEvent, KeyCode, KeyModifiers, KeyState, ProcessCreateFlags, ProcessCreateInfo,
    ProcessDetailsFlat, ProcessRunState, SyscallError, program_shared_data, sys_create_file,
    sys_create_process, sys_exit, sys_get_pid, sys_get_process_info, sys_sleep_ms,
    sys_wait_process,
};

// Indexes into the shared test counters, test_user_shell uses the same values
const COUNTER_CHECKS_PASSED: usize = 0;
const COUNTER_CHECKS_FAILED: usize = 1;
const COUNTER_CHECKS_DONE: usize = 2;
const COUNTER_EXITED_CHILD_PID: usize = 3;
const COUNTER_KILLED_CHILD_PID: usize = 4;

const MODE_EXIT: &str = "exit";
const MODE_HANG: &str = "hang";
const MODE_SHUTDOWN: &str = "shutdown";

const OWN_PATH: &str = "/BIN/shtest";
const OWN_NAME: &str = "shtest";
const CHILD_EXIT_CODE: i32 = 7;
const CHILD_EXIT_ARGS: &[u8] = b"exit\x007\x00";
const CHILD_EXIT_ARGC: u32 = 2;
const CHILD_HANG_ARGS: &[u8] = b"hang\x00";
const CHILD_HANG_ARGC: u32 = 1;
const HANG_SLEEP_MS: u64 = 1000;
const INTERRUPT_POLL_MS: u64 = 10;
const INTERRUPT_POLL_LIMIT: usize = 500;
const MISSING_PID: u32 = 1_000_000;
// the child takes the priority of the test through INHERIT_PRIORITY
const IGNORED_PRIORITY: u8 = 0;

const CAPTURE_BYTES: usize = 4096;
const LINE_BUFFER_BYTES: usize = 32;
const BIG_FILE_BYTES: usize = 10_000;
const BIG_FILE_PATTERN_MODULUS: usize = 251;

// a narrow terminal, so typed lines wrap early
const TERMINAL_COLUMNS: u32 = 20;
const TERMINAL_MARGIN_LEFT_PX: u32 = 4;
const TERMINAL_WIDTH_PX: u32 =
    TERMINAL_MARGIN_LEFT_PX + TERMINAL_COLUMNS * FontMetrics::FONT_8X13.char_width;
const TERMINAL_HEIGHT_PX: u32 = 64;
const TERMINAL_SCROLLBACK_VIEW_BYTES: usize = 4096;
const TERMINAL_WINDOW_ID: u32 = 0;
const WRAPPED_INPUT_BYTES: usize = 70;
const WRAPPED_CURSOR_BACK: usize = 33;

const EXIT_CODE_PANIC: i32 = 255;
const EXIT_CODE_BAD_ARGS: i32 = 2;
const EXIT_CODE_POWER_FAILED: i32 = 3;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys_exit(EXIT_CODE_PANIC)
}

// the kernel enters with a 16 byte aligned rsp, the ABI wants it 8 off at function entry
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "call {shtest_main}",
        "ud2",
        shtest_main = sym shtest_main,
    )
}

fn check(name: &str, condition: bool) {
    let counters = &program_shared_data().test_counters;
    if condition {
        counters[COUNTER_CHECKS_PASSED].fetch_add(1, Ordering::AcqRel);
        return;
    }

    counters[COUNTER_CHECKS_FAILED].fetch_add(1, Ordering::AcqRel);
    rustspace::println!("shtest: FAILED: {}", name);
}

/// Keeps the output of one command line, raw like a file would
struct Capture {
    bytes: [u8; CAPTURE_BYTES],
    len: usize,
}

impl Capture {
    const fn new() -> Self {
        Self {
            bytes: [0; CAPTURE_BYTES],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("<not utf-8>")
    }

    fn has_line(&self, line: &str) -> bool {
        self.as_str().lines().any(|candidate| candidate == line)
    }
}

impl Write for Capture {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        self.write_bytes(text.as_bytes());

        Ok(())
    }
}

impl ShellOutput for Capture {
    fn write_bytes(&mut self, bytes: &[u8]) {
        let copy_len = bytes.len().min(CAPTURE_BYTES - self.len);
        self.bytes[self.len..self.len + copy_len].copy_from_slice(&bytes[..copy_len]);
        self.len += copy_len;
    }
}

struct Shell {
    state: ShellState,
    capture: Capture,
}

impl Shell {
    const fn new() -> Self {
        Self {
            state: ShellState::new(),
            capture: Capture::new(),
        }
    }

    /// Runs a line that has to stay a builtin, returns its output
    fn run(&mut self, line: &str) -> &str {
        self.capture = Capture::new();
        let outcome = run_command_line(&mut self.state, line, &mut self.capture);
        check(
            "a builtin line is done in the shell",
            matches!(outcome, CommandAction::Done),
        );
        if !matches!(outcome, CommandAction::Done) {
            rustspace::println!("shtest: line '{}' was not done", line);
        }

        self.capture.as_str()
    }

    /// Runs a line and checks its output and $?
    fn expect(&mut self, line: &str, expected_output: &str, expected_exit_code: i32) {
        let matches_output = self.run(line) == expected_output;
        if !matches_output {
            rustspace::println!(
                "shtest: '{}' printed '{}', expected '{}'",
                line,
                self.capture.as_str(),
                expected_output
            );
        }
        check(line, matches_output);
        check(
            "the exit code of the line",
            self.state.last_exit_code == expected_exit_code,
        );
    }

    fn expect_ok(&mut self, line: &str) {
        self.expect(line, "", EXIT_CODE_SUCCESS);
    }

    /// Runs a line that has to leave a program to start
    fn expect_program(&mut self, line: &str, expected_path: &str, expected_args: &str) {
        self.capture = Capture::new();
        let outcome = run_command_line(&mut self.state, line, &mut self.capture);
        let matches = match outcome {
            CommandAction::StartProgram { path, args, .. } => {
                path.as_str() == expected_path && args == expected_args
            }
            CommandAction::Done | CommandAction::ClearScreen => false,
        };
        check(line, matches);
    }
}

fn check_paths() {
    let root = FsPath::ROOT_DIR_PATH;
    let resolve = |cwd: &FsPath, input: &str| {
        FsPath::resolve_from_cwd(cwd, input).unwrap_or(FsPath::ROOT_DIR_PATH)
    };

    check("root is /", root.as_str() == "/");
    check("a/b", resolve(&root, "a/b").as_str() == "/a/b");
    check(
        "double separators",
        resolve(&root, "//a///b/").as_str() == "/a/b",
    );
    check("dots", resolve(&root, "./a/./b/..").as_str() == "/a");
    check("above the root", resolve(&root, "../../..").as_str() == "/");
    let cwd = resolve(&root, "/x/y");
    check("relative to cwd", resolve(&cwd, "z").as_str() == "/x/y/z");
    check("parent of cwd", resolve(&cwd, "..").as_str() == "/x");
    check("absolute ignores cwd", resolve(&cwd, "/q").as_str() == "/q");
    check("file name", cwd.file_name() == "y");

    let mut too_long = root;
    let mut overflowed = false;
    for _ in 0..rustspace::fs::path::MAX_PATH_BYTES {
        overflowed |= too_long.push_component("abcdefgh").is_err();
    }
    check("a path past the limit is refused", overflowed);
}

fn check_history() {
    let mut history = CommandHistory::new();
    check(
        "empty history has nothing older",
        history.step_to_older().is_none(),
    );
    check(
        "empty history has nothing newer",
        history.step_to_newer().is_none(),
    );

    history.push_line(b"ls");
    history.push_line(b"pwd");
    history.push_line(b"pwd");
    history.push_line(b"");
    history.push_line(&[b'x'; HISTORY_ENTRY_MAX_BYTES + 1]);
    check(
        "empty, too long and repeated lines are skipped",
        history.len() == 2,
    );

    check(
        "up recalls the newest",
        history.step_to_older() == Some(b"pwd".as_slice()),
    );
    check(
        "up again goes older",
        history.step_to_older() == Some(b"ls".as_slice()),
    );
    check("up stops at the oldest", history.step_to_older().is_none());
    check(
        "down goes newer",
        history.step_to_newer() == Some(b"pwd".as_slice()),
    );
    check(
        "down past the newest empties the line",
        history.step_to_newer() == Some(b"".as_slice()),
    );
    check(
        "down with nothing recalled does nothing",
        history.step_to_newer().is_none(),
    );

    history.step_to_older();
    history.push_line(b"cd /");
    check("a push ends browsing", history.step_to_newer().is_none());
    check(
        "the pushed line is the newest",
        history.step_to_older() == Some(b"cd /".as_slice()),
    );

    let mut full = CommandHistory::new();
    for index in 0..HISTORY_CAPACITY + 1 {
        let line = [
            b'a' + (index % 2) as u8,
            b'0' + (index / 10) as u8,
            b'0' + (index % 10) as u8,
        ];
        full.push_line(&line);
    }
    check(
        "a full history keeps its capacity",
        full.len() == HISTORY_CAPACITY,
    );
    check(
        "a full history drops the oldest",
        full.entry_at(0).as_bytes() == b"b01",
    );
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

fn key_event(key: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent {
        event_type: EventType::KeyEvent,
        _pad: [0; 3],
        value: key as u32,
        extra: KeyState::Pressed as u32,
        modifiers: modifiers.bits(),
    }
}

fn backspace_event(modifiers: KeyModifiers) -> InputEvent {
    char_event(AsciiChar::BACKSPACE, modifiers)
}

fn check_line_editing() {
    check("word of an empty line", word_start_before(b"") == 0);
    check("single word", word_start_before(b"ls") == 0);
    check("last word", word_start_before(b"cat A.TXT") == 4);
    check(
        "trailing whitespace goes with the word",
        word_start_before(b"cat A.TXT  ") == 4,
    );
    check(
        "only whitespace delimits",
        word_start_before(b"cd /SHT/./a-b.c") == 3,
    );
    check("tabs delimit", word_start_before(b"echo\tx") == 5);
    check("only whitespace", word_start_before(b"   ") == 0);

    check(
        "backspace without alt",
        translate(backspace_event(KeyModifiers::empty())) == Some(InputAction::Backspace),
    );
    check(
        "left alt backspace deletes a word",
        translate(backspace_event(KeyModifiers::LALT)) == Some(InputAction::DeleteWord),
    );
    check(
        "right alt backspace deletes one char",
        translate(backspace_event(KeyModifiers::RALT)) == Some(InputAction::Backspace),
    );

    check("word end of an empty line", word_end_after(b"", 0) == 0);
    check("word end at the end", word_end_after(b"ls", 2) == 2);
    check("word end from inside", word_end_after(b"cat A.TXT", 1) == 3);
    check(
        "word end skips whitespace first",
        word_end_after(b"cat  A.TXT x", 3) == 10,
    );

    check(
        "left without alt",
        translate(key_event(KeyCode::ArrowLeft, KeyModifiers::empty()))
            == Some(InputAction::ArrowLeft),
    );
    check(
        "alt left moves a word",
        translate(key_event(KeyCode::ArrowLeft, KeyModifiers::LALT)) == Some(InputAction::WordLeft),
    );
    check(
        "alt right moves a word",
        translate(key_event(KeyCode::ArrowRight, KeyModifiers::LALT))
            == Some(InputAction::WordRight),
    );
    check(
        "a left alt chord types nothing",
        translate(char_event('n', KeyModifiers::LALT)) == Some(InputAction::Ignore),
    );
    check(
        "right alt left moves one char",
        translate(key_event(KeyCode::ArrowLeft, KeyModifiers::RALT))
            == Some(InputAction::ArrowLeft),
    );
    check(
        "right alt still types",
        translate(char_event('n', KeyModifiers::RALT)) == Some(InputAction::Char('n')),
    );
}

/// A terminal drawing into heap buffers, events go through the same path as in the shell window
struct TestTerminal {
    terminal: Terminal<UserSurface>,
    // the terminal points into these
    _pixels: Vec<u32>,
    _pixels_second: Vec<u32>,
    _scrollback_bytes: Vec<u8>,
}

impl TestTerminal {
    fn new() -> Self {
        let pixel_count = (TERMINAL_WIDTH_PX * TERMINAL_HEIGHT_PX) as usize;
        let mut pixels = vec![0u32; pixel_count];
        let mut pixels_second = vec![0u32; pixel_count];
        // the real scrollback is mapped twice back to back, writes and reads may run past the view
        let mut scrollback_bytes = vec![0u8; TERMINAL_SCROLLBACK_VIEW_BYTES * 2];

        let surface = unsafe {
            UserSurface::new(
                pixels.as_mut_ptr(),
                pixels_second.as_mut_ptr(),
                TERMINAL_WIDTH_PX,
                TERMINAL_HEIGHT_PX,
            )
        };
        let scrollback = Scrollback {
            base: scrollback_bytes.as_mut_ptr(),
            view_size: TERMINAL_SCROLLBACK_VIEW_BYTES,
            total_size: TERMINAL_SCROLLBACK_VIEW_BYTES * 2,
            relative: 0,
            absolute_filled: 0,
        };
        let mut terminal = Terminal::new(surface, TERMINAL_WINDOW_ID, scrollback);
        terminal.write_prompt();

        Self {
            terminal,
            _pixels: pixels,
            _pixels_second: pixels_second,
            _scrollback_bytes: scrollback_bytes,
        }
    }

    fn type_text(&mut self, text: &[u8]) {
        for &byte in text {
            self.terminal
                .handle_event(char_event(byte as char, KeyModifiers::empty()));
        }
    }

    fn press(&mut self, key: KeyCode, modifiers: KeyModifiers, times: usize) {
        for _ in 0..times {
            self.terminal.handle_event(key_event(key, modifiers));
        }
    }

    fn backspace(&mut self, modifiers: KeyModifiers) {
        self.terminal.handle_event(backspace_event(modifiers));
    }

    fn is_at(&self, typed: &[u8], cursor_offset: usize) -> bool {
        self.terminal.typed_input() == typed && self.terminal.cursor_offset() == cursor_offset
    }
}

fn ctrl_c(term: &mut TestTerminal) {
    term.terminal
        .handle_event(char_event('c', KeyModifiers::LCTRL));
}

fn check_terminal_interrupt() {
    let mut term = TestTerminal::new();
    term.type_text(b"ls /");
    ctrl_c(&mut term);
    check("ctrl+c drops the typed line", term.is_at(b"", 0));

    check(
        "start a job that hangs",
        term.terminal.job.start_program(OWN_PATH, MODE_HANG).is_ok(),
    );
    ctrl_c(&mut term);
    for _ in 0..INTERRUPT_POLL_LIMIT {
        term.terminal.poll_job();
        if !term.terminal.job.is_running() {
            break;
        }
        sys_sleep_ms(INTERRUPT_POLL_MS);
    }
    check("ctrl+c ends the job", !term.terminal.job.is_running());
    check(
        "the job counts as terminated",
        term.terminal.shell_state.last_exit_code == EXIT_CODE_TERMINATED,
    );
    check("a new prompt follows the job", term.is_at(b"", 0));
}

fn check_terminal_cursor() {
    let mut term = TestTerminal::new();
    let none = KeyModifiers::empty();
    let alt = KeyModifiers::LALT;

    term.type_text(b"cat a.txt");
    check(
        "typing keeps the cursor at the end",
        term.is_at(b"cat a.txt", 9),
    );
    term.press(KeyCode::ArrowLeft, none, 3);
    term.type_text(b"X");
    check("a char goes in at the cursor", term.is_at(b"cat a.Xtxt", 7));
    term.backspace(none);
    check("backspace at the cursor", term.is_at(b"cat a.txt", 6));

    term.press(KeyCode::ArrowLeft, alt, 1);
    check(
        "alt left inside a word goes to its start",
        term.is_at(b"cat a.txt", 4),
    );
    term.press(KeyCode::ArrowLeft, alt, 1);
    check(
        "alt left goes to the previous word",
        term.is_at(b"cat a.txt", 0),
    );
    term.press(KeyCode::ArrowLeft, alt, 1);
    term.press(KeyCode::ArrowLeft, none, 1);
    check("left stops at the prompt", term.is_at(b"cat a.txt", 0));
    term.press(KeyCode::ArrowRight, alt, 1);
    check(
        "alt right goes to the end of the word",
        term.is_at(b"cat a.txt", 3),
    );
    term.press(KeyCode::ArrowRight, alt, 1);
    check("alt right skips whitespace", term.is_at(b"cat a.txt", 9));
    term.press(KeyCode::ArrowRight, alt, 1);
    term.press(KeyCode::ArrowRight, none, 1);
    check("right stops at the end", term.is_at(b"cat a.txt", 9));

    term.press(KeyCode::ArrowLeft, none, 5);
    term.backspace(alt);
    check("alt backspace at the cursor", term.is_at(b"a.txt", 0));
    term.backspace(none);
    term.backspace(alt);
    check(
        "deleting at the prompt does nothing",
        term.is_at(b"a.txt", 0),
    );

    term.terminal.handle_event(char_event('n', alt));
    check("a left alt chord types nothing", term.is_at(b"a.txt", 0));

    let mut wrapped = TestTerminal::new();
    let mut long_input = [b'w'; WRAPPED_INPUT_BYTES];
    long_input[WRAPPED_INPUT_BYTES / 2] = b' ';
    wrapped.type_text(&long_input);
    wrapped.press(KeyCode::ArrowLeft, none, WRAPPED_CURSOR_BACK);
    wrapped.type_text(b"I");

    let insert_at = WRAPPED_INPUT_BYTES - WRAPPED_CURSOR_BACK;
    let mut expected = [0u8; WRAPPED_INPUT_BYTES + 1];
    expected[..insert_at].copy_from_slice(&long_input[..insert_at]);
    expected[insert_at] = b'I';
    expected[insert_at + 1..].copy_from_slice(&long_input[insert_at..]);
    check(
        "insert into wrapped input",
        wrapped.is_at(&expected, insert_at + 1),
    );
    wrapped.backspace(none);
    check(
        "backspace in wrapped input",
        wrapped.is_at(&long_input, insert_at),
    );
    wrapped.terminal.render();
}

fn check_directories(shell: &mut Shell) {
    shell.expect("pwd", "/\n", EXIT_CODE_SUCCESS);
    shell.expect_ok("mkdir /SHT");
    shell.expect_ok("cd SHT");
    shell.expect("pwd", "/SHT\n", EXIT_CODE_SUCCESS);
    shell.expect_ok("cd ..");
    shell.expect("pwd", "/\n", EXIT_CODE_SUCCESS);
    shell.expect_ok("cd /SHT/./../SHT/");
    shell.expect("pwd", "/SHT\n", EXIT_CODE_SUCCESS);
    shell.expect("cd /NOPE", "cd: /NOPE: not found\n", EXIT_CODE_FAILURE);
    shell.expect("pwd", "/SHT\n", EXIT_CODE_SUCCESS);
    shell.expect("cd a b", "usage: cd [dir]\n", EXIT_CODE_FAILURE);
    shell.expect_ok("cd");
    shell.expect("pwd", "/\n", EXIT_CODE_SUCCESS);
    shell.expect_ok("cd /SHT");
}

fn check_files(shell: &mut Shell) {
    shell.expect_ok("mkf A.TXT");
    shell.expect(
        "mkf A.TXT",
        "mkf: /SHT/A.TXT: already exists\n",
        EXIT_CODE_FAILURE,
    );
    shell.expect(
        "cd A.TXT",
        "cd: /SHT/A.TXT: not a directory\n",
        EXIT_CODE_FAILURE,
    );
    shell.expect("cat A.TXT", "", EXIT_CODE_SUCCESS);

    check("write A.TXT", write_text_file("/SHT/A.TXT", b"x\ny\n"));
    shell.expect("cat /SHT/A.TXT", "x\ny\n", EXIT_CODE_SUCCESS);
    check("create NEW.TXT", sys_create_file("/SHT/NEW.TXT").is_ok());
    check("write NEW.TXT", write_text_file("/SHT/NEW.TXT", b"new\n"));
    shell.expect("cat NEW.TXT A.TXT", "new\nx\ny\n", EXIT_CODE_SUCCESS);
    shell.expect(
        "cat NONE.TXT",
        "cat: /SHT/NONE.TXT: not found\n",
        EXIT_CODE_FAILURE,
    );
    shell.expect("cat", "usage: cat <file>...\n", EXIT_CODE_FAILURE);
}

fn write_text_file(path: &str, text: &[u8]) -> bool {
    let Ok(mut file) = File::open(path, FD_FLAG_WRITE) else {
        return false;
    };

    file.write(text) == Ok(text.len())
}

fn write_big_file(path: &str) -> bool {
    if sys_create_file(path).is_err() {
        return false;
    }
    let Ok(mut file) = File::open(path, FD_FLAG_WRITE) else {
        return false;
    };

    let mut pattern = [0u8; BIG_FILE_BYTES];
    for (index, byte) in pattern.iter_mut().enumerate() {
        *byte = (index % BIG_FILE_PATTERN_MODULUS) as u8;
    }

    file.write(&pattern) == Ok(BIG_FILE_BYTES)
}

fn has_big_file_content(path: &str) -> bool {
    let Ok(mut file) = File::open(path, FD_FLAG_READ) else {
        return false;
    };

    let mut content = [0u8; BIG_FILE_BYTES + 1];
    let mut read_total = 0;
    loop {
        match file.read(&mut content[read_total..]) {
            Ok(0) => break,
            Ok(read) => read_total += read,
            Err(_) => return false,
        }
    }

    read_total == BIG_FILE_BYTES
        && content[..BIG_FILE_BYTES]
            .iter()
            .enumerate()
            .all(|(index, &byte)| byte == (index % BIG_FILE_PATTERN_MODULUS) as u8)
}

fn check_copy(shell: &mut Shell) {
    shell.expect_ok("cp A.TXT B.TXT");
    shell.expect("cat B.TXT", "x\ny\n", EXIT_CODE_SUCCESS);
    shell.expect(
        "cp A.TXT /SHT",
        "cp: source and target are the same file\n",
        EXIT_CODE_FAILURE,
    );
    shell.expect_ok("mkdir SUB");
    shell.expect_ok("cp A.TXT SUB");
    shell.expect("cat SUB/A.TXT", "x\ny\n", EXIT_CODE_SUCCESS);
    shell.expect_ok("cp NEW.TXT B.TXT");
    shell.expect("cat B.TXT", "new\n", EXIT_CODE_SUCCESS);
    shell.expect(
        "cp SUB X.TXT",
        "cp: /SHT/SUB: is a directory\n",
        EXIT_CODE_FAILURE,
    );
    shell.expect("cp A.TXT", "usage: cp <file> <target>\n", EXIT_CODE_FAILURE);

    check("write the big file", write_big_file("/SHT/BIG.BIN"));
    shell.expect_ok("cp BIG.BIN SUB/BIG2.BIN");
    check(
        "a copy over many chunks is exact",
        has_big_file_content("/SHT/SUB/BIG2.BIN"),
    );
}

fn check_listing(shell: &mut Shell) {
    shell.run("ls");
    for line in ["A.TXT", "B.TXT", "NEW.TXT", "BIG.BIN", "SUB/"] {
        check(line, shell.capture.has_line(line));
    }
    check(
        "ls lists directories first, then names in order",
        shell
            .capture
            .as_str()
            .lines()
            .eq(["SUB/", "A.TXT", "B.TXT", "BIG.BIN", "NEW.TXT"]),
    );

    shell.run("ls -l");
    check(
        "ls -l shows a file",
        shell.capture.has_line("-rw-          4 A.TXT"),
    );
    check(
        "ls -l shows a size",
        shell.capture.has_line("-rw-      10000 BIG.BIN"),
    );
    check(
        "ls -l shows a directory",
        shell.capture.has_line("dr--          0 SUB/"),
    );

    shell.expect("ls SUB", "A.TXT\nBIG2.BIN\n", EXIT_CODE_SUCCESS);
    shell.expect("ls A.TXT", "A.TXT\n", EXIT_CODE_SUCCESS);
    shell.expect("ls -x", "usage: ls [-l] [path]\n", EXIT_CODE_FAILURE);

    shell.run("ls /");
    check("ls / shows /BIN", shell.capture.has_line("BIN/"));
    check("ls / shows /SHT", shell.capture.has_line("SHT/"));

    shell.run("stat A.TXT");
    check("stat path", shell.capture.has_line("path       /SHT/A.TXT"));
    check("stat type", shell.capture.has_line("type       file"));
    check("stat size", shell.capture.has_line("size       4 bytes"));
    check(
        "stat times",
        shell.capture.as_str().contains("created    ")
            && shell.capture.as_str().contains("modified   "),
    );
    shell.run("stat /");
    check(
        "stat of the root",
        shell.capture.has_line("type       directory"),
    );
}

fn check_remove(shell: &mut Shell) {
    shell.expect(
        "rm SUB",
        "rm: /SHT/SUB: is a directory\n",
        EXIT_CODE_FAILURE,
    );
    shell.expect_ok("mkdir SUB/DEEP");
    shell.expect_ok("mkf SUB/DEEP/F.TXT");
    shell.expect_ok("rm -r SUB");
    shell.expect("ls SUB", "ls: /SHT/SUB: not found\n", EXIT_CODE_FAILURE);
    shell.expect_ok("rm A.TXT B.TXT");
    shell.expect(
        "cat A.TXT",
        "cat: /SHT/A.TXT: not found\n",
        EXIT_CODE_FAILURE,
    );
    shell.expect("rm /", "rm: refusing to remove /\n", EXIT_CODE_FAILURE);
    shell.expect("rm", "usage: rm [-r] <path>...\n", EXIT_CODE_FAILURE);
}

fn check_exit_codes(shell: &mut Shell) {
    shell.expect(
        "cat NONE.TXT",
        "cat: /SHT/NONE.TXT: not found\n",
        EXIT_CODE_FAILURE,
    );
    shell.expect("echo $?", "1\n", EXIT_CODE_SUCCESS);
    shell.expect("echo $?", "0\n", EXIT_CODE_SUCCESS);
    shell.expect("echo", "\n", EXIT_CODE_SUCCESS);
}

fn check_programs(shell: &mut Shell) {
    shell.expect_program("lsproc", "/BIN/lsproc", "");
    shell.expect_program("nosuchpr a  b", "/BIN/nosuchpr", "a  b");
    shell.expect_program("run lsproc x", "/BIN/lsproc", "x");
    shell.expect_program("run ./PROG", "/SHT/PROG", "");
    shell.expect_program("../BIN/outwrite basic", "/BIN/outwrite", "basic");
    shell.expect("run", "usage: run <program> [args]\n", EXIT_CODE_FAILURE);
}

fn check_help_and_system(shell: &mut Shell) {
    shell.run("help");
    check(
        "help lists seeproc",
        shell.capture.as_str().contains("- seeproc <pid>"),
    );
}

fn spawn_child(packed_args: &[u8], argc: u32) -> Result<u32, SyscallError> {
    sys_create_process(
        &ProcessCreateInfo::new(OWN_PATH, IGNORED_PRIORITY)
            .with_args(packed_args, argc)
            .with_flags(ProcessCreateFlags::INHERIT_PRIORITY),
    )
}

fn check_processes(shell: &mut Shell) {
    let counters = &program_shared_data().test_counters;
    let own_pid = sys_get_pid();

    let Ok(exiting_pid) = spawn_child(CHILD_EXIT_ARGS, CHILD_EXIT_ARGC) else {
        check("spawn the exiting child", false);
        return;
    };
    counters[COUNTER_EXITED_CHILD_PID].store(exiting_pid, Ordering::Release);
    check(
        "wait returns the exit code of the child",
        sys_wait_process(exiting_pid) == Ok(CHILD_EXIT_CODE),
    );
    check(
        "a removed child is gone",
        sys_wait_process(exiting_pid) == Err(SyscallError::PROCESS_NOT_FOUND),
    );
    let mut details = ProcessDetailsFlat::EMPTY;
    check(
        "a removed child has no details",
        sys_get_process_info(exiting_pid as usize, &mut details)
            == Err(SyscallError::PROCESS_NOT_FOUND),
    );

    check(
        "only the parent may wait",
        sys_wait_process(BIGPROC_PID as u32) == Err(SyscallError::PERMISSION_DENIED),
    );
    check(
        "waiting on itself is refused",
        sys_wait_process(own_pid as u32) == Err(SyscallError::INVALID_ARGUMENT),
    );
    check(
        "waiting on a missing pid",
        sys_wait_process(MISSING_PID) == Err(SyscallError::PROCESS_NOT_FOUND),
    );

    let Ok(hanging_pid) = spawn_child(CHILD_HANG_ARGS, CHILD_HANG_ARGC) else {
        check("spawn the hanging child", false);
        return;
    };
    counters[COUNTER_KILLED_CHILD_PID].store(hanging_pid, Ordering::Release);

    check(
        "details of a live child",
        sys_get_process_info(hanging_pid as usize, &mut details).is_ok(),
    );
    check("details pid", details.pid == hanging_pid as u64);
    check("details name", details.name() == OWN_NAME);
    check("details parent", details.parent_pid == own_pid as u64);
    check(
        "a live child is not terminated",
        details.run_state() != ProcessRunState::Terminated,
    );
    check("a live child has memory", details.mapped_bytes > 0);

    shell.run(LineBuffer::with_pid("seeproc", hanging_pid).as_str());
    check(
        "seeproc prints the name",
        shell.capture.has_line("name       shtest"),
    );
    check(
        "seeproc prints the state",
        shell.capture.has_line("state      running")
            || shell.capture.has_line("state      blocked")
            || shell.capture.has_line("state      ready"),
    );
    check(
        "seeproc keeps $? at 0",
        shell.state.last_exit_code == EXIT_CODE_SUCCESS,
    );

    shell.run("seeproc 1");
    check(
        "seeproc of bigproc",
        shell.capture.has_line("name       bigproc") && shell.capture.has_line("parent     none"),
    );
    shell.expect(
        "seeproc 0",
        "seeproc: pid 0: no such process\n",
        EXIT_CODE_FAILURE,
    );
    shell.expect(
        "seeproc 1000000",
        "seeproc: pid 1000000: no such process\n",
        EXIT_CODE_FAILURE,
    );
    shell.expect("seeproc x", "usage: seeproc <pid>\n", EXIT_CODE_FAILURE);

    shell.expect(
        "term 1",
        "term: pid 1: permission denied\n",
        EXIT_CODE_FAILURE,
    );
    shell.expect("term", "usage: term [-o] <pid>\n", EXIT_CODE_FAILURE);
    shell.expect_ok(LineBuffer::with_pid("term -o", hanging_pid).as_str());
    check(
        "a terminated child exits with the term code",
        sys_wait_process(hanging_pid) == Ok(EXIT_CODE_TERMINATED),
    );
}

/// A command line built at runtime, like `term 12`
struct LineBuffer {
    bytes: [u8; LINE_BUFFER_BYTES],
    len: usize,
}

impl LineBuffer {
    fn with_pid(command: &str, pid: u32) -> Self {
        let mut line = Self {
            bytes: [0; LINE_BUFFER_BYTES],
            len: 0,
        };
        let _ = write!(line, "{} {}", command, pid);

        line
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl Write for LineBuffer {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let end = self.len + text.len();
        if end > LINE_BUFFER_BYTES {
            return Err(core::fmt::Error);
        }
        self.bytes[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;

        Ok(())
    }
}

fn check_cleanup(shell: &mut Shell) {
    shell.expect_ok("cd /");
    shell.expect_ok("rm -r SHT");
    shell.run("ls");
    check(
        "the test directory is gone",
        !shell.capture.has_line("SHT/"),
    );
}

fn run_checks() -> ! {
    let mut shell = Shell::new();

    check_paths();
    check_history();
    check_line_editing();
    check_terminal_cursor();
    check_terminal_interrupt();
    check_directories(&mut shell);
    check_files(&mut shell);
    check_copy(&mut shell);
    check_listing(&mut shell);
    check_remove(&mut shell);
    check_exit_codes(&mut shell);
    check_programs(&mut shell);
    check_help_and_system(&mut shell);
    check_processes(&mut shell);
    check_cleanup(&mut shell);

    program_shared_data().test_counters[COUNTER_CHECKS_DONE].store(1, Ordering::Release);
    sys_exit(0)
}

/// Runs a power builtin, it returns only when powering off failed
fn run_power_builtin(line: &str) -> ! {
    let mut shell = Shell::new();
    let output = shell.run(line);
    rustspace::println!("shtest: {} returned: {}", line, output);

    sys_exit(EXIT_CODE_POWER_FAILED)
}

extern "C" fn shtest_main(argc: usize, argv: *const ArgSlice) -> ! {
    let args = unsafe { Args::from_entry(argc, argv) };

    match args.len() {
        0 => run_checks(),
        1 if args.get(0) == MODE_HANG => loop {
            sys_sleep_ms(HANG_SLEEP_MS);
        },
        1 if args.get(0) == MODE_SHUTDOWN => run_power_builtin(MODE_SHUTDOWN),
        2 if args.get(0) == MODE_EXIT => match args.get(1).parse::<i32>() {
            Ok(exit_code) => sys_exit(exit_code),
            Err(_) => sys_exit(EXIT_CODE_BAD_ARGS),
        },
        _ => sys_exit(EXIT_CODE_BAD_ARGS),
    }
}
