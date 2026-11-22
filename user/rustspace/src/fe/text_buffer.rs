use crate::SyscallError;
use alloc::vec::Vec;
use core::cmp::max;

/// Smallest growth step, also the gap a freshly loaded file gets for typing
pub const GAP_MIN_GROWTH_BYTES: usize = 4 * 1024;

const NEWLINE: u8 = b'\n';

/// Heap text with the free space kept at the edit position,
/// typing moves no bytes, moving the edit position moves only the bytes in between
/// The whole Vec is storage, its len is the capacity and the gap is part of it
pub struct TextBuffer {
    pub bytes: Vec<u8>,
    pub gap_start: usize,
    pub gap_end: usize,
}

impl TextBuffer {
    pub const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            gap_start: 0,
            gap_end: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.bytes.len()
    }

    pub fn len(&self) -> usize {
        self.capacity() - (self.gap_end - self.gap_start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub const fn is_full(&self) -> bool {
        self.gap_start == self.gap_end
    }

    /// Storage for reading a file of `file_size_bytes` straight in, with a gap to type into
    /// behind it. `set_loaded_len` makes the read part the text
    /// A larger buffer from an earlier file is kept
    pub fn load_area_mut(&mut self, file_size_bytes: usize) -> Result<&mut [u8], SyscallError> {
        let area_size = file_size_bytes
            .checked_add(GAP_MIN_GROWTH_BYTES)
            .ok_or(SyscallError::OUT_OF_MEMORY)?;
        if area_size > self.capacity() {
            // a fresh Vec, growing the old one would copy text that is replaced anyway
            self.bytes = Vec::new();
            self.bytes
                .try_reserve_exact(area_size)
                .map_err(|_| SyscallError::OUT_OF_MEMORY)?;
            self.bytes.resize(area_size, 0);
        }
        self.gap_start = 0;
        self.gap_end = self.capacity();

        Ok(&mut self.bytes)
    }

    pub fn set_loaded_len(&mut self, len: usize) {
        debug_assert!(len <= self.capacity());

        self.gap_start = len;
        self.gap_end = self.capacity();
    }

    /// The text in order: what is before the gap, then what is after it
    pub fn segments(&self) -> (&[u8], &[u8]) {
        (&self.bytes[..self.gap_start], &self.bytes[self.gap_end..])
    }

    #[inline(always)]
    pub fn byte_at(&self, position: usize) -> u8 {
        debug_assert!(position < self.len());

        let index = if position < self.gap_start {
            position
        } else {
            position + (self.gap_end - self.gap_start)
        };

        self.bytes[index]
    }

    /// Returns false when the buffer is full and growing it failed
    pub fn insert(&mut self, position: usize, byte: u8) -> bool {
        debug_assert!(position <= self.len());
        if self.is_full() && !self.grow() {
            return false;
        }

        self.move_gap_to(position);
        self.bytes[self.gap_start] = byte;
        self.gap_start += 1;

        true
    }

    pub fn remove(&mut self, position: usize) {
        debug_assert!(position < self.len());

        self.move_gap_to(position);
        self.gap_end += 1;
    }

    /// Start of the line `position` is on
    pub fn line_start(&self, position: usize) -> usize {
        debug_assert!(position <= self.len());

        let mut start = position;
        while start > 0 && self.byte_at(start - 1) != NEWLINE {
            start -= 1;
        }

        start
    }

    /// Position of the newline ending the line `position` is on, or the text length
    pub fn line_end(&self, position: usize) -> usize {
        debug_assert!(position <= self.len());

        let len = self.len();
        let mut end = position;
        while end < len && self.byte_at(end) != NEWLINE {
            end += 1;
        }

        end
    }

    /// Doubles the storage and moves the text after the gap to the new end
    fn grow(&mut self) -> bool {
        let old_capacity = self.capacity();
        let growth = max(old_capacity, GAP_MIN_GROWTH_BYTES);
        if self.bytes.try_reserve_exact(growth).is_err() {
            return false;
        }
        self.bytes.resize(old_capacity + growth, 0);

        let after_gap_len = old_capacity - self.gap_end;
        let new_gap_end = self.capacity() - after_gap_len;
        self.bytes
            .copy_within(self.gap_end..old_capacity, new_gap_end);
        self.gap_end = new_gap_end;

        true
    }

    fn move_gap_to(&mut self, position: usize) {
        if position < self.gap_start {
            let moved = self.gap_start - position;
            self.bytes
                .copy_within(position..self.gap_start, self.gap_end - moved);
            self.gap_start = position;
            self.gap_end -= moved;
        } else if position > self.gap_start {
            let moved = position - self.gap_start;
            self.bytes
                .copy_within(self.gap_end..self.gap_end + moved, self.gap_start);
            self.gap_start += moved;
            self.gap_end += moved;
        }
    }
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new()
    }
}
