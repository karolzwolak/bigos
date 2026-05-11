#include "libc/syscall.h"

extern int main(int argc, char **argv);

void _start(void) {
    int argc = 0;
    char **argv = (void*)0;
    
    int ret = main(argc, argv);
    
    sys_exit(ret);
    
    while(1);
}

void __stack_chk_fail(void) {
    sys_exit(1);
}