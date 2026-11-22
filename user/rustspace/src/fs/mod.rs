pub mod dir_listing;
pub mod path;
pub mod time;

use crate::{sys_list_dir, sys_stat_file, DirEntryFlat, FileAttributes, StatFlat, SyscallError};
use path::FsPath;

/// Directory entries fetched per ListDir call
pub const LIST_PAGE_ENTRIES_COUNT: usize = 16;

pub fn stat_path(path: &FsPath) -> Result<StatFlat, SyscallError> {
    let mut stat = StatFlat::zeroed();
    if path.is_root() {
        stat.is_dir = 1;
        stat.attributes = FileAttributes::DIR_DEFAULT.bits();
        return Ok(stat);
    }

    sys_stat_file(path.as_str(), &mut stat)?;

    Ok(stat)
}

pub const fn is_text_byte(byte: u8) -> bool {
    matches!(byte, b' '..=b'~' | b'\n' | b'\t' | b'\r')
}

pub fn do_for_each_dir_entry(
    path: &FsPath,
    mut visit: impl FnMut(&DirEntryFlat),
) -> Result<(), SyscallError> {
    let mut entries = [DirEntryFlat::zeroed(); LIST_PAGE_ENTRIES_COUNT];
    let mut start_index = 0;

    loop {
        let listed = sys_list_dir(path.as_str(), start_index, &mut entries)?;
        for entry in &entries[..listed] {
            visit(entry);
        }

        if listed < LIST_PAGE_ENTRIES_COUNT {
            return Ok(());
        }
        start_index += listed;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EntryMode {
    pub is_dir: bool,
    pub attributes: FileAttributes,
}

impl EntryMode {
    pub const TEXT_LEN: usize = 4;

    pub const fn new(is_dir: bool, attributes: FileAttributes) -> Self {
        Self { is_dir, attributes }
    }

    pub const fn chars(self) -> [u8; Self::TEXT_LEN] {
        const fn flag_char(attributes: FileAttributes, flag: FileAttributes, set: u8) -> u8 {
            if attributes.contains(flag) {
                set
            } else {
                b'-'
            }
        }

        [
            if self.is_dir { b'd' } else { b'-' },
            flag_char(self.attributes, FileAttributes::READ, b'r'),
            flag_char(self.attributes, FileAttributes::WRITE, b'w'),
            flag_char(self.attributes, FileAttributes::EXECUTE, b'x'),
        ]
    }
}

impl core::fmt::Display for EntryMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let chars = self.chars();

        // SAFETY: only ASCII letters and '-'
        f.write_str(unsafe { core::str::from_utf8_unchecked(&chars) })
    }
}
