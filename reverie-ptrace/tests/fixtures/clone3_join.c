/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
#define _GNU_SOURCE
#include <errno.h>
#include <linux/sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

enum { PRE = 791, POST = 792 };

/* Only the original parent owns this PID. Error cleanup is not a test pass. */
static int reap_failed_child(pid_t child, int code) {
    int saved = errno, status;
    if (kill(child, SIGKILL) && errno != ESRCH) return 91;
    pid_t got;
    do { got = waitpid(child, &status, 0); } while (got < 0 && errno == EINTR);
    errno = saved;
    return got == child ? code : 92;
}

int main(int argc, char **argv) {
    if (argc != 2) return 70;
    int mode = atoi(argv[1]); /* 0: final EINVAL; 1: hidden child; 2: Getpid */
    if (mode < 0 || mode > 2) return 71;
    struct sigaction action = {.sa_handler = SIG_DFL}, actual;
    if (sigemptyset(&action.sa_mask) || sigaction(SIGCHLD, &action, NULL) ||
        sigaction(SIGCHLD, NULL, &actual) || actual.sa_handler != SIG_DFL ||
        (actual.sa_flags & SA_NOCLDWAIT)) return 72;
    sigset_t unblocked;
    if (sigemptyset(&unblocked) || sigaddset(&unblocked, SIGCHLD) ||
        sigprocmask(SIG_UNBLOCK, &unblocked, NULL)) return 73;
    int effect[2], release[2];
    if (pipe2(effect, 0) || pipe2(release, 0)) return 74;
    char *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) return 75;
    memcpy(page, "ABCD", 4);
    if (syscall(SYS_write, PRE, page, 4) != 4) return 76;
    struct clone_args args = {.flags = CLONE_UNTRACED, .exit_signal = SIGCHLD};
    errno = 0;
    long result = mode == 2 ? syscall(SYS_getpid)
                           : syscall(SYS_clone3, &args, mode == 0 ? 0 : sizeof(args));
    int call_errno = errno;
    if (mode == 1 && result == 0) {
        if (close(effect[0]) || close(release[1])) _exit(81);
        /* The actual child branch publishes zero, its PID, and one real effect. */
        struct { long result, pid; char byte; } message = {result, getpid(), 'X'};
        /* write is a Tool sentinel subscription. An untraced child's inherited
         * TRACE rule would return ENOSYS; writev is genuinely unsubscribed. */
        struct iovec record = {.iov_base = &message, .iov_len = sizeof(message)};
        if (syscall(SYS_writev, effect[1], &record, 1) != (long)sizeof(message)) _exit(82);
        char byte = 0;
        if (read(release[0], &byte, 1) != 1 || byte != 'R') _exit(83);
        if (close(effect[1]) || close(release[0])) _exit(84);
        _exit(0);
    }
    if (mode == 1 && result > 0) {
        pid_t child = (pid_t)result;
        if ((long)child != result || call_errno) return reap_failed_child(child, 85);
        if (close(effect[1]) || close(release[0])) return reap_failed_child(child, 86);
        struct { long result, pid; char byte; } message = {0};
        if (read(effect[0], &message, sizeof(message)) != (ssize_t)sizeof(message) ||
            message.result != 0 || message.pid != result || message.byte != 'X')
            return reap_failed_child(child, 87);
        /* waitpid(WNOHANG)==0 joins the live original child at the source probe. */
        int status = -1;
        if (waitpid(child, &status, WNOHANG) != 0)
            return reap_failed_child(child, 88);
        long probe = syscall(SYS_write, POST, page, 4);
        if (write(release[1], "R", 1) != 1) return reap_failed_child(child, 89);
        pid_t waited;
        do { waited = waitpid(child, &status, 0); } while (waited < 0 && errno == EINTR);
        char byte;
        ssize_t eof = read(effect[0], &byte, 1);
        int close_a = close(effect[0]), close_b = close(release[1]);
        int absent_status = 0;
        errno = 0;
        pid_t absent = waitpid(-1, &absent_status, WNOHANG);
        int absent_errno = errno;
        /* Semantic checks follow release, actual wait and pipe closure. */
        if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) || eof != 0 ||
            close_a || close_b || absent != -1 || absent_errno != ECHILD) return 93;
        if (probe != 4 || memcmp(page, "ABCD", 4)) return 94;
        printf("CLONE3_JOIN mode=1 result=%ld errno=%d child_result=%ld child_pid=%ld effect=X live=1 waited=%ld status=0 eof=1 pipes_closed=1 absent=ECHILD bytes=ABCD\n",
               result, call_errno, message.result, message.pid, (long)waited);
    } else {
        /* Includes unavailable/filtered clone3: retain the exact actual error. */
        long probe = syscall(SYS_write, POST, page, 4);
        int a = close(effect[0]), b = close(effect[1]);
        int c = close(release[0]), d = close(release[1]);
        if (a || b || c || d || probe != 4 || memcmp(page, "ABCD", 4)) return 95;
        int status = 0;
        errno = 0;
        pid_t absent = waitpid(-1, &status, WNOHANG);
        if (absent != -1 || errno != ECHILD) return 99;
        printf("CLONE3_JOIN mode=%d result=%ld errno=%d child=none pipes_closed=1 absent=ECHILD bytes=ABCD\n",
               mode, result, call_errno);
        /* No ENOSYS/EPERM skip or errno widening. */
        if (mode == 0 && (result != -1 || call_errno != EINVAL)) return 96;
        if (mode == 1 || (mode == 2 && (result != getpid() || call_errno))) return 97;
    }
    return munmap(page, 4096) ? 98 : 0;
}
