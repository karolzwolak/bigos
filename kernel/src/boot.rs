use crate::main;

#[unsafe(no_mangle)]
unsafe extern "C" fn kmain() -> ! {
    unsafe { kernel::boot_common::bsp_early_init() };
    main()
}
