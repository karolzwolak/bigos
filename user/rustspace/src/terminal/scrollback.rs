use core::cmp::min;

use crate::CircularBufferInfo;

pub struct Scrollback {
    pub base: *mut u8,
    pub view_size: usize,
    pub total_size: usize,
    pub relative: usize,
    pub absolute_filled: u64,
}

impl Scrollback {
    pub fn new(info: CircularBufferInfo) -> Self {
        debug_assert!(info.virtual_base != 0);
        debug_assert!(info.view_size != 0);
        debug_assert!(info.total_virtual_size == info.view_size * 2);

        Self {
            base: info.virtual_base as *mut u8,
            view_size: info.view_size as usize,
            total_size: info.total_virtual_size as usize,
            relative: 0,
            absolute_filled: 0,
        }
    }

    pub const fn empty() -> Self {
        Self {
            base: core::ptr::null_mut(),
            view_size: 0,
            total_size: 0,
            relative: 0,
            absolute_filled: 0,
        }
    }

    pub fn is_valid(&self) -> bool {
        !self.base.is_null() && self.view_size != 0
    }

    #[inline]
    pub fn is_in_buffer(&self, abs_pos: u64) -> bool {
        if abs_pos >= self.absolute_filled {
            return false;
        }
        let backward_offset = self.absolute_filled - abs_pos;
        backward_offset <= self.view_size as u64
    }

    pub fn get_writable_slice(&mut self, max: usize) -> &mut [u8] {
        debug_assert!(self.relative < self.view_size);

        let available = self.view_size;
        let length = min(max, available);

        unsafe { core::slice::from_raw_parts_mut(self.base.add(self.relative), length) }
    }

    pub fn commit_bytes(&mut self, n: usize) {
        debug_assert!(n <= self.view_size);
        debug_assert!(self.relative < self.view_size);

        self.relative += n;
        self.absolute_filled += n as u64;
        if self.relative >= self.view_size {
            self.relative -= self.view_size;
        }
        debug_assert!(self.relative < self.view_size);
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) -> u64 {
        let start_abs = self.absolute_filled;
        let mut remaining = bytes.len();
        let mut offset = 0;

        while remaining > 0 {
            let chunk_max = remaining;
            let dest = self.get_writable_slice(chunk_max);
            let n = core::cmp::min(dest.len(), remaining);
            dest[..n].copy_from_slice(&bytes[offset..offset + n]);
            self.commit_bytes(n);

            offset += n;
            remaining -= n;
        }

        start_abs
    }

    pub fn read_at(&self, abs_pos: u64, max_len: usize) -> &[u8] {
        if !self.is_in_buffer(abs_pos) {
            return &[];
        }

        let backward_offset = (self.absolute_filled - abs_pos) as usize;
        let to_read_len = min(backward_offset, max_len);

        let offset_from_base: usize = {
            if backward_offset <= self.relative {
                self.relative - backward_offset
            } else {
                self.view_size + self.relative - backward_offset
            }
        };
        debug_assert!(offset_from_base < self.view_size);

        unsafe {
            let ptr = self.base.add(offset_from_base);
            core::slice::from_raw_parts(ptr, to_read_len)
        }
    }

    pub fn current_absolute(&self) -> u64 {
        self.absolute_filled
    }

    pub fn remove_last_byte(&mut self) {
        if self.absolute_filled == 0 {
            return;
        }
        self.absolute_filled -= 1;
        if self.relative == 0 {
            self.relative = self.view_size - 1;
        } else {
            self.relative -= 1;
        }
    }
}
