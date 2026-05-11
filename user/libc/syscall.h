#ifndef SYSCALL_H
#define SYSCALL_H

#include "types.h"

#define SYS_CREATE_PROCESS 0
#define SYS_TERMINATE_PROCESS 1
#define SYS_WRITE 2
#define SYS_READ 3
#define SYS_GET_LINE 4
#define SYS_ALLOCATE 5
#define SYS_CREATE_FILE 6
#define SYS_REMOVE_FILE 7
#define SYS_LOAD_FILE 8
#define SYS_UNLOAD_FILE 9
#define SYS_CREATE_WINDOW 10
#define SYS_GET_PROCESS_INFO 11

#define SYS_EXIT 999

static inline long syscall6(
    long num,
    long arg1,
    long arg2,
    long arg3,
    long arg4,
    long arg5,
    long arg6
) {
    long ret;
    __asm__ volatile (
        "mov %5, %%r10\n"
        "mov %6, %%r8\n"
        "mov %7, %%r9\n"
        "syscall"
        : "=a"(ret)
        : "a"(num),
        "D"(arg1),
        "S"(arg2),
        "d"(arg3),
        "r"(arg4),
        "r"(arg5),
        "r"(arg6)
        : "rcx", "r11", "r10", "r8", "r9", "memory"
    );
    return ret;
}

static inline long syscall3(long num, long arg1, long arg2, long arg3) {
    return syscall6(num, arg1, arg2, arg3, 0, 0, 0);
}

static inline long syscall1(long num, long arg1) {
    return syscall6(num, arg1, 0, 0, 0, 0, 0);
}

static inline void sys_exit(int code) {
    syscall1(SYS_EXIT, code);
    __builtin_unreachable();
}

static inline long sys_write(int fd, const void *buf, size_t count) {
    return syscall3(SYS_WRITE, fd, (long)buf, count);
}

static inline long sys_read(int fd, void *buf, size_t count) {
    return syscall3(SYS_READ, fd, (long)buf, count);
}

static inline void* sys_allocate(size_t size) {
    return (void*)syscall1(SYS_ALLOCATE, size);
}

#endif