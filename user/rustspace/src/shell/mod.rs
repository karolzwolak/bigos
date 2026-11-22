#![allow(clippy::result_large_err)]

pub mod history;

use crate::fs::dir_listing::{DirListing, ParentEntry};
use crate::fs::path::{CURRENT_DIR_NAME, FsPath, PARENT_DIR_NAME};
use crate::fs::time::DateTime;
use crate::fs::{EntryMode, LIST_PAGE_ENTRIES_COUNT, stat_path};
use crate::{
    ChildPolicy, CpuInfoFlat, DirEntryFlat, FD_FLAG_READ, FD_FLAG_WRITE, File, FileAttributes,
    NO_PARENT_PID, PROCESS_NO_CORE_ASSIGNED, ProcessDetailsFlat, ProcessRunState, SyscallError,
    sys_create_dir, sys_create_file, sys_delete, sys_get_cpu_info, sys_get_process_info,
    sys_list_dir, sys_shutdown, sys_terminate_process,
};
use core::fmt::Write;

const BIN_DIR_NAME: &str = "BIN";
const MAX_COMMAND_ARGUMENTS: usize = 32;
const COPY_CHUNK_BYTES: usize = 4096;
const SIZE_COLUMN_WIDTH: usize = 10;
const DETAIL_LABEL_WIDTH: usize = 11;
const BYTES_PER_KIB: u64 = 1024;
const HZ_PER_MHZ: u64 = 1_000_000;
const WHOAMI_TEXT: &str = "a bigOS enjoyer";
pub const BIGOS_LOGO_LINES: [&str; 8] = [
    ".-.                 .-----. .-----.",
    "| |                 | .-. | | .---'",
    "| '---. .-. .-----. | | | | | '---.",
    "| .-. | | | | .-. | | | | | '---. |",
    "| '-' | | | | '-' | | '-' | .---' |",
    "'-----' '-' '---. | '-----' '-----'",
    "                | |",
    "                '-'",
];

pub const EXIT_CODE_SUCCESS: i32 = 0;
pub const EXIT_CODE_FAILURE: i32 = 1;
pub const EXIT_CODE_NOT_FOUND: i32 = 127;
pub const EXIT_CODE_TERMINATED: i32 = -1;

const LAST_EXIT_CODE_WORD: &str = "$?";
const OPTION_PREFIX: char = '-';
const OPTION_LONG: &str = "-l";
const OPTION_RECURSIVE: &str = "-r";
const OPTION_ORPHAN: &str = "-o";
const PATH_SEPARATOR: char = '/';
const UNPRINTABLE_REPLACEMENT: u8 = b'?';

const VENDOR_NAMES: [&str; 3] = ["unknown", "Intel", "AMD"];
/// mirrors the kernel's `CpuFeatureFlags`
const FEATURE_NAMES: [&str; 15] = [
    "apic",
    "x2apic",
    "tsc",
    "tsc_deadline",
    "pge",
    "pat",
    "sse",
    "sse2",
    "sse3",
    "sse4.1",
    "sse4.2",
    "avx",
    "aes",
    "rdrand",
    "hypervisor",
];

pub trait ShellOutput: Write {
    fn write_bytes(&mut self, bytes: &[u8]);
}

#[derive(Clone, Copy)]
pub struct ShellState {
    pub cwd: FsPath,
    pub last_exit_code: i32,
}

impl ShellState {
    pub const fn new() -> Self {
        Self {
            cwd: FsPath::ROOT_DIR_PATH,
            last_exit_code: EXIT_CODE_SUCCESS,
        }
    }
}

impl Default for ShellState {
    fn default() -> Self {
        Self::new()
    }
}

/// Keeps printable ASCII and line breaks, turns tabs into spaces and the rest into '?'
pub fn sanitize_for_screen(bytes: &mut [u8]) {
    for byte in bytes {
        *byte = match *byte {
            b' '..=b'~' | b'\n' | b'\r' => *byte,
            b'\t' => b' ',
            _ => UNPRINTABLE_REPLACEMENT,
        };
    }
}

#[allow(clippy::large_enum_variant)]
pub enum CommandAction<'a> {
    Done,
    ClearScreen,
    StartProgram {
        name: &'a str,
        path: FsPath,
        args: &'a str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellBuiltin {
    Help,
    Clear,
    Pwd,
    Cd,
    Ls,
    Cat,
    Mkdir,
    Mkf,
    Rm,
    Cp,
    Stat,
    Echo,
    Run,
    Term,
    Seeproc,
    Cpuinfo,
    Whoami,
    Bigos,
    Shutdown,
}

pub struct BuiltinInfo {
    pub builtin: ShellBuiltin,
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
}

macro_rules! builtin_table {
    ($($builtin:ident, $name:literal, $usage:literal, $summary:literal;)*) => {
        pub const BUILTINS: [BuiltinInfo; [$(ShellBuiltin::$builtin),*].len()] = [
            $(BuiltinInfo {
                builtin: ShellBuiltin::$builtin,
                name: $name,
                usage: $usage,
                summary: $summary,
            }),*
        ];
    };
}

builtin_table! {
    Help, "help", "help", "list the builtins";
    Clear, "clear", "clear", "clear the screen";
    Pwd, "pwd", "pwd", "print the working directory";
    Cd, "cd", "cd [dir]", "change the working directory";
    Ls, "ls", "ls [-l] [path]", "list a directory, -l to list permissions, type and size";
    Cat, "cat", "cat <file>...", "print file contents";
    Mkdir, "mkdir", "mkdir <dir>...", "create directories";
    Mkf, "mkf", "mkf <file>...", "create empty files";
    Rm, "rm", "rm [-r] <path>...", "delete files, -r to delete directories with their content";
    Cp, "cp", "cp <file> <target>", "copy a file";
    Stat, "stat", "stat <path>...", "print file details";
    Echo, "echo", "echo [text]", "print the entered text, echo $? prints the last exit code";
    Run, "run", "run <program> [args]", "start a program, a name without / is looked up in /BIN";
    Term, "term", "term [-o] <pid>", "terminate a process, -o keeps its children alive";
    Seeproc, "seeproc", "seeproc <pid>", "print process details";
    Cpuinfo, "cpuinfo", "cpuinfo", "print about about the CPU";
    Whoami, "whoami", "whoami", "if you feel lost";
    Bigos, "bigos", "bigos", "secret";
    Shutdown, "shutdown", "shutdown", "power the machine off";
}

pub fn find_builtin(name: &str) -> Option<&'static BuiltinInfo> {
    BUILTINS.iter().find(|info| info.name == name)
}

#[allow(clippy::large_enum_variant)]
enum CommandError {
    Usage,
    Path { path: FsPath, error: SyscallError },
    Process { pid: usize, error: SyscallError },
    Syscall(SyscallError),
    Message(&'static str),
}

fn path_error(path: &FsPath) -> impl FnOnce(SyscallError) -> CommandError + '_ {
    move |error| CommandError::Path { path: *path, error }
}

struct ExitCodeText {
    bytes: [u8; Self::CAPACITY],
    len: usize,
}

impl ExitCodeText {
    const CAPACITY: usize = 11;

    fn new(exit_code: i32) -> Self {
        let mut text = Self {
            bytes: [0; Self::CAPACITY],
            len: 0,
        };
        let _ = write!(text, "{}", exit_code);

        text
    }

    fn as_str(&self) -> &str {
        // SAFETY: only ASCII digits and '-' are written
        unsafe { core::str::from_utf8_unchecked(&self.bytes[..self.len]) }
    }
}

impl Write for ExitCodeText {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let end = self.len + text.len();
        debug_assert!(end <= Self::CAPACITY);

        self.bytes[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;

        Ok(())
    }
}

struct CommandArguments<'a> {
    arguments: [&'a str; MAX_COMMAND_ARGUMENTS],
    count: usize,
}

impl<'a> CommandArguments<'a> {
    fn split(text: &'a str, last_exit_code: &'a str) -> Result<Self, CommandError> {
        let mut arguments = Self {
            arguments: [""; MAX_COMMAND_ARGUMENTS],
            count: 0,
        };

        for word in text.split_ascii_whitespace() {
            if arguments.count == MAX_COMMAND_ARGUMENTS {
                return Err(CommandError::Message("too many arguments"));
            }
            arguments.arguments[arguments.count] = if word == LAST_EXIT_CODE_WORD {
                last_exit_code
            } else {
                word
            };
            arguments.count += 1;
        }

        Ok(arguments)
    }

    fn as_slice(&self) -> &[&'a str] {
        &self.arguments[..self.count]
    }
}

pub fn run_command_line<'a, O: ShellOutput>(
    state: &mut ShellState,
    line: &'a str,
    out: &mut O,
) -> CommandAction<'a> {
    let line = line.trim();
    let Some(command) = line.split_ascii_whitespace().next() else {
        return CommandAction::Done;
    };
    let rest = line[command.len()..].trim_start();

    let Some(info) = find_builtin(command) else {
        return run_program(state, command, rest, out);
    };

    match info.builtin {
        ShellBuiltin::Clear => {
            state.last_exit_code = EXIT_CODE_SUCCESS;
            CommandAction::ClearScreen
        }
        ShellBuiltin::Run => {
            let Some(program) = rest.split_ascii_whitespace().next() else {
                set_exit_code_and_print_error(info, Err(CommandError::Usage), state, out);
                return CommandAction::Done;
            };
            run_program(state, program, rest[program.len()..].trim_start(), out)
        }
        _ => {
            let result = run_builtin(info.builtin, state, rest, out);
            set_exit_code_and_print_error(info, result, state, out);
            CommandAction::Done
        }
    }
}

fn run_program<'a, O: ShellOutput>(
    state: &mut ShellState,
    name: &'a str,
    args: &'a str,
    out: &mut O,
) -> CommandAction<'a> {
    let resolved =
        if name.contains(PATH_SEPARATOR) || name == CURRENT_DIR_NAME || name == PARENT_DIR_NAME {
            FsPath::resolve_from_cwd(&state.cwd, name)
        } else {
            FsPath::ROOT_DIR_PATH
                .child_absolute_path(BIN_DIR_NAME)
                .and_then(|bin_dir| bin_dir.child_absolute_path(name))
        };

    match resolved {
        Ok(path) => CommandAction::StartProgram { name, path, args },
        Err(error) => {
            let _ = writeln!(out, "{}: {}", name, error);
            state.last_exit_code = EXIT_CODE_FAILURE;
            CommandAction::Done
        }
    }
}

fn set_exit_code_and_print_error<O: ShellOutput>(
    info: &BuiltinInfo,
    result: Result<(), CommandError>,
    state: &mut ShellState,
    out: &mut O,
) {
    let error = match result {
        Ok(()) => {
            state.last_exit_code = EXIT_CODE_SUCCESS;
            return;
        }
        Err(error) => error,
    };

    state.last_exit_code = EXIT_CODE_FAILURE;
    let _ = match error {
        CommandError::Usage => writeln!(out, "usage: {}", info.usage),
        CommandError::Path { path, error } => writeln!(out, "{}: {}: {}", info.name, path, error),
        CommandError::Process { pid, error } => {
            writeln!(out, "{}: pid {}: {}", info.name, pid, error)
        }
        CommandError::Syscall(error) => writeln!(out, "{}: {}", info.name, error),
        CommandError::Message(message) => writeln!(out, "{}: {}", info.name, message),
    };
}

fn run_builtin<O: ShellOutput>(
    builtin: ShellBuiltin,
    state: &mut ShellState,
    rest: &str,
    out: &mut O,
) -> Result<(), CommandError> {
    let last_exit_code = ExitCodeText::new(state.last_exit_code);
    let arguments = CommandArguments::split(rest, last_exit_code.as_str())?;

    execute_command(builtin, state, arguments.as_slice(), out)
}

fn execute_command<O: ShellOutput>(
    builtin: ShellBuiltin,
    state: &mut ShellState,
    args: &[&str],
    out: &mut O,
) -> Result<(), CommandError> {
    match builtin {
        ShellBuiltin::Help => {
            help(out);
            Ok(())
        }
        ShellBuiltin::Pwd => {
            let _ = writeln!(out, "{}", state.cwd);
            Ok(())
        }
        ShellBuiltin::Cd => cd(state, args),
        ShellBuiltin::Ls => ls(state, args, out),
        ShellBuiltin::Cat => do_for_each_path(state, args, |path| cat(path, out)),
        ShellBuiltin::Mkdir => do_for_each_path(state, args, |path| {
            sys_create_dir(path.as_str()).map_err(path_error(path))
        }),
        ShellBuiltin::Mkf => do_for_each_path(state, args, |path| {
            sys_create_file(path.as_str()).map_err(path_error(path))
        }),
        ShellBuiltin::Rm => rm(state, args),
        ShellBuiltin::Cp => cp(state, args),
        ShellBuiltin::Stat => do_for_each_path(state, args, |path| stat(path, out)),
        ShellBuiltin::Echo => {
            echo(args, out);
            Ok(())
        }
        ShellBuiltin::Term => term(args),
        ShellBuiltin::Seeproc => seeproc(args, out),
        ShellBuiltin::Cpuinfo => cpuinfo(out),
        ShellBuiltin::Whoami => {
            let _ = writeln!(out, "{}", WHOAMI_TEXT);
            Ok(())
        }
        ShellBuiltin::Bigos => {
            for line in BIGOS_LOGO_LINES {
                let _ = writeln!(out, "{}", line);
            }
            Ok(())
        }
        ShellBuiltin::Shutdown => Err(CommandError::Syscall(sys_shutdown())),
        ShellBuiltin::Clear | ShellBuiltin::Run => {
            debug_assert!(false, "{:?} reached execute_command", builtin);
            Ok(())
        }
    }
}

fn do_for_each_path(
    state: &ShellState,
    args: &[&str],
    mut action: impl FnMut(&FsPath) -> Result<(), CommandError>,
) -> Result<(), CommandError> {
    if args.is_empty() || args.iter().any(|arg| arg.starts_with(OPTION_PREFIX)) {
        return Err(CommandError::Usage);
    }

    for arg in args {
        let path = FsPath::resolve_from_cwd(&state.cwd, arg).map_err(CommandError::Syscall)?;
        action(&path)?;
    }

    Ok(())
}

fn help<O: ShellOutput>(out: &mut O) {
    for info in &BUILTINS {
        let _ = writeln!(out, "- {:<22} {}", info.usage, info.summary);
    }
}

fn cd(state: &mut ShellState, args: &[&str]) -> Result<(), CommandError> {
    let target = match args {
        [] => FsPath::ROOT_DIR_PATH,
        [path] => FsPath::resolve_from_cwd(&state.cwd, path).map_err(CommandError::Syscall)?,
        _ => return Err(CommandError::Usage),
    };

    let stat = stat_path(&target).map_err(path_error(&target))?;
    if stat.is_dir == 0 {
        return Err(path_error(&target)(SyscallError::NOT_A_DIRECTORY));
    }
    state.cwd = target;

    Ok(())
}

fn write_direntry<O: ShellOutput>(
    out: &mut O,
    name: &str,
    is_dir: bool,
    attributes: FileAttributes,
    size: u64,
    long: bool,
) {
    let dir_suffix = if is_dir { "/" } else { "" };
    let _ = if long {
        writeln!(
            out,
            "{} {:>width$} {}{}",
            EntryMode::new(is_dir, attributes),
            size,
            name,
            dir_suffix,
            width = SIZE_COLUMN_WIDTH
        )
    } else {
        writeln!(out, "{}{}", name, dir_suffix)
    };
}

fn ls<O: ShellOutput>(state: &ShellState, args: &[&str], out: &mut O) -> Result<(), CommandError> {
    let (long, paths) = match args.first() {
        Some(&OPTION_LONG) => (true, &args[1..]),
        _ => (false, args),
    };
    let path = match paths {
        [] => state.cwd,
        [path] if !path.starts_with(OPTION_PREFIX) => {
            FsPath::resolve_from_cwd(&state.cwd, path).map_err(CommandError::Syscall)?
        }
        _ => return Err(CommandError::Usage),
    };

    let stat = stat_path(&path).map_err(path_error(&path))?;
    if stat.is_dir == 0 {
        write_direntry(
            out,
            stat.name_str(),
            false,
            stat.attributes(),
            stat.size,
            long,
        );
        return Ok(());
    }

    let mut listing = DirListing::new();
    listing
        .load(&path, ParentEntry::Skip)
        .map_err(path_error(&path))?;
    for entry in &listing.entries {
        write_direntry(
            out,
            entry.name_str(),
            entry.is_dir != 0,
            entry.attributes(),
            entry.size,
            long,
        );
    }

    Ok(())
}

fn cat<O: ShellOutput>(path: &FsPath, out: &mut O) -> Result<(), CommandError> {
    let mut file = File::open(path.as_str(), FD_FLAG_READ).map_err(path_error(path))?;
    let mut chunk = [0u8; COPY_CHUNK_BYTES];

    loop {
        let read = file.read(&mut chunk).map_err(path_error(path))?;
        if read == 0 {
            return Ok(());
        }
        out.write_bytes(&chunk[..read]);
    }
}

fn delete_file_tree(path: &FsPath) -> Result<(), CommandError> {
    let mut entries = [DirEntryFlat::zeroed(); LIST_PAGE_ENTRIES_COUNT];

    loop {
        let listed = sys_list_dir(path.as_str(), 0, &mut entries).map_err(path_error(path))?;
        if listed == 0 {
            break;
        }

        for entry in &entries[..listed] {
            let child = path
                .child_absolute_path(entry.name_str())
                .map_err(path_error(path))?;
            if entry.is_dir != 0 {
                delete_file_tree(&child)?;
            } else {
                sys_delete(child.as_str()).map_err(path_error(&child))?;
            }
        }
    }

    sys_delete(path.as_str()).map_err(path_error(path))
}

fn rm(state: &ShellState, args: &[&str]) -> Result<(), CommandError> {
    let (recursive, paths) = match args.first() {
        Some(&OPTION_RECURSIVE) => (true, &args[1..]),
        _ => (false, args),
    };

    do_for_each_path(state, paths, |path| {
        if path.is_root() {
            return Err(CommandError::Message("refusing to remove /"));
        }

        let stat = stat_path(path).map_err(path_error(path))?;
        if stat.is_dir == 0 {
            sys_delete(path.as_str()).map_err(path_error(path))
        } else if recursive {
            delete_file_tree(path)
        } else {
            Err(path_error(path)(SyscallError::IS_A_DIRECTORY))
        }
    })
}

fn cp(state: &ShellState, args: &[&str]) -> Result<(), CommandError> {
    let [source, target] = args else {
        return Err(CommandError::Usage);
    };
    if source.starts_with(OPTION_PREFIX) || target.starts_with(OPTION_PREFIX) {
        return Err(CommandError::Usage);
    }

    let source = FsPath::resolve_from_cwd(&state.cwd, source).map_err(CommandError::Syscall)?;
    let source_stat = stat_path(&source).map_err(path_error(&source))?;
    if source_stat.is_dir != 0 {
        return Err(path_error(&source)(SyscallError::IS_A_DIRECTORY));
    }

    let mut target = FsPath::resolve_from_cwd(&state.cwd, target).map_err(CommandError::Syscall)?;
    let mut target_stat = stat_path(&target);
    if let Ok(stat) = target_stat
        && stat.is_dir != 0
    {
        target = target
            .child_absolute_path(source.file_name())
            .map_err(path_error(&target))?;
        target_stat = stat_path(&target);
    }

    match target_stat {
        Ok(_) if target == source => {
            return Err(CommandError::Message("source and target are the same file"));
        }
        Ok(stat) if stat.is_dir != 0 => {
            return Err(path_error(&target)(SyscallError::IS_A_DIRECTORY));
        }
        Ok(_) => sys_delete(target.as_str()).map_err(path_error(&target))?,
        Err(SyscallError::NOT_FOUND) => {}
        Err(error) => return Err(path_error(&target)(error)),
    }

    sys_create_file(target.as_str()).map_err(path_error(&target))?;
    let mut source_file = File::open(source.as_str(), FD_FLAG_READ).map_err(path_error(&source))?;
    let mut target_file =
        File::open(target.as_str(), FD_FLAG_WRITE).map_err(path_error(&target))?;

    let mut chunk = [0u8; COPY_CHUNK_BYTES];
    loop {
        let read = source_file.read(&mut chunk).map_err(path_error(&source))?;
        if read == 0 {
            return Ok(());
        }

        let mut written = 0;
        while written < read {
            written += target_file
                .write(&chunk[written..read])
                .map_err(path_error(&target))?;
        }
    }
}

fn write_time<O: ShellOutput>(out: &mut O, label: &str, unix_seconds: u32) {
    let _ = writeln!(
        out,
        "{:<width$}{}",
        label,
        DateTime::from_unix_seconds(unix_seconds),
        width = DETAIL_LABEL_WIDTH
    );
}

fn stat<O: ShellOutput>(path: &FsPath, out: &mut O) -> Result<(), CommandError> {
    let stat = stat_path(path).map_err(path_error(path))?;
    let kind = if stat.is_dir != 0 {
        "directory"
    } else {
        "file"
    };

    let _ = writeln!(
        out,
        "{:<width$}{}",
        "path",
        path,
        width = DETAIL_LABEL_WIDTH
    );
    let _ = writeln!(
        out,
        "{:<width$}{}",
        "type",
        kind,
        width = DETAIL_LABEL_WIDTH
    );
    let _ = writeln!(
        out,
        "{:<width$}{} bytes",
        "size",
        stat.size,
        width = DETAIL_LABEL_WIDTH
    );
    if !path.is_root() {
        write_time(out, "created", stat.created_time);
        write_time(out, "modified", stat.modified_time);
    }

    Ok(())
}

fn echo<O: ShellOutput>(args: &[&str], out: &mut O) {
    for (index, word) in args.iter().enumerate() {
        let separator = if index == 0 { "" } else { " " };
        let _ = write!(out, "{}{}", separator, word);
    }
    let _ = writeln!(out);
}

fn parse_pid(text: &str) -> Result<usize, CommandError> {
    text.parse::<usize>().map_err(|_| CommandError::Usage)
}

fn term(args: &[&str]) -> Result<(), CommandError> {
    let (child_policy, pid) = match args {
        [pid] => (ChildPolicy::KillChildren, pid),
        [option, pid] if *option == OPTION_ORPHAN => (ChildPolicy::OrphanChildren, pid),
        _ => return Err(CommandError::Usage),
    };
    let pid = parse_pid(pid)?;

    sys_terminate_process(pid, EXIT_CODE_TERMINATED, child_policy)
        .map_err(|error| CommandError::Process { pid, error })
}

fn seeproc<O: ShellOutput>(args: &[&str], out: &mut O) -> Result<(), CommandError> {
    let [pid] = args else {
        return Err(CommandError::Usage);
    };
    let pid = parse_pid(pid)?;

    let mut details = ProcessDetailsFlat::EMPTY;
    sys_get_process_info(pid, &mut details)
        .map_err(|error| CommandError::Process { pid, error })?;

    macro_rules! detail {
        ($label:literal, $($value:tt)*) => {
            let _ = write!(out, "{:<width$}", $label, width = DETAIL_LABEL_WIDTH);
            let _ = writeln!(out, $($value)*);
        };
    }

    let run_state = details.run_state();
    detail!("pid", "{}", details.pid);
    detail!("name", "{}", details.name());
    if details.parent_pid == NO_PARENT_PID {
        detail!("parent", "none");
    } else {
        detail!("parent", "{}", details.parent_pid);
    }
    detail!("state", "{}", run_state.name());
    if run_state == ProcessRunState::Terminated {
        detail!("exit code", "{}", details.exit_code);
    }
    detail!("priority", "{}", details.priority);
    if details.assigned_core == PROCESS_NO_CORE_ASSIGNED {
        detail!("core", "-");
    } else {
        detail!("core", "{}", details.assigned_core);
    }
    detail!("memory", "{} KiB", details.mapped_bytes / BYTES_PER_KIB);
    detail!("children", "{}", details.child_count);
    detail!("open files", "{}", details.open_fd_count);
    detail!("channels", "{}", details.channel_count);

    Ok(())
}

fn cpuinfo<O: ShellOutput>(out: &mut O) -> Result<(), CommandError> {
    let mut info = CpuInfoFlat::zeroed();
    if !sys_get_cpu_info(&mut info) {
        return Err(CommandError::Message("GetCpuInfo failed"));
    }

    let vendor = VENDOR_NAMES
        .get(info.vendor as usize)
        .copied()
        .unwrap_or(VENDOR_NAMES[0]);
    let _ = writeln!(
        out,
        "{:<w$}{} family {:#x} model {:#x} stepping {}",
        "cpu",
        vendor,
        info.display_family,
        info.display_model,
        info.stepping,
        w = DETAIL_LABEL_WIDTH
    );
    let _ = writeln!(
        out,
        "{:<w$}{}",
        "cores",
        info.core_count,
        w = DETAIL_LABEL_WIDTH
    );
    let _ = writeln!(
        out,
        "{:<w$}{} MHz",
        "tsc",
        info.tsc_frequency_hz / HZ_PER_MHZ,
        w = DETAIL_LABEL_WIDTH
    );
    let _ = writeln!(
        out,
        "{:<w$}{} bytes",
        "cache line",
        info.cache_line_size,
        w = DETAIL_LABEL_WIDTH
    );

    let _ = write!(out, "{:<w$}", "features", w = DETAIL_LABEL_WIDTH);
    for (bit, name) in FEATURE_NAMES.iter().enumerate() {
        if info.features & (1 << bit) != 0 {
            let _ = write!(out, "{} ", name);
        }
    }
    let _ = writeln!(out);

    let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    if let Some(uptime_s) = now_tsc
        .saturating_sub(info.boot_tsc)
        .checked_div(info.tsc_frequency_hz)
    {
        let _ = writeln!(
            out,
            "{:<w$}{} s",
            "uptime",
            uptime_s,
            w = DETAIL_LABEL_WIDTH
        );
    }

    Ok(())
}
