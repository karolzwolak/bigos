use crate::process::process::INVALID_PID;
use crate::{MAX_CORES, serial_println};
use limine::mp::MpInfo;
use spin::Mutex;

lazy_static::lazy_static! {
    pub static ref CORE_POOL: Mutex<CorePool> = Mutex::new(CorePool::new());
}
pub static mut TOTAL_CORE_COUNT: u8 = 1;

/// How many cores the machine has and which LAPIC each of them is
/// Core 0 is the boot core and runs no tasks
pub struct CorePool {
    total_cores: u8,
    lapic_ids: [u8; MAX_CORES as usize],
}

impl CorePool {
    pub fn new() -> Self {
        Self {
            total_cores: 1,
            lapic_ids: [0; MAX_CORES as usize],
        }
    }

    /// Called once during kernel initialization
    pub fn init_with_core_count(&mut self, core_count: u8, cpus: &[&MpInfo]) {
        if core_count == 0 {
            serial_println!("ERROR: Invalid core count: {}, using 1", core_count);
            return;
        }
        if core_count > MAX_CORES {
            serial_println!(
                "ERROR: Core count {} exceeds max supported {}, using {}",
                core_count,
                MAX_CORES,
                MAX_CORES
            );
            self.total_cores = MAX_CORES;
        }

        unsafe {
            TOTAL_CORE_COUNT = core_count;
        }

        self.total_cores = core_count;

        for i in 0..core_count {
            self.lapic_ids[i as usize] = cpus[i as usize].lapic_id as u8;
            crate::interrupts::set_lapic_id_of_core(i, cpus[i as usize].lapic_id as u8);
        }

        serial_println!("CorePool initialized with {} cores", core_count);
    }

    pub fn total_cores(&self) -> u8 {
        self.total_cores
    }

    pub fn lapic_id_of_core(&self, core_id: u8) -> u8 {
        debug_assert!(core_id < self.total_cores);

        self.lapic_ids[core_id as usize]
    }
}
