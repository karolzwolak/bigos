pub const HISTORY_CAPACITY: usize = 64;
pub const HISTORY_ENTRY_MAX_BYTES: usize = 256;

#[derive(Debug, Clone, Copy)]
pub struct HistoryEntry {
    pub bytes: [u8; HISTORY_ENTRY_MAX_BYTES],
    pub len: usize,
}

impl HistoryEntry {
    pub const EMPTY: Self = Self {
        bytes: [0; HISTORY_ENTRY_MAX_BYTES],
        len: 0,
    };

    pub fn as_bytes(&self) -> &[u8] {
        debug_assert!(self.len <= HISTORY_ENTRY_MAX_BYTES);
        &self.bytes[..self.len]
    }
}

pub struct CommandHistory {
    pub entries: [HistoryEntry; HISTORY_CAPACITY],
    pub next_slot: usize,
    pub count: usize,
    /// Index of the recalled entry, `count` while nothing is recalled
    pub browse_index: usize,
}

impl CommandHistory {
    pub const fn new() -> Self {
        Self {
            entries: [HistoryEntry::EMPTY; HISTORY_CAPACITY],
            next_slot: 0,
            count: 0,
            browse_index: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn entry_at(&self, index: usize) -> &HistoryEntry {
        debug_assert!(index < self.count);
        let slot = (self.next_slot + HISTORY_CAPACITY - self.count + index) % HISTORY_CAPACITY;

        &self.entries[slot]
    }

    /// Records an executed line and ends browsing
    pub fn push_line(&mut self, line: &[u8]) {
        self.browse_index = self.count;

        let is_recordable = !line.is_empty() && line.len() <= HISTORY_ENTRY_MAX_BYTES;
        if !is_recordable || (self.count != 0 && self.entry_at(self.count - 1).as_bytes() == line) {
            return;
        }

        let entry = &mut self.entries[self.next_slot];
        entry.bytes[..line.len()].copy_from_slice(line);
        entry.len = line.len();

        self.next_slot = (self.next_slot + 1) % HISTORY_CAPACITY;
        self.count = (self.count + 1).min(HISTORY_CAPACITY);
        self.browse_index = self.count;
    }

    /// Steps to an older entry, returns the line to show or None at the oldest one
    pub fn step_to_older(&mut self) -> Option<&[u8]> {
        debug_assert!(self.browse_index <= self.count);
        if self.browse_index == 0 {
            return None;
        }

        self.browse_index -= 1;
        Some(self.entry_at(self.browse_index).as_bytes())
    }

    /// Steps to a newer entry, past the newest one the line is empty again
    pub fn step_to_newer(&mut self) -> Option<&[u8]> {
        debug_assert!(self.browse_index <= self.count);
        if self.browse_index == self.count {
            return None;
        }

        self.browse_index += 1;
        if self.browse_index == self.count {
            return Some(&[]);
        }

        Some(self.entry_at(self.browse_index).as_bytes())
    }
}

impl Default for CommandHistory {
    fn default() -> Self {
        Self::new()
    }
}
