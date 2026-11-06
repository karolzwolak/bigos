use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::{Config, Uart16550Tty, backend::PioBackend};

lazy_static! {
    /// COM1 (0x3F8) — kernel debug logs
    pub static ref SERIAL1: Mutex<Uart16550Tty<PioBackend>> = {
        let serial_port = unsafe { Uart16550Tty::new_port(0x3F8, Config::default()).unwrap() };
        Mutex::new(serial_port)
    };

    /// COM2 (0x2F8) — userspace program output (sys_write fd=1/fd=2)
    pub static ref SERIAL2: Mutex<Uart16550Tty<PioBackend>> = {
        let serial_port = unsafe { Uart16550Tty::new_port(0x2F8, Config::default()).unwrap() };
        Mutex::new(serial_port)
    };
}

#[doc(hidden)]
pub fn _print(args: ::core::fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    // disable interrupts to avoid SERIAL1 deadlocks
    interrupts::without_interrupts(|| {
        SERIAL1
            .lock()
            .write_fmt(args)
            .expect("Printing to serial failed");
    });
}

/// Write to COM2
/// Called from the sys_write syscall handler for fd=1 and fd=2
#[doc(hidden)]
pub fn _print2(args: ::core::fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        SERIAL2
            .lock()
            .write_fmt(args)
            .expect("Printing to serial2 failed");
    });
}

/// Prints to the host through the serial interface
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::io::serial::_print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! serial2_print {
    ($($arg:tt)*) => {
        $crate::io::serial::_print2(format_args!($($arg)*))
    };
}

/// Prints to the host through the serial interface, appending a newline
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(
        concat!($fmt, "\n"), $($arg)*));
}

/// Prints to the host through the serial interface with core ID and
/// microsecond timestamp prepended, appending a newline
#[macro_export]
macro_rules! serial_println_core {
    () => (
        $crate::serial_print!(
            "[Core {} | {}us] \n",
            $crate::util::cpuinfo::get_current_core_id(),
            $crate::interrupts::tsc_timestamp_us(),
        )
    );
    ($fmt:expr) => (
        $crate::serial_print!(
            concat!("[Core {} | {}us] ", $fmt, "\n"),
            $crate::util::cpuinfo::get_current_core_id(),
            $crate::interrupts::tsc_timestamp_us(),
        )
    );
    ($fmt:expr, $($arg:tt)*) => (
        $crate::serial_print!(
            concat!("[Core {} | {}us] ", $fmt, "\n"),
            $crate::util::cpuinfo::get_current_core_id(),
            $crate::interrupts::tsc_timestamp_us(),
            $($arg)*
        )
    );
}
