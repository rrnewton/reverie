/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <ucontext.h>
#include <unistd.h>

enum { PRE_READ = 781, POST_READ = 782, INJECT_MADVISE = 783 };
static volatile sig_atomic_t private_returns;

static void private_return(int signal, siginfo_t *info, void *context) {
    ucontext_t *frame = context;
    /* This is a non-birth syscall: RAX==0 is success, never a clone child. */
    if (signal != SIGILL || info->si_signo != SIGILL
        || info->si_code != ILL_ILLOPN || private_returns
        || (uintptr_t)info->si_addr != 0x71000002
        || (uintptr_t)frame->uc_mcontext.gregs[REG_RIP] != 0x71000002
        || frame->uc_mcontext.gregs[REG_RAX] != 0) _exit(81);
    uintptr_t *stack = (void *)frame->uc_mcontext.gregs[REG_RSP];
    frame->uc_mcontext.gregs[REG_RIP] = *stack;
    frame->uc_mcontext.gregs[REG_RSP] += sizeof(uintptr_t);
    ++private_returns;
}

static long private_madvise(char *page) {
    register long result __asm__("rax") = SYS_madvise;
    register long arg3 __asm__("r10") = 0;
    register long arg4 __asm__("r8") = 0;
    register long arg5 __asm__("r9") = 0;
    __asm__ volatile("call *%[stub]" : "+a"(result)
        : [stub]"r"((void *)0x71000000), "D"(page), "S"(4096L),
          "d"((long)MADV_NORMAL), "r"(arg3), "r"(arg4), "r"(arg5)
        : "rcx", "r11", "memory");
    return result;
}

static int ordinary_ioctl_case(int mode) {
    int pipefd[2] = {-1, -1};
    char *page = mmap(0, 4096, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) return 92;
    memcpy(page, "ABCD", 4);
    int status = 0;
    long result = -2;
    int actual_errno = 0;
    long read_before = -1, write_before = -1, read_after = -1, write_after = -1;
    unsigned long request = mode == 4 ? FIONREAD : FIONBIO;
    int argument = mode == 4 ? 123456 : 1;
    if (syscall(SYS_pipe2, pipefd, 0)) { status = 93; goto cleanup; }
    read_before = syscall(SYS_fcntl, pipefd[0], F_GETFL);
    write_before = syscall(SYS_fcntl, pipefd[1], F_GETFL);
    if (read_before < 0 || write_before < 0
        || (read_before & (O_ACCMODE | O_NONBLOCK)) != O_RDONLY
        || (write_before & (O_ACCMODE | O_NONBLOCK)) != O_WRONLY) {
        status = 94; goto cleanup;
    }
    // Pipe creation/flag reads precede the real valid source read. Neither
    // ioctl is Tool-subscribed, injected, mocked, or retried.
    if (syscall(SYS_write, PRE_READ, page, 4) != 4) { status = 95; goto cleanup; }
    errno = 0;
    result = syscall(SYS_ioctl, mode == 4 ? -1 : pipefd[0], request, &argument);
    actual_errno = errno; // capture before any subsequent syscall/diagnostic
    read_after = syscall(SYS_fcntl, pipefd[0], F_GETFL);
    write_after = syscall(SYS_fcntl, pipefd[1], F_GETFL);
    if ((mode == 4 ? result != -1 || actual_errno != EBADF
                   : result != 0 || actual_errno != 0)
        || argument != (mode == 4 ? 123456 : 1)
        || read_after != (mode == 4 ? read_before : read_before | O_NONBLOCK)
        || write_after != write_before || memcmp(page, "ABCD", 4)) {
        status = 96; goto cleanup;
    }
    if (syscall(SYS_write, POST_READ, page, 4) != 4) { status = 97; goto cleanup; }
    if (memcmp(page, "ABCD", 4)) status = 98;
cleanup:;
    // Attempt both owned closes even if an earlier premise failed. A failure
    // remains a failure; process exit is not credited as these explicit closes.
    int closed_read = pipefd[0] >= 0 ? close(pipefd[0]) : -1;
    int closed_write = pipefd[1] >= 0 ? close(pipefd[1]) : -1;
    if (closed_read || closed_write) status = status ? status : 99;
    if (munmap(page, 4096)) status = status ? status : 100;
    char line[256];
    int length = snprintf(line, sizeof(line),
        "OBSERVATION_IOCTL mode=%d native_result=%ld errno=%d request=%lu argument=%d read_flags_delta=%ld write_flags_delta=%ld bytes=ABCD closes=%d,%d status=%d\n",
        mode, result, actual_errno, request, argument,
        read_before ^ read_after, write_before ^ write_after,
        closed_read, closed_write, status);
    if (length < 0 || (size_t)length >= sizeof(line)
        || syscall(SYS_write, STDOUT_FILENO, line, (size_t)length) != length) return 101;
    return status;
}

int main(int argc, char **argv) {
    if (argc != 2 || strlen(argv[1]) != 1 || argv[1][0] < '0'
        || argv[1][0] > '5') return 82;
    int mode = argv[1][0] - '0';
    if (mode >= 4) return ordinary_ioctl_case(mode);
    char *page = mmap(0, 4096, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) return 83;
    memcpy(page, "ABCD", 4);
    if (mode == 1) {
        struct sigaction action = { .sa_sigaction = private_return, .sa_flags = SA_SIGINFO };
        if (sigemptyset(&action.sa_mask) || sigaction(SIGILL, &action, 0)) return 84;
    }
    if (syscall(SYS_write, PRE_READ, page, 4) != 4) return 85;
    errno = 0;
    long result;
    if (mode == 0) result = syscall(SYS_madvise, page, 4096, MADV_NORMAL);
    else if (mode == 1) result = private_madvise(page);
    else if (mode == 2) result = syscall(SYS_write, INJECT_MADVISE, page, 4096);
    else result = syscall(SYS_getpid); /* actual positive PID, not a synthetic zero */
    int actual_errno = errno;
    if ((mode == 3 ? result <= 0 : result != 0) || actual_errno != 0) return 86;
    if (memcmp(page, "ABCD", 4) || private_returns != (mode == 1)) return 87;
    if (syscall(SYS_write, POST_READ, page, 4) != 4) return 88;
    if (memcmp(page, "ABCD", 4)) return 89;
    char line[192];
    int length = snprintf(line, sizeof(line),
        "OBSERVATION_EQ mode=%d native_result=%ld errno=%d bytes=ABCD private_returns=%d\n",
        mode, result, actual_errno, (int)private_returns);
    if (length < 0 || (size_t)length >= sizeof(line)
        || syscall(SYS_write, STDOUT_FILENO, line, (size_t)length) != length) return 90;
    if (munmap(page, 4096)) return 91;
    return 0;
}
