OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

SECTIONS
{
    /* User programs start at 4MB */
    . = 0x400000;
    
    .text : {
        *(.text*)
    }
    
    .rodata : {
        *(.rodata*)
    }
    
    .data : {
        *(.data*)
    }
    
    .bss : {
        *(COMMON)
        *(.bss*)
    }
    
    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note.gnu.build-id)
    }
}