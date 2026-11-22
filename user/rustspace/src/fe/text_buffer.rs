use crate::SyscallError;
use alloc::vec::Vec;
use core::cmp::max;

pub const GAP_MIN_GROWTH_BYTES: usize = 4 * 1024;

const NEWLINE: u8 = b'\n';

/// Gap buffer: the text is `bytes[..gap_start]` and `bytes[gap_end..]`,
/// the bytes in between are free space, where we insert and remove text.
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

    pub const fn is_gap_empty(&self) -> bool {
        self.gap_start == self.gap_end
    }

    pub fn prepare_file_load_area(
        &mut self,
        file_size_bytes: usize,
    ) -> Result<&mut [u8], SyscallError> {
        let area_size = file_size_bytes
            .checked_add(GAP_MIN_GROWTH_BYTES)
            .ok_or(SyscallError::OUT_OF_MEMORY)?;
        if area_size > self.capacity() {
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

    pub fn finish_file_load(&mut self, len: usize) {
        debug_assert!(len <= self.capacity());

        self.gap_start = len;
        self.gap_end = self.capacity();
    }

    pub fn text_before_and_after_gap(&self) -> (&[u8], &[u8]) {
        (&self.bytes[..self.gap_start], &self.bytes[self.gap_end..])
    }

    #[inline(always)]
    pub fn byte_at_position(&self, position: usize) -> u8 {
        debug_assert!(position < self.len());

        let index = if position < self.gap_start {
            position
        } else {
            position + (self.gap_end - self.gap_start)
        };

        self.bytes[index]
    }

    pub fn insert_byte_at_position(&mut self, position: usize, byte: u8) -> bool {
        debug_assert!(position <= self.len());
        if self.is_gap_empty() && !self.grow_gap() {
            return false;
        }

        self.move_gap_to_position(position);
        self.bytes[self.gap_start] = byte;
        self.gap_start += 1;

        true
    }

    pub fn remove_byte_at_position(&mut self, position: usize) {
        debug_assert!(position < self.len());

        self.move_gap_to_position(position);
        self.gap_end += 1;
    }

    pub fn line_start_at_position(&self, position: usize) -> usize {
        debug_assert!(position <= self.len());

        let mut start = position;
        while start > 0 && self.byte_at_position(start - 1) != NEWLINE {
            start -= 1;
        }

        start
    }

    pub fn line_end_at_position(&self, position: usize) -> usize {
        debug_assert!(position <= self.len());

        let len = self.len();
        let mut end = position;
        while end < len && self.byte_at_position(end) != NEWLINE {
            end += 1;
        }

        end
    }

    /// Doubles the storage and moves the text after the gap to the new end
    fn grow_gap(&mut self) -> bool {
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

    fn move_gap_to_position(&mut self, position: usize) {
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
