extern crate alloc;
use crate::memory::allocator::ALLOCATOR;
use crate::serial_println;
use alloc::alloc::{GlobalAlloc, Layout};
use core::fmt;
use core::ops::{Index, IndexMut};
use core::ptr::{self, NonNull};

pub struct Dequeue<T> {
    head: usize,
    tail: usize,
    size: usize,
    capacity: usize,
    data: NonNull<T>,
}

impl<T> Dequeue<T> {
    pub fn new() -> Self {
        Dequeue::with_capacity(4)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = if capacity.is_power_of_two() {
            capacity
        } else {
            capacity.next_power_of_two()
        };
        assert!(capacity > 0);

        let layout = Layout::array::<T>(capacity).expect("Cant create layout");
        let ptr = unsafe { ALLOCATOR.alloc(layout).cast::<T>() };
        if ptr.is_null() {
            serial_println!(
                "ERROR: Cant create dequeue - alloc of size {} failed",
                layout.size()
            );
            panic!();
        }

        let data = unsafe { NonNull::new_unchecked(ptr) };

        Self {
            head: 0,
            tail: 0,
            size: 0,
            capacity,
            data,
        }
    }

    fn expand(&mut self) {
        let new_capacity = self.capacity * 2;
        if super::DATA_STRUCTURE_DEBUG_PRINT {
            serial_println!("Dequeue: expand() to {}", new_capacity);
        }

        let layout = Layout::array::<T>(new_capacity).expect("Cant create layout");
        let new_data_ptr = unsafe { ALLOCATOR.alloc(layout).cast::<T>() };
        if new_data_ptr.is_null() {
            serial_println!(
                "ERROR: Cant expand dequeue - alloc of size {} failed",
                layout.size()
            );
            panic!();
        }

        unsafe {
            for i in 0..self.size {
                let index = self.wrap_index(self.head + i);
                ptr::copy_nonoverlapping(self.data.as_ptr().add(index), new_data_ptr.add(i), 1);
            }
            let old_layout = Layout::array::<T>(self.capacity).expect("Cant create layout");
            ALLOCATOR.dealloc(self.data.cast().as_ptr(), old_layout);
        }
        if super::DATA_STRUCTURE_DEBUG_PRINT {
            serial_println!("Dequeue: expanded");
        }

        self.head = 0;
        self.tail = self.size;
        self.capacity = new_capacity;
        self.data = unsafe { NonNull::new_unchecked(new_data_ptr) };
    }

    // wrap index to the beginning when it gets larger than capacity; capacity is always power of 2
    fn wrap_index(&self, index: usize) -> usize {
        index & (self.capacity - 1)
    }

    pub fn get_ptr_at(&self, index: usize) -> *mut T {
        debug_assert!(index < self.size);
        let wrapped_index = self.wrap_index(self.head + index);
        unsafe { self.data.as_ptr().add(wrapped_index) }
    }

    pub fn push_back(&mut self, value: T) {
        if self.size == self.capacity {
            self.expand();
        }

        unsafe {
            ptr::write(self.data.as_ptr().add(self.tail), value);
        }

        self.tail = self.wrap_index(self.tail + 1);
        self.size += 1;
    }

    pub fn push_front(&mut self, value: T) {
        if self.size == self.capacity {
            self.expand();
        }

        self.head = self.wrap_index(self.head.wrapping_sub(1));

        unsafe {
            ptr::write(self.data.as_ptr().add(self.head), value);
        }

        self.size += 1;
        if super::DATA_STRUCTURE_DEBUG_PRINT {
            serial_println!("push_front: New head: {}, size: {}", self.head, self.size);
        }
    }

    pub fn pop_back(&mut self) -> T {
        assert!(self.size > 0);

        self.tail = self.wrap_index(self.tail.wrapping_sub(1));

        let value = unsafe { ptr::read(self.data.as_ptr().add(self.tail)) };
        self.size -= 1;
        if super::DATA_STRUCTURE_DEBUG_PRINT {
            serial_println!("pop_back: New tail: {}, size: {}", self.tail, self.size);
        }

        value
    }

    pub fn pop_front(&mut self) -> T {
        assert!(self.size > 0);

        let value = unsafe { ptr::read(self.data.as_ptr().add(self.head)) };
        self.head = self.wrap_index(self.head + 1);
        self.size -= 1;
        if super::DATA_STRUCTURE_DEBUG_PRINT {
            serial_println!("pop_front: New head: {}, size: {}", self.head, self.size);
        }

        value
    }

    pub fn front(&self) -> &T {
        assert!(self.size > 0);
        unsafe { &*self.data.as_ptr().add(self.head) }
    }

    pub fn front_mut(&mut self) -> &mut T {
        assert!(self.size > 0);
        unsafe { &mut *self.data.as_ptr().add(self.head) }
    }

    pub fn back(&self) -> &T {
        assert!(self.size > 0);
        let idx = self.wrap_index(self.tail.wrapping_sub(1));
        unsafe { &*self.data.as_ptr().add(idx) }
    }

    pub fn back_mut(&mut self) -> &mut T {
        assert!(self.size > 0);
        let idx = self.wrap_index(self.tail.wrapping_sub(1));
        unsafe { &mut *self.data.as_ptr().add(idx) }
    }

    pub fn get(&self, index: usize) -> &T {
        assert!(index < self.size);
        unsafe { &*self.get_ptr_at(index) }
    }

    pub fn get_mut(&mut self, index: usize) -> &mut T {
        assert!(index < self.size);
        unsafe { &mut *self.get_ptr_at(index) }
    }

    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn clear(&mut self) {
        for i in 0..self.size {
            unsafe {
                ptr::drop_in_place(self.get_ptr_at(i));
            }
        }
        self.head = 0;
        self.tail = 0;
        self.size = 0;
    }

    pub fn iter(&self) -> Iter<'_, T> {
        Iter { dq: self, index: 0 }
    }

    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        IterMut { dq: self, index: 0 }
    }

    // iterate and consume
    pub fn drain(&mut self) -> Drain<'_, T> {
        Drain { dq: self, index: 0 }
    }
}

pub struct Iter<'a, T> {
    dq: &'a Dequeue<T>,
    index: usize,
}

impl<'a, T: 'a> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.dq.len() {
            let item = self.dq.get(self.index);
            self.index += 1;
            Some(item)
        } else {
            None
        }
    }
}

pub struct IterMut<'a, T> {
    dq: &'a mut Dequeue<T>,
    index: usize,
}

impl<'a, T: 'a> Iterator for IterMut<'a, T> {
    type Item = &'a mut T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.dq.len() {
            let item = unsafe { &mut *self.dq.get_ptr_at(self.index) };
            self.index += 1;
            Some(item)
        } else {
            None
        }
    }
}

pub struct Drain<'a, T> {
    dq: &'a mut Dequeue<T>,
    index: usize,
}

impl<'a, T> Iterator for Drain<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.dq.len() {
            let item = unsafe { ptr::read(self.dq.get_ptr_at(self.index)) };
            self.index += 1;
            Some(item)
        } else {
            None
        }
    }
}

impl<'a, T> Drop for Drain<'a, T> {
    fn drop(&mut self) {
        // remove the drained elements
        for _ in 0..self.index {
            self.dq.pop_front();
        }
    }
}

impl<T> Index<usize> for Dequeue<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index)
    }
}

impl<T> IndexMut<usize> for Dequeue<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.get_mut(index)
    }
}

impl<T> Drop for Dequeue<T> {
    fn drop(&mut self) {
        self.clear();
        if self.capacity > 0 {
            unsafe {
                let layout = Layout::array::<T>(self.capacity).expect("Cant create layout");
                ALLOCATOR.dealloc(self.data.cast().as_ptr(), layout);
            }
        }
    }
}

unsafe impl<T: Send> Send for Dequeue<T> {}
unsafe impl<T: Sync> Sync for Dequeue<T> {}

impl<T> Default for Dequeue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Display> fmt::Display for Dequeue<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[")?;
        for (i, val) in self.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", val)?;
        }
        write!(f, "]")
    }
}
