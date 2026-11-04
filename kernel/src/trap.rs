/// Register state of an interrupted ring 3 context, lowest address first
/// The general registers are pushed by PUSH_ALL_GPRS and the rest by the CPU
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,

    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

pub const TRAP_FRAME_SIZE: usize = core::mem::size_of::<TrapFrame>();

// the stack stays 16 byte aligned across the pushes of a ring 3 interrupt
const _: () = assert!(TRAP_FRAME_SIZE % 16 == 0);

impl TrapFrame {
    pub const fn zeroed() -> Self {
        Self {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            r11: 0,
            r10: 0,
            r9: 0,
            r8: 0,
            rdi: 0,
            rsi: 0,
            rbp: 0,
            rbx: 0,
            rdx: 0,
            rcx: 0,
            rax: 0,
            rip: 0,
            cs: 0,
            rflags: 0,
            rsp: 0,
            ss: 0,
        }
    }
}

macro_rules! push_all_gprs {
    () => {
        concat!(
            "push rax\n",
            "push rcx\n",
            "push rdx\n",
            "push rbx\n",
            "push rbp\n",
            "push rsi\n",
            "push rdi\n",
            "push r8\n",
            "push r9\n",
            "push r10\n",
            "push r11\n",
            "push r12\n",
            "push r13\n",
            "push r14\n",
            "push r15\n",
        )
    };
}

macro_rules! pop_all_gprs {
    () => {
        concat!(
            "pop r15\n",
            "pop r14\n",
            "pop r13\n",
            "pop r12\n",
            "pop r11\n",
            "pop r10\n",
            "pop r9\n",
            "pop r8\n",
            "pop rdi\n",
            "pop rsi\n",
            "pop rbp\n",
            "pop rbx\n",
            "pop rdx\n",
            "pop rcx\n",
            "pop rax\n",
        )
    };
}

pub(crate) use pop_all_gprs;
pub(crate) use push_all_gprs;

const RPL_MASK: u64 = 0b11;
const RPL_USER: u64 = 3;

/// Runs after the Rust handler of every interrupt stub, before the registers are popped
/// Interrupted ring 0 code just continues, the kernel is not preemptible
/// A ring 3 return goes through exit_to_user so a reschedule request or a kill takes effect
pub extern "C" fn interrupt_return_path(frame: &mut TrapFrame) {
    if frame.cs & RPL_MASK != RPL_USER {
        return;
    }

    // after a sysretq the SS the CPU pushes for a ring 3 interrupt reads 0x18, RPL 0 (seen
    // under KVM with qemu64), and iretq then faults with #GP because SS.RPL != CS.RPL
    // so every return to ring 3 gets the user selector back
    frame.ss = crate::gdt::USER_DATA_SELECTOR_VALUE as u64;

    crate::process::scheduler::exit_to_user();
}

pub fn is_user_frame(frame: &TrapFrame) -> bool {
    frame.cs & RPL_MASK == RPL_USER
}

/// Defines a naked interrupt entry: pushes every general register, calls
/// `handler(&mut TrapFrame)`, runs the common return path and returns with iretq
macro_rules! interrupt_stub {
    ($stub_name:ident, $handler:path) => {
        #[unsafe(naked)]
        pub unsafe extern "C" fn $stub_name() {
            core::arch::naked_asm!(
                $crate::trap::push_all_gprs!(),
                "mov rdi, rsp",
                "call {handler}",
                "mov rdi, rsp",
                "call {return_path}",
                $crate::trap::pop_all_gprs!(),
                "iretq",
                handler = sym $handler,
                return_path = sym $crate::trap::interrupt_return_path,
            )
        }
    };
}

pub(crate) use interrupt_stub;
