/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
#define _GNU_SOURCE
#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <ucontext.h>
#include <unistd.h>

static volatile sig_atomic_t handled;
static pid_t sender;
static char bytes[6] = "ZZZZZZ";
static void caught(int signal, siginfo_t *info, void *context) {
    ucontext_t *frame = context;
    if (signal != SIGUSR1 || info->si_signo != SIGUSR1
        || info->si_code != SI_TKILL || info->si_pid != sender
        || info->si_uid != getuid() || handled
        || frame->uc_mcontext.gregs[REG_RAX] != -EINTR) _exit(101);
    ++handled;
    if (syscall(SYS_write, 879, 0, 0) != 1) _exit(102);
}
static int filter(unsigned int action, int deny_getpid) {
    struct sock_filter code[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K,
                 deny_getpid ? SYS_getpid : UINT32_MAX, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, action),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = { .len = sizeof(code) / sizeof(code[0]), .filter = code };
    return prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)
        || prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program);
}
int main(int argc, char **argv) {
    if (argc != 2) return 103;
    int mode = atoi(argv[1]);
    if (mode == 8) {
        long pid = syscall(SYS_getpid), tid = syscall(SYS_gettid);
        if (ptrace(PTRACE_TRACEME, 0, 0, 0)
            || syscall(SYS_tgkill, pid, tid, SIGSTOP)) return 104;
        (void)syscall(SYS_getpid); /* parent holds an ordinary PTRACE_SYSCALL ENTRY */
        return 105;
    }
    if (mode < 0 || mode > 6) return 106;
    sender = mode == 4 || mode == 5 ? getppid() : getpid();
    struct sigaction action = {0};
    action.sa_sigaction = caught; action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGUSR1, &action, 0)) return 107;
    int fds[2];
    if (pipe(fds) || write(fds[1], "ABCDEF", 6) != 6) return 108;
    unsigned int disposition = mode == 1 || mode == 3 || mode == 5
        ? SECCOMP_RET_TRAP : SECCOMP_RET_ERRNO | EACCES;
    if (filter(disposition, mode == 6)) return 109;
    if (syscall(SYS_write, 880, fds[0], mode) != 0) return 110;
    errno = 0;
    long result = syscall(SYS_read, fds[0], bytes, sizeof(bytes));
    if (mode == 0 || mode == 1) return 111; /* backend refusal cannot return here */
    if (mode == 6) {
        if (result != 0 || handled || memcmp(bytes, "ZZZZZZ", 6)) return 112;
    } else {
        if (result != -1 || errno != EINTR || handled != 1
            || memcmp(bytes, "ZZZZZZ", 6)) return 113;
    }
    if (read(fds[0], bytes, 6) != 6 || memcmp(bytes, "ABCDEF", 6)) return 114;
    if (close(fds[0]) || close(fds[1])) return 115;
    return 0;
}
