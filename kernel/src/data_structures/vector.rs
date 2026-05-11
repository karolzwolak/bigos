extern crate alloc;
use crate::memory::allocator::ALLOCATOR;
use crate::serial_println;
use alloc::alloc::{GlobalAlloc, Layout};
use core::ops::{Deref, DerefMut, Index, IndexMut};
use core::ptr::{self, NonNull};
use core::slice;

pub struct Vec<T> {
    pub size: usize,
    pub capacity: usize,
    pub data: NonNull<T>,
}

impl<T> Default for Vec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Vec<T> {
    pub fn new() -> Self {
        Vec::with_capacity(4)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "Capacity must be greater than 0");
        let layout = Layout::array::<T>(capacity).expect("Layout creation failed");
        let ptr = unsafe { ALLOCATOR.alloc(layout).cast::<T>() };

        if ptr.is_null() {
            panic!("Allocation failed");
        }

        let data = unsafe { NonNull::new_unchecked(ptr.cast::<T>()) };

        Self {
            size: 0,
            capacity,
            data,
        }
    }

    pub fn get(&self, index: usize) -> &T {
        assert!(index < self.size, "Index out of bounds");
        unsafe { &*self.data.as_ptr().add(index) }
    }

    pub fn get_mut(&mut self, index: usize) -> &mut T {
        assert!(index < self.size, "Index out of bounds");
        unsafe { &mut *self.data.as_ptr().add(index) }
    }

    pub fn resize(&mut self, new_capacity: usize) {
        if new_capacity <= self.capacity {
            return;
        }
        let old_capacity = self.capacity;

        if self.capacity == 0 {
            self.capacity = 1;
        }
        while self.capacity < new_capacity {
            self.capacity *= 2;
        }

        let new_layout = Layout::array::<T>(new_capacity).expect("Layout creation failed");
        let new_ptr = unsafe { ALLOCATOR.alloc(new_layout).cast::<T>() };

        unsafe {
            ptr::copy_nonoverlapping(self.data.as_ptr(), new_ptr, self.size);
            let old_layout = Layout::array::<T>(old_capacity).expect("Layout creation failed");
            ALLOCATOR.dealloc(self.data.cast().as_ptr(), old_layout);
        }

        self.data = unsafe { NonNull::new_unchecked(new_ptr) };
        self.capacity = new_capacity;
    }

    pub fn push(&mut self, value: T) {
        if self.size == self.capacity {
            self.resize(self.capacity * 2);
        }
        unsafe {
            ptr::write(self.data.as_ptr().add(self.size), value);
        }
        self.size += 1;

        serial_println!(
            "Vec push: size={}, capacity={} ptr={:#x}",
            self.size,
            self.capacity,
            self.data.as_ptr() as usize
        );
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.size != 0 {
            self.size -= 1;
            unsafe { Some(ptr::read(self.data.as_ptr().add(self.size))) }
        } else {
            None
        }
    }

    pub fn insert(&mut self, index: usize, item: T) {
        assert!(index <= self.size, "Index is out of bounds");

        if self.size == self.capacity {
            self.resize(self.capacity * 2);
        }

        unsafe {
            let ptr = self.data.as_ptr().add(index);
            ptr::copy(ptr, ptr.add(1), self.size - index);
            ptr::write(ptr, item);
        }

        self.size += 1;
    }

    pub fn remove(&mut self, index: usize) -> T {
        assert!(index < self.size, "Index is out of bounds");

        unsafe {
            let ptr = self.data.as_ptr().add(index);
            let item = ptr::read(ptr);
            ptr::copy(ptr.add(1), ptr, self.size - index - 1);
            self.size -= 1;

            item
        }
    }

    pub fn clear(&mut self) {
        unsafe {
            ptr::drop_in_place(self.as_mut_slice());
        }
        self.size = 0;
    }

    pub fn as_slice(&self) -> &[T] {
        if self.size == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(self.data.as_ptr(), self.size) }
        }
    }

    pub fn as_mut_slice(&mut self) -> &mut [T] {
        if self.size == 0 {
            &mut []
        } else {
            unsafe { slice::from_raw_parts_mut(self.data.as_ptr(), self.size) }
        }
    }
}

impl<T> Deref for Vec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T> DerefMut for Vec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<T> Index<usize> for Vec<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index)
    }
}

impl<T> IndexMut<usize> for Vec<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.get_mut(index)
    }
}

impl<T> FromIterator<T> for Vec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut vec = Vec::new();
        for item in iter {
            vec.push(item);
        }
        vec
    }
}

impl<T> Drop for Vec<T> {
    fn drop(&mut self) {
        unsafe {
            ptr::drop_in_place(self.as_mut_slice());
            if self.capacity > 0 {
                let layout = Layout::array::<T>(self.capacity).expect("Layout creation failed");
                ALLOCATOR.dealloc(self.data.cast().as_ptr(), layout);
            }
        }
    }
}

impl<T> Clone for Vec<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        let mut new_vec = Vec::with_capacity(self.capacity);
        for item in self.as_slice() {
            new_vec.push(item.clone());
        }
        new_vec
    }
}
