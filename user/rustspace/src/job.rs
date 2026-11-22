use crate::channel::{ChannelReader, ReadReadiness, STDOUT_CHANNEL_DATA_SIZE_BYTES};
use crate::{MAX_ARGC, ProcessCreateFlags, ProcessCreateInfo, SyscallError, sys_create_process};

/// Programs named without a / are looked up here
const BIN_DIR_PREFIX: &[u8] = b"/BIN/";
pub const JOB_PATH_MAX_BYTES: usize = 64;
pub const JOB_ARGS_MAX_BYTES: usize = 256;
const ARGUMENT_TERMINATOR: u8 = 0;
const PATH_SEPARATOR: char = '/';
// the child takes the shell's priority through INHERIT_PRIORITY
const IGNORED_PRIORITY: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobPoll {
    NoJob,
    Output(usize),
    /// the program runs did not write anything since the last poll
    NothingYet,
    /// the program is gone and all of its output was handed out
    Finished {
        pid: u32,
    },
}

/// A program the shell started with `run`, its stdout goes into a channel the shell reads
pub enum ShellJob {
    Idle,
    Running { reader: ChannelReader, pid: u32 },
}

impl ShellJob {
    pub const fn is_running(&self) -> bool {
        matches!(self, ShellJob::Running { .. })
    }

    /// The first word names the program, a name without a / is looked up in /BIN
    pub fn start(&mut self, command_line: &str) -> Result<u32, SyscallError> {
        debug_assert!(!self.is_running());

        let command_line = command_line.trim_start();
        let Some(program) = command_line.split_ascii_whitespace().next() else {
            return Err(SyscallError::INVALID_ARGUMENT);
        };
        let args = &command_line[program.len()..];

        let mut path_buffer = [0u8; JOB_PATH_MAX_BYTES];
        let prefix: &[u8] = if program.contains(PATH_SEPARATOR) {
            &[]
        } else {
            BIN_DIR_PREFIX
        };
        let path_len = prefix.len() + program.len();
        if path_len > JOB_PATH_MAX_BYTES {
            return Err(SyscallError::INVALID_PATH);
        }
        path_buffer[..prefix.len()].copy_from_slice(prefix);
        path_buffer[prefix.len()..path_len].copy_from_slice(program.as_bytes());
        // SAFETY: an ASCII prefix followed by the bytes of a str
        let path = unsafe { core::str::from_utf8_unchecked(&path_buffer[..path_len]) };

        self.start_program(path, args)
    }

    /// Starts the program at path, args are whitespace separated
    pub fn start_program(&mut self, path: &str, args: &str) -> Result<u32, SyscallError> {
        debug_assert!(!self.is_running());

        let mut packed_args = [0u8; JOB_ARGS_MAX_BYTES];
        let mut packed_len = 0;
        let mut argc = 0;
        for word in args.split_ascii_whitespace() {
            let end = packed_len + word.len() + 1;
            if end > JOB_ARGS_MAX_BYTES || argc == MAX_ARGC {
                return Err(SyscallError::INVALID_ARGUMENT);
            }
            packed_args[packed_len..end - 1].copy_from_slice(word.as_bytes());
            packed_args[end - 1] = ARGUMENT_TERMINATOR;

            packed_len = end;
            argc += 1;
        }

        let reader = ChannelReader::create(STDOUT_CHANNEL_DATA_SIZE_BYTES)?;
        let spawned = sys_create_process(
            &ProcessCreateInfo::new(path, IGNORED_PRIORITY)
                .with_args(&packed_args[..packed_len], argc as u32)
                .with_flags(ProcessCreateFlags::INHERIT_PRIORITY)
                .with_stdout_channel(reader.channel_id()),
        );
        let pid = match spawned {
            Ok(pid) => pid,
            Err(error) => {
                let closed = reader.close();
                debug_assert!(closed.is_ok());
                return Err(error);
            }
        };

        *self = ShellJob::Running { reader, pid };
        Ok(pid)
    }

    /// Copies what the program wrote into out without blocking
    /// Can be called until NothingYet or Finished is returned to drain everything
    pub fn poll(&mut self, out: &mut [u8]) -> JobPoll {
        debug_assert!(!out.is_empty());

        let ShellJob::Running { reader, .. } = self else {
            return JobPoll::NoJob;
        };

        match reader.check_readable() {
            ReadReadiness::HasBytes => JobPoll::Output(reader.read_ring_available_bytes(out)),
            ReadReadiness::NothingYet => JobPoll::NothingYet,
            ReadReadiness::WriterDone => {
                let ShellJob::Running { reader, pid } = core::mem::replace(self, ShellJob::Idle)
                else {
                    unreachable!()
                };
                let closed = reader.close();
                debug_assert!(closed.is_ok());

                JobPoll::Finished { pid }
            }
        }
    }
}
