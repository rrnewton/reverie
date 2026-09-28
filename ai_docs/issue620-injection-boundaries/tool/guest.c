#define _GNU_SOURCE
#include <errno.h>
#include <inttypes.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>

#if !defined(__x86_64__)
#error "This diagnostic binds the x86_64 raw syscall ABI"
#endif
struct probe_memory {
    struct timespec a, scratch_a, b;
    uint64_t mask_a, mask_b, query;
};
_Static_assert(sizeof(struct timespec) == 16, "timespec ABI");
_Static_assert(offsetof(struct probe_memory, scratch_a) == 16, "scratch ABI");
_Static_assert(offsetof(struct probe_memory, b) == 32, "B timeout ABI");
_Static_assert(offsetof(struct probe_memory, mask_a) == 48, "A mask ABI");
_Static_assert(offsetof(struct probe_memory, mask_b) == 56, "B mask ABI");
_Static_assert(offsetof(struct probe_memory, query) == 64, "query ABI");
static volatile sig_atomic_t handler_calls, saved_usr1_blocked, active_usr1_blocked;
static long raw6(long nr, long a0, long a1, long a2, long a3, long a4, long a5) {
    register long r10 __asm__("r10") = a3;
    register long r8 __asm__("r8") = a4;
    register long r9 __asm__("r9") = a5;
    long out;
    __asm__ volatile("syscall" : "=a"(out) : "a"(nr), "D"(a0), "S"(a1), "d"(a2), "r"(r10), "r"(r8), "r"(r9) : "rcx", "r11", "memory");
    return out;
}
static void on_usr1(int sig, siginfo_t *info, void *context) {
    (void)sig; (void)info;
    uint64_t active = 0, saved = 0;
    ucontext_t *u = context;
    memcpy(&saved, &u->uc_sigmask, sizeof(saved));
    long rc = raw6(SYS_rt_sigprocmask, SIG_SETMASK, 0, (long)&active, 8, 0, 0);
    if (rc != 0) _exit(91);
    saved_usr1_blocked = (saved & (UINT64_C(1) << (SIGUSR1 - 1))) != 0;
    active_usr1_blocked = (active & (UINT64_C(1) << (SIGUSR1 - 1))) != 0;
    ++handler_calls;
}
static int fail(const char *what, long rc, int saved_errno) {
    fprintf(stderr, "setup_failure operation=%s result=%ld errno=%d\n", what, rc, saved_errno);
    return 90;
}
int main(void) {
    struct sigaction action = {0};
    action.sa_sigaction = on_usr1;
    action.sa_flags = SA_SIGINFO; /* deliberately no SA_RESTART */
    sigemptyset(&action.sa_mask);
    int rc = sigaction(SIGUSR1, &action, NULL); int e = errno;
    if (rc != 0) return fail("sigaction-USR1", rc, e);
    struct sigaction alarm_action = {0};
    alarm_action.sa_handler = SIG_DFL;
    sigemptyset(&alarm_action.sa_mask);
    rc = sigaction(SIGALRM, &alarm_action, NULL); e = errno;
    if (rc != 0) return fail("sigaction-ALRM", rc, e);
    const uint64_t original = UINT64_C(1) << (SIGUSR1 - 1);
    long r = raw6(SYS_rt_sigprocmask, SIG_SETMASK, (long)&original, 0, 8, 0, 0);
    if (r != 0) return fail("set-original-mask", r, 0);
    long pid = raw6(SYS_getpid, 0, 0, 0, 0, 0, 0);
    long tid = raw6(SYS_gettid, 0, 0, 0, 0, 0, 0);
    if (pid <= 0 || tid != pid) return fail("identity", tid, 0);
    r = raw6(SYS_tgkill, pid, tid, SIGUSR1, 0, 0, 0);
    if (r != 0) return fail("prequeue-USR1", r, 0);
    uint64_t pending_before = 0;
    r = raw6(SYS_rt_sigpending, (long)&pending_before, 8, 0, 0, 0, 0);
    if (r != 0 || pending_before != original || handler_calls != 0) return fail("pending-before", r, 0);
    struct probe_memory memory = {.mask_b = UINT64_C(1) << (SIGUSR2 - 1), .query = UINT64_C(0xa5a5a5a5a5a5a5a5)};
    alarm(5); /* upper bound only; no timer/sleep establishes causality */
    const long result = raw6(SYS_ppoll, 0, 0, (long)&memory.a, (long)&memory.mask_a, 8, 0);
    alarm(0);
    uint64_t mask_after = 0, pending_after = 0;
    r = raw6(SYS_rt_sigprocmask, SIG_SETMASK, 0, (long)&mask_after, 8, 0, 0);
    if (r != 0) return fail("mask-after", r, 0);
    r = raw6(SYS_rt_sigpending, (long)&pending_after, 8, 0, 0, 0, 0);
    if (r != 0) return fail("pending-after", r, 0);
    const int leaked = result == -512 || result == -513 || result == -514 || result == -516;
    printf("{\"event\":\"guest_outcome\",\"pid\":%ld,\"tid\":%ld,\"raw_result\":%ld,\"handler_calls\":%d,\"handler_saved_usr1_blocked\":%d,\"handler_active_usr1_blocked\":%d,\"mask_after\":%" PRIu64 ",\"pending_after\":%" PRIu64 ",\"query_word\":%" PRIu64 ",\"private_restart_leaked\":%s}\n", pid, tid, result, (int)handler_calls, (int)saved_usr1_blocked, (int)active_usr1_blocked, mask_after, pending_after, memory.query, leaked ? "true" : "false");
    if (fflush(stdout) != 0) return 93;
    return leaked ? 92 : 0;
}
