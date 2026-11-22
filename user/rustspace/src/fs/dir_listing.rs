use super::do_for_each_dir_entry;
use super::path::{FsPath, PARENT_DIR_NAME};
use crate::{DirEntryFlat, FS_NAME_LEN, FileAttributes, SyscallError};
use alloc::vec::Vec;
use core::cmp::Ordering;

const PARENT_ENTRY: DirEntryFlat = {
    let mut entry = DirEntryFlat::zeroed();
    let name = PARENT_DIR_NAME.as_bytes();
    entry.name[0] = name[0];
    entry.name[1] = name[1];
    entry.name_len = name.len() as u8;
    entry.is_dir = 1;
    entry.attributes = FileAttributes::DIR_DEFAULT.bits();

    entry
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentEntry {
    /// a ".." entry first, except in the root
    Include,
    Skip,
}

pub struct DirListing {
    pub entries: Vec<DirEntryFlat>,
}

impl DirListing {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// dirs then files, ".." first; On an error the listing is left empty
    pub fn load(&mut self, path: &FsPath, parent_entry: ParentEntry) -> Result<(), SyscallError> {
        self.entries.clear();
        if parent_entry == ParentEntry::Include && !path.is_root() {
            self.entries.push(PARENT_ENTRY);
        }
        let sorted_start = self.entries.len();

        let entries = &mut self.entries;
        let result = do_for_each_dir_entry(path, |entry| {
            debug_assert!(entry.name_len as usize <= FS_NAME_LEN);

            entries.push(*entry);
        });
        if let Err(error) = result {
            self.entries.clear();
            return Err(error);
        }

        self.entries[sorted_start..].sort_unstable_by(compare_entries);

        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.name_str().eq_ignore_ascii_case(name))
    }
}

impl Default for DirListing {
    fn default() -> Self {
        Self::new()
    }
}

fn compare_entries(left: &DirEntryFlat, right: &DirEntryFlat) -> Ordering {
    right.is_dir.cmp(&left.is_dir).then_with(|| {
        let left_name = left.name[..left.name_len as usize].iter();
        let right_name = right.name[..right.name_len as usize].iter();

        left_name
            .map(u8::to_ascii_uppercase)
            .cmp(right_name.map(u8::to_ascii_uppercase))
    })
}
