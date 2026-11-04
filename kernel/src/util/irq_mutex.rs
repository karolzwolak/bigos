use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
use spin::{Mutex, MutexGuard};
use x86_64::instructions::interrupts;

/// Spin lock that keeps interrupts off while it is held
/// Required for data an interrupt handler touches too, else a handler that
/// interrupts the holder would spin forever on its own core
pub struct IrqMutex<T> {
    inner: Mutex<T>,
}

pub struct IrqMutexGuard<'a, T> {
    guard: ManuallyDrop<MutexGuard<'a, T>>,
    interrupts_were_enabled: bool,
}

impl<T> IrqMutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }

    pub fn lock(&self) -> IrqMutexGuard<'_, T> {
        let interrupts_were_enabled = interrupts::are_enabled();
        interrupts::disable();

        IrqMutexGuard {
            guard: ManuallyDrop::new(self.inner.lock()),
            interrupts_were_enabled,
        }
    }

    pub fn try_lock(&self) -> Option<IrqMutexGuard<'_, T>> {
        let interrupts_were_enabled = interrupts::are_enabled();
        interrupts::disable();

        match self.inner.try_lock() {
            Some(guard) => Some(IrqMutexGuard {
                guard: ManuallyDrop::new(guard),
                interrupts_were_enabled,
            }),
            None => {
                if interrupts_were_enabled {
                    interrupts::enable();
                }

                None
            }
        }
    }
}

impl<T> Deref for IrqMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for IrqMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> Drop for IrqMutexGuard<'_, T> {
    fn drop(&mut self) {
        // unlock first, an interrupt that lands after the enable may want this lock
        // SAFETY: the guard is dropped exactly once, here
        unsafe { ManuallyDrop::drop(&mut self.guard) };

        if self.interrupts_were_enabled {
            interrupts::enable();
        }
    }
}
