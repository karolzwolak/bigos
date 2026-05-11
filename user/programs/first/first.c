#include "../../libc/syscall.h"

int main() {
    unsigned long ret = syscall6(997, 1, 2, 3, 4, 5, 6);
    char buffer[5] = {'m', 'o', 'f', 'u', '\n'};
    sys_write(1, &buffer, 5);
    sys_exit(123);
    return ret;
}
