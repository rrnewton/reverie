#define _GNU_SOURCE
#include <errno.h>
#include <inttypes.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef __x86_64__
#error This diagnostic binds Linux x86-64 syscall and ptrace register ABIs.
#endif

/* Separate instruction sites make a leftover single-step trap distinguishable
 * from execution of B. These are tracer-installed requests, never fake results. */
__asm__(".text\n"
        ".globl probe_a_stub\nprobe_a_stub:\nsyscall\n"
        ".globl probe_a_after\nprobe_a_after:\nint3\nret\n"
        ".globl probe_b_stub\nprobe_b_stub:\nsyscall\n"
        ".globl probe_b_after\nprobe_b_after:\nint3\nret\n");
extern const char probe_a_stub[], probe_a_after[], probe_b_stub[], probe_b_after[];

enum { NOHAND = 514, MAX_STOPS = 24, DEADLINE_SECONDS = 5 };
struct shared {
    uint64_t original, temporary_a, temporary_b, queried;
    struct timespec zero_a, zero_scratch, zero_b;
    volatile sig_atomic_t handler_calls;
};
struct snapshot {
    struct user_regs_struct regs;
    struct __ptrace_syscall_info syscall;
    uint64_t current, reported, pending, shared_pending;
    unsigned queued_usr1;
};
static struct shared *data;
static pid_t owned = -1;
static uint64_t owned_start;
static int child_owned;
static volatile sig_atomic_t expired;
static const char *route_name, *point_name, *b_name;
static unsigned case_number, sequence;

static long raw6(long nr, unsigned long a, unsigned long b, unsigned long c,
                 unsigned long d, unsigned long e, unsigned long f) {
    register unsigned long r10 __asm__("r10") = d;
    register unsigned long r8 __asm__("r8") = e;
    register unsigned long r9 __asm__("r9") = f;
    long result;
    __asm__ volatile("syscall" : "=a"(result)
                     : "a"(nr), "D"(a), "S"(b), "d"(c),
                       "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return result;
}
static void alarm_handler(int sig) { (void)sig; expired = 1; }
static void guest_handler(int sig) { (void)sig; data->handler_calls++; }

static void cleanup(void) {
    if (!child_owned) return;
    /* SIGCHLD was reset before fork; ownership ends only on our exact waitpid
     * result or ECHILD. Never signal a numeric PID after that point. */
    if (kill(owned, SIGKILL) < 0 && errno != ESRCH) perror("cleanup kill");
    expired = 0;
    alarm(DEADLINE_SECONDS);
    for (unsigned n = 0; n < MAX_STOPS; n++) {
        int status = 0;
        pid_t got = waitpid(owned, &status, __WALL);
        int saved = errno;
        if (got == owned && (WIFEXITED(status) || WIFSIGNALED(status))) {
            printf("{\"event\":\"retired\",\"case\":%u,\"pid\":%d,\"start\":%" PRIu64
                   ",\"status\":%d,\"signal\":%d}\n", case_number, owned,
                   owned_start, status, WIFSIGNALED(status) ? WTERMSIG(status) : 0);
            child_owned = 0;
            break;
        }
        if (got < 0 && saved == ECHILD) { child_owned = 0; break; }
        if (got < 0 && saved == EINTR && !expired) continue;
        if (got != owned || !WIFSTOPPED(status) || expired) break;
        (void)ptrace(PTRACE_CONT, owned, NULL, (void *)(uintptr_t)SIGKILL);
    }
    alarm(0);
    if (child_owned) {
        /* PTRACE_O_EXITKILL plus the outer cgroup retirement guard remain in
         * force. This is failure, never evidence of successful cleanup. */
        fputs("cleanup could not prove exact child retirement\n", stderr);
        _Exit(91);
    }
}
static void fail(const char *what, int line, long result, int error) {
    fprintf(stderr, "failure case=%u route=%s point=%s B=%s line=%d check=%s result=%ld errno=%d expired=%d\n",
            case_number, route_name, point_name, b_name, line, what, result,
            error, (int)expired);
    cleanup();
    exit(1);
}
#define REQUIRE(x) do { if (!(x)) fail(#x, __LINE__, 0, 0); } while (0)
static long request(enum __ptrace_request op, void *addr, void *value) {
    errno = 0;
    long result = ptrace(op, owned, addr, value);
    int saved = errno;
    if (result == -1 && saved) fail("ptrace", __LINE__, result, saved);
    return result;
}
static uint64_t start_time(void) {
    char path[80], buffer[4096];
    int n = snprintf(path, sizeof(path), "/proc/%d/stat", owned);
    REQUIRE(n > 0 && (size_t)n < sizeof(path));
    FILE *file = fopen(path, "r");
    if (!file) fail("open stat", __LINE__, -1, errno);
    REQUIRE(fgets(buffer, sizeof(buffer), file) != NULL);
    REQUIRE(fclose(file) == 0);
    char *end = strrchr(buffer, ')');
    REQUIRE(end != NULL);
    char *save = NULL;
    char *word = strtok_r(end + 1, " ", &save);
    for (unsigned field = 3; field < 22 && word; field++) word = strtok_r(NULL, " ", &save);
    REQUIRE(word != NULL);
    errno = 0;
    char *tail = NULL;
    unsigned long long value = strtoull(word, &tail, 10);
    REQUIRE(errno == 0 && tail != word && (*tail == '\0' || *tail == '\n'));
    return (uint64_t)value;
}
static int wait_stop(const char *phase) {
    int status = 0;
    errno = 0;
    pid_t got = waitpid(owned, &status, __WALL);
    int saved = errno;
    printf("{\"event\":\"wait\",\"case\":%u,\"phase\":\"%s\",\"pid\":%d,\"got\":%d,\"status\":%d,\"errno\":%d}\n",
           case_number, phase, owned, got, status, got < 0 ? saved : 0);
    if (got == owned && (WIFEXITED(status) || WIFSIGNALED(status))) child_owned = 0;
    REQUIRE(!expired && got == owned && WIFSTOPPED(status));
    REQUIRE(++sequence <= MAX_STOPS);
    return WSTOPSIG(status);
}
static void resume(enum __ptrace_request op, const char *why) {
    REQUIRE(!expired && child_owned);
    printf("{\"event\":\"resume\",\"case\":%u,\"why\":\"%s\",\"request\":%d,\"delivered_signal\":0}\n",
           case_number, why, (int)op);
    request(op, NULL, NULL);
}
static struct snapshot capture(const char *phase, int stop_signal) {
    struct snapshot s = {0};
    REQUIRE(start_time() == owned_start);
    request(PTRACE_GETREGS, NULL, &s.regs);
    long info_bytes = request(PTRACE_GET_SYSCALL_INFO,
                              (void *)(uintptr_t)sizeof(s.syscall), &s.syscall);
    REQUIRE(info_bytes >= (long)offsetof(struct __ptrace_syscall_info, entry));
    if (s.syscall.op == PTRACE_SYSCALL_INFO_ENTRY)
        REQUIRE(info_bytes >= (long)(offsetof(struct __ptrace_syscall_info, entry.args) + sizeof(s.syscall.entry.args)));
    if (s.syscall.op == PTRACE_SYSCALL_INFO_EXIT)
        REQUIRE(info_bytes >= (long)(offsetof(struct __ptrace_syscall_info, exit.is_error) + sizeof(s.syscall.exit.is_error)));
    request(PTRACE_GETSIGMASK, (void *)(uintptr_t)sizeof(s.reported), &s.reported);
    char path[80], line[4096];
    int n = snprintf(path, sizeof(path), "/proc/%d/status", owned);
    REQUIRE(n > 0 && (size_t)n < sizeof(path));
    FILE *file = fopen(path, "r");
    if (!file) fail("open status", __LINE__, -1, errno);
    unsigned fields = 0;
    while (fgets(line, sizeof(line), file)) {
        unsigned long long value;
        if (sscanf(line, "SigBlk: %llx", &value) == 1) { s.current = value; fields |= 1; }
        if (sscanf(line, "SigPnd: %llx", &value) == 1) { s.pending = value; fields |= 2; }
        if (sscanf(line, "ShdPnd: %llx", &value) == 1) { s.shared_pending = value; fields |= 4; }
    }
    REQUIRE(!ferror(file) && fclose(file) == 0 && fields == 7);
    printf("{\"event\":\"snapshot\",\"case\":%u,\"phase\":\"%s\",\"pid\":%d,\"start\":%" PRIu64
           ",\"stop\":%d,\"op\":%u,\"rax\":%" PRId64 ",\"orig_rax\":%" PRId64
           ",\"rip\":%llu,\"current_mask\":%" PRIu64 ",\"ptrace_mask\":%" PRIu64
           ",\"pending\":%" PRIu64 ",\"shared_pending\":%" PRIu64
           ",\"handler_calls\":%d,\"query_word\":%" PRIu64 "}\n",
           case_number, phase, owned, owned_start, stop_signal, s.syscall.op,
           (int64_t)s.regs.rax, (int64_t)s.regs.orig_rax, s.regs.rip,
           s.current, s.reported, s.pending, s.shared_pending,
           (int)data->handler_calls, data->queried);
    for (unsigned domain = 0; domain < 2; domain++) {
        struct __ptrace_peeksiginfo_args args = {
            .off = 0, .flags = domain ? PTRACE_PEEKSIGINFO_SHARED : 0, .nr = 8
        };
        siginfo_t infos[8] = {{0}};
        long count = request(PTRACE_PEEKSIGINFO, &args, infos);
        REQUIRE(count >= 0 && count < 8); /* Require a complete finite queue read. */
        printf("{\"event\":\"queue_scan\",\"case\":%u,\"phase\":\"%s\",\"domain\":%u,\"count\":%ld,\"complete\":true}\n",
               case_number, phase, domain, count);
        for (long i = 0; i < count; i++) {
            printf("{\"event\":\"queued\",\"case\":%u,\"phase\":\"%s\",\"domain\":%u,\"signo\":%d,\"code\":%d,\"sender\":%d}\n",
                   case_number, phase, domain, infos[i].si_signo, infos[i].si_code, infos[i].si_pid);
            if (infos[i].si_signo == SIGUSR1) s.queued_usr1++;
        }
    }
    if (stop_signal != (SIGTRAP | 0x80)) {
        siginfo_t info = {0};
        request(PTRACE_GETSIGINFO, NULL, &info);
        printf("{\"event\":\"delivery_stop\",\"case\":%u,\"phase\":\"%s\",\"signo\":%d,\"code\":%d,\"sender\":%d,\"addr\":%" PRIuPTR "}\n",
               case_number, phase, info.si_signo, info.si_code, info.si_pid, (uintptr_t)info.si_addr);
        if (stop_signal == SIGUSR1) {
            REQUIRE(info.si_signo == SIGUSR1 && info.si_code == SI_TKILL && info.si_pid == owned);
            REQUIRE(s.queued_usr1 == 0);
        }
    }
    return s;
}
static void tracee(void) {
    struct sigaction action = {.sa_handler = guest_handler};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGUSR1, &action, NULL) != 0) _exit(92);
    if (raw6(SYS_rt_sigprocmask, SIG_SETMASK, (uintptr_t)&data->original, 0, 8, 0, 0) != 0) _exit(92);
    long me = raw6(SYS_getpid, 0, 0, 0, 0, 0, 0);
    if (me <= 0 || raw6(SYS_tgkill, (unsigned long)me, (unsigned long)me, SIGUSR1, 0, 0, 0) != 0) _exit(92);
    if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) _exit(92);
    if (raw6(SYS_tgkill, (unsigned long)me, (unsigned long)me, SIGSTOP, 0, 0, 0) != 0) _exit(92);
    (void)raw6(SYS_ppoll, 0, 0, (uintptr_t)&data->zero_a, (uintptr_t)&data->temporary_a, 8, 0);
    _exit(93); /* Tracer must stop at B's exit before any original continuation. */
}
static void install(struct user_regs_struct regs, const char *site, long nr,
                    unsigned long a, unsigned long b, unsigned long c,
                    unsigned long d, unsigned long e, unsigned long f) {
    regs.rip = (uintptr_t)site;
    regs.rax = (unsigned long)nr;
    regs.orig_rax = (unsigned long)nr;
    regs.rdi = a; regs.rsi = b; regs.rdx = c; regs.r10 = d; regs.r8 = e; regs.r9 = f;
    request(PTRACE_SETREGS, NULL, &regs);
    printf("{\"event\":\"install_request\",\"case\":%u,\"nr\":%ld,\"rip\":%" PRIuPTR
           ",\"args\":[%lu,%lu,%lu,%lu,%lu,%lu]}\n", case_number, nr, (uintptr_t)site, a, b, c, d, e, f);
}
static struct snapshot syscall_stop(const char *phase, unsigned op, long nr) {
    int sig = wait_stop(phase);
    REQUIRE(sig == (SIGTRAP | 0x80));
    struct snapshot s = capture(phase, sig);
    REQUIRE(s.syscall.op == op);
    if (op == PTRACE_SYSCALL_INFO_ENTRY) REQUIRE(s.syscall.entry.nr == (uint64_t)nr);
    else REQUIRE(s.syscall.exit.rval == (int64_t)s.regs.rax);
    return s;
}
static void one_case(unsigned route, unsigned held, unsigned b) {
    route_name = route == 0 ? "exact-syscall" : route == 1 ? "private-syscall" : "private-step";
    point_name = held ? "signal-held" : route == 2 ? "actual-step-completion" : "syscall-exit-queued";
    b_name = b == 0 ? "getpid" : b == 1 ? "mask-query" : "second-wait";
    sequence = 0; expired = 0; owned_start = 0;
    memset(data, 0, sizeof(*data));
    data->original = UINT64_C(1) << (SIGUSR1 - 1);
    data->temporary_b = UINT64_C(1) << (SIGUSR2 - 1);
    data->queried = UINT64_C(0xa5a5a5a5a5a5a5a5);
    printf("{\"event\":\"case_start\",\"case\":%u,\"route\":\"%s\",\"point\":\"%s\",\"B\":\"%s\",\"O\":%" PRIu64 ",\"T_A\":0,\"T_B\":%" PRIu64 "}\n",
           case_number, route_name, point_name, b_name, data->original, data->temporary_b);
    owned = fork();
    if (owned < 0) fail("fork", __LINE__, owned, errno);
    if (owned == 0) tracee();
    child_owned = 1; alarm(DEADLINE_SECONDS);
    int sig = wait_stop("initial");
    REQUIRE(sig == SIGSTOP);
    owned_start = start_time();
    request(PTRACE_SETOPTIONS, NULL, (void *)(uintptr_t)(PTRACE_O_TRACESYSGOOD | PTRACE_O_EXITKILL));
    struct snapshot s = capture("initial", sig);
    REQUIRE(s.current == data->original && s.reported == data->original && s.queued_usr1 == 1);
    resume(PTRACE_SYSCALL, "forward-to-original-A-entry");
    s = syscall_stop("A-original-entry", PTRACE_SYSCALL_INFO_ENTRY, SYS_ppoll);
    REQUIRE(s.syscall.entry.args[0] == 0 && s.syscall.entry.args[1] == 0);
    REQUIRE(s.syscall.entry.args[2] == (uintptr_t)&data->zero_a && s.syscall.entry.args[3] == (uintptr_t)&data->temporary_a && s.syscall.entry.args[4] == 8);
    if (route) {
        s.regs.orig_rax = (unsigned long long)-1;
        request(PTRACE_SETREGS, NULL, &s.regs);
        resume(PTRACE_SYSCALL, "skip-original-A-before-any-mask-effect");
        s = syscall_stop("A-skipped-exit", PTRACE_SYSCALL_INFO_EXIT, -1);
        REQUIRE((int64_t)s.regs.rax == -ENOSYS && s.current == data->original && s.queued_usr1 == 1);
        install(s.regs, probe_a_stub, SYS_ppoll, 0, 0,
                (uintptr_t)&data->zero_scratch, (uintptr_t)&data->temporary_a, 8, 0);
        if (route == 2) {
            resume(PTRACE_SINGLESTEP, "execute-private-A-single-step");
            sig = wait_stop("A-step-completion");
            REQUIRE(sig == SIGTRAP || sig == SIGUSR1);
            s = capture("A-step-completion", sig);
            REQUIRE(s.regs.rip == (uintptr_t)probe_a_after);
        } else {
            resume(PTRACE_SYSCALL, "execute-private-A-entry");
            s = syscall_stop("A-private-entry", PTRACE_SYSCALL_INFO_ENTRY, SYS_ppoll);
            resume(PTRACE_SYSCALL, "execute-private-A-exit");
            s = syscall_stop("A-exit", PTRACE_SYSCALL_INFO_EXIT, SYS_ppoll);
        }
    } else {
        resume(PTRACE_SYSCALL, "execute-exact-A-exit");
        s = syscall_stop("A-exit", PTRACE_SYSCALL_INFO_EXIT, SYS_ppoll);
    }
    REQUIRE((int64_t)s.regs.rax == -NOHAND && data->handler_calls == 0);
    if (route != 2) REQUIRE(s.queued_usr1 == 1); /* Exit stop precedes delivery. */
    if (held) {
        resume(PTRACE_CONT, "reach-real-SIGUSR1-delivery-stop");
        sig = wait_stop("A-held");
        REQUIRE(sig == SIGUSR1);
        s = capture("A-held", sig);
        REQUIRE((int64_t)s.regs.rax == -NOHAND && s.queued_usr1 == 0);
    }
    long nr = b == 0 ? SYS_getpid : b == 1 ? SYS_rt_sigprocmask : SYS_ppoll;
    if (b == 0) install(s.regs, probe_b_stub, nr, 0, 0, 0, 0, 0, 0);
    else if (b == 1) install(s.regs, probe_b_stub, nr, SIG_SETMASK, 0, (uintptr_t)&data->queried, 8, 0, 0);
    else install(s.regs, probe_b_stub, nr, 0, 0, (uintptr_t)&data->zero_b, (uintptr_t)&data->temporary_b, 8, 0);
    resume(PTRACE_SYSCALL, "resume-for-B-suppressing-current-stop-if-any");
    unsigned suppressed_usr1 = 0, stale_step = 0;
    for (;;) {
        sig = wait_stop("before-B-entry");
        s = capture("before-B-entry", sig);
        if (sig == (SIGTRAP | 0x80)) {
            REQUIRE(s.syscall.op == PTRACE_SYSCALL_INFO_ENTRY && s.syscall.entry.nr == (uint64_t)nr);
            REQUIRE(s.regs.rip == (uintptr_t)probe_b_after);
            break;
        }
        if (sig == SIGUSR1) {
            REQUIRE(++suppressed_usr1 == 1);
            resume(PTRACE_SYSCALL, "suppress-actual-USR1-delivery-before-B");
        } else if (sig == SIGTRAP && route == 2) {
            siginfo_t info = {0}; request(PTRACE_GETSIGINFO, NULL, &info);
            REQUIRE(++stale_step == 1 && info.si_code == TRAP_TRACE);
            REQUIRE((uintptr_t)info.si_addr == (uintptr_t)probe_a_after && s.regs.rip == (uintptr_t)probe_b_stub);
            resume(PTRACE_SYSCALL, "suppress-exact-old-A-step-trap-before-B");
        } else fail("unexpected pre-B stop", __LINE__, sig, 0);
    }
    resume(PTRACE_SYSCALL, "execute-B-to-syscall-exit");
    s = syscall_stop("B-exit", PTRACE_SYSCALL_INFO_EXIT, nr);
    long result = (long)s.regs.rax;
    if (b == 0) REQUIRE(result == owned);
    if (b == 1) REQUIRE(result == 0 && data->queried != UINT64_C(0xa5a5a5a5a5a5a5a5));
    if (b == 2) REQUIRE(result == 0 || result == -NOHAND);
    REQUIRE(data->handler_calls == 0);
    printf("{\"event\":\"case_observed\",\"case\":%u,\"B_result\":%ld,\"query_word\":%" PRIu64
           ",\"before_B_suppressed_usr1\":%u,\"before_B_stale_step\":%u,\"queued_usr1_after_B\":%u,\"current_after_B\":%" PRIu64
           ",\"ptrace_mask_after_B\":%" PRIu64 ",\"guest_handler_executed\":false}\n",
           case_number, result, data->queried, suppressed_usr1, stale_step, s.queued_usr1, s.current, s.reported);
    cleanup();
    REQUIRE(!child_owned);
}
int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    struct sigaction action = {.sa_handler = SIG_DFL};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGCHLD, &action, NULL) != 0) return 1;
    action.sa_handler = alarm_handler;
    if (sigaction(SIGALRM, &action, NULL) != 0) return 1;
    sigset_t unblocked;
    sigemptyset(&unblocked); sigaddset(&unblocked, SIGCHLD); sigaddset(&unblocked, SIGALRM);
    if (sigprocmask(SIG_UNBLOCK, &unblocked, NULL) != 0) return 1;
    data = mmap(NULL, sizeof(*data), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (data == MAP_FAILED) return 1;
    for (unsigned route = 0; route < 3; route++) {
        for (unsigned held = 0; held < (route == 2 ? 1U : 2U); held++) {
            for (unsigned b = 0; b < 3; b++) { case_number++; one_case(route, held, b); }
        }
    }
    if (munmap(data, sizeof(*data)) != 0) return 1;
    printf("{\"event\":\"experiment_complete\",\"cases\":%u,\"all_children_reaped\":true,\"hypothesis_verdict\":\"requires-raw-comparison\"}\n", case_number);
    return case_number == 15 ? 0 : 1;
}
