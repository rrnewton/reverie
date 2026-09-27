/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/*
 * Guest for the trap-only P2 site-patching tests
 * (reverie-ptrace/src/liteinst_trap_only_p2_tests.rs).
 *
 * Usage: trap_only_p2 <mode> <report-file>
 *
 * Every observation is appended to the report file as text; the test runs
 * the same mode under plain ptrace and under trap-only with site patching on
 * and requires equal reports. Absolute addresses are comparable because the
 * tracer disables address randomisation. PID values are never printed.
 *
 * `tp_site` is a shared generic syscall site: `tp_site_fn(nr, a1..a6)` issues
 * one `syscall` there. Its first execution patches it (the test Tool
 * subscribes to every syscall), so later executions go through the trap-only
 * hop. A call whose sixth argument (r9) carries TP_MAGIC asks the test Tool
 * for a handler shape or for tracer-side signals (see the Rust side).
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/futex.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>

#define TP_MAGIC 0x7e57000000000000L
/* Handler shapes (low byte) and tracer actions, decoded by the test Tool. */
#define SHAPE_INJECT 0
#define SHAPE_TAIL 1
#define SHAPE_EMULATE 2
#define SHAPE_PRIVATE 3
#define SHAPE_TWO_INJECTS 4
#define SHAPE_TWO_PRIVATE 5
#define SEND_SIGUSR1 0x100
#define SEND_SIGWINCH 0x200
#define SEND_QUEUE 0x400
/* The tracer leaves SIGUSR1 pending for its final resume of the stop. */
#define SEND_RESUME 0x800

long tp_site_fn(long nr, long a1, long a2, long a3, long a4, long a5, long a6);
extern char tp_site[], tp_site_end[];
/* rcx and r11 right after `syscall` at tp_site, split by whether the call
 * returned zero (a new child) or not (a parent or any other result). */
long tp_nz_rcx, tp_nz_r11, tp_z_rcx, tp_z_r11;

__asm__(".pushsection .text\n"
        ".globl tp_site_fn\n"
        ".type tp_site_fn, @function\n"
        "tp_site_fn:\n"
        "  mov %rdi, %rax\n"
        "  mov %rsi, %rdi\n"
        "  mov %rdx, %rsi\n"
        "  mov %rcx, %rdx\n"
        "  mov %r8, %r10\n"
        "  mov %r9, %r8\n"
        "  mov 8(%rsp), %r9\n"
        /* Fix the arithmetic flags, so that r11 (rflags at the syscall) does
         * not inherit the caller's `sub $8, %rsp`, whose parity follows the
         * randomized stack address. */
        "  cmp %rax, %rax\n"
        ".globl tp_site\n"
        "tp_site:\n"
        "  syscall\n"
        ".globl tp_site_end\n"
        "tp_site_end:\n"
        /* Clear the tag, so that a later libc call does not carry it. */
        "  xor %r9d, %r9d\n"
        "  test %rax, %rax\n"
        "  jz 1f\n"
        "  mov %rcx, tp_nz_rcx(%rip)\n"
        "  mov %r11, tp_nz_r11(%rip)\n"
        "  ret\n"
        "1:\n"
        "  mov %rcx, tp_z_rcx(%rip)\n"
        "  mov %r11, tp_z_r11(%rip)\n"
        "  ret\n"
        ".size tp_site_fn, .-tp_site_fn\n"
        ".popsection\n");

/* T8: a site that loads rcx/r11 sentinels immediately before `syscall` and
 * captures both right after it. rdi = nr, rsi = 1 to set DF, 2 to set AC. */
long t8_rcx, t8_r11;
long t8_fn(long nr, long flags);
extern char t8_site[], t8_site_end[];
__asm__(".pushsection .text\n"
        ".globl t8_fn\n"
        ".type t8_fn, @function\n"
        "t8_fn:\n"
        "  mov %rdi, %rax\n"
        "  cmp $1, %rsi\n"
        "  jne 2f\n"
        "  std\n"
        "2:\n"
        "  cmp $2, %rsi\n"
        "  jne 3f\n"
        "  pushf\n"
        "  orl $0x40000, (%rsp)\n"
        "  popf\n"
        "3:\n"
        "  mov $0x1111, %rcx\n"
        "  mov $0x2222, %r11\n"
        ".globl t8_site\n"
        "t8_site:\n"
        "  syscall\n"
        ".globl t8_site_end\n"
        "t8_site_end:\n"
        "  mov %rcx, t8_rcx(%rip)\n"
        "  mov %r11, t8_r11(%rip)\n"
        "  cld\n"
        "  pushf\n"
        "  andl $0xfffbffff, (%rsp)\n"
        "  popf\n"
        "  ret\n"
        ".size t8_fn, .-t8_fn\n"
        ".popsection\n");

#define SITE(nr, a1, a2, a3, a4, a5) \
  tp_site_fn((long)(nr), (long)(a1), (long)(a2), (long)(a3), (long)(a4), (long)(a5), 0)
/* r9 carries TP_MAGIC, a sequence number (bits 16-47) and the action. The
 * sequence number lets the tool act once per call even when the kernel
 * restarts it. */
static long tp_seq;
#define SITEM(nr, a1, a2, a3, a4, a5, act)                                              \
  tp_site_fn((long)(nr), (long)(a1), (long)(a2), (long)(a3), (long)(a4), (long)(a5), \
             TP_MAGIC | ((++tp_seq) << 16) | (act))

static int report_fd = -1;

static void say(const char *fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  vdprintf(report_fd, fmt, ap);
  va_end(ap);
}

static void die(const char *what) {
  say("FATAL %s errno=%d\n", what, errno);
  _exit(99);
}

static const char *where(long rip) {
  static char buf[64];
  if (rip == (long)tp_site_end)
    return "tp_site_end";
  if (rip == (long)tp_site)
    return "tp_site";
  if (rip == (long)t8_site_end)
    return "t8_site_end";
  /* The fixture is linked -no-pie: its own text addresses are the same in
   * every run, while library and stack addresses are not. */
  if (rip >= 0x400000 && rip < 0x1000000) {
    snprintf(buf, sizeof buf, "%#lx", rip);
    return buf;
  }
  return "other";
}

/* Warms tp_site: the first call patches it, the next two take the hop. */
static void warm(void) {
  for (int i = 0; i < 3; i++) {
    if (SITE(SYS_getpid, 0, 0, 0, 0, 0) != getpid())
      die("warm getpid");
  }
}

struct rec {
  int sig, code, value;
  long rip, rax, rcx, r11, trapno, err;
};
static struct rec recs[64];
static volatile int nrec;

static void handler(int sig, siginfo_t *si, void *uc_) {
  ucontext_t *uc = uc_;
  if (nrec >= 64)
    return;
  struct rec *r = &recs[nrec++];
  r->sig = sig;
  r->code = si->si_code;
  r->value = si->si_code == SI_QUEUE ? si->si_value.sival_int : -1;
  r->rip = uc->uc_mcontext.gregs[REG_RIP];
  r->rax = uc->uc_mcontext.gregs[REG_RAX];
  r->rcx = uc->uc_mcontext.gregs[REG_RCX];
  r->r11 = uc->uc_mcontext.gregs[REG_R11];
  r->trapno = uc->uc_mcontext.gregs[REG_TRAPNO];
  r->err = uc->uc_mcontext.gregs[REG_ERR];
}

static int restart_pipe_wr = -1;
static int *restart_futex;

/* The same recorder, plus the side effects that let a restarted call finish. */
static void restart_handler(int sig, siginfo_t *si, void *uc) {
  handler(sig, si, uc);
  if (restart_pipe_wr >= 0) {
    char c = 'x';
    if (write(restart_pipe_wr, &c, 1) != 1)
      _exit(98);
  }
  if (restart_futex)
    *restart_futex = 1;
}

static void install(int sig, int flags, void (*fn)(int, siginfo_t *, void *)) {
  struct sigaction sa;
  memset(&sa, 0, sizeof sa);
  sa.sa_sigaction = fn;
  sa.sa_flags = SA_SIGINFO | flags;
  sigemptyset(&sa.sa_mask);
  if (sigaction(sig, &sa, NULL) != 0)
    die("sigaction");
}

static void dump(const char *tag) {
  long pid = getpid();
  for (int i = 0; i < nrec; i++) {
    struct rec *r = &recs[i];
    char rax[32];
    if (r->rax == pid)
      snprintf(rax, sizeof rax, "<pid>");
    else
      snprintf(rax, sizeof rax, "%ld", r->rax);
    say("%s %d: sig=%d code=%d value=%d rip=%s rax=%s rcx=%s", tag, i, r->sig, r->code,
        r->value, where(r->rip), rax, where(r->rcx));
    say(" r11=%#lx trapno=%ld err=%ld\n", r->r11, r->trapno, r->err);
  }
  nrec = 0;
}

static void report_result(const char *tag, long r) {
  say("%s ret=%ld errno=%d rcx=%s r11=%#lx\n", tag, r, r < 0 && r > -4096 ? (int)-r : 0,
      where(r == 0 ? tp_z_rcx : tp_nz_rcx), r == 0 ? tp_z_r11 : tp_nz_r11);
}

static void site_bytes(const char *tag) {
  unsigned char *p = (unsigned char *)tp_site;
  say("%s site bytes %02x %02x\n", tag, p[0], p[1]);
}

/* T1a */
static void mode_sig_pending(void) {
  install(SIGUSR1, 0, handler);
  warm();
  long r = SITEM(SYS_getpid, 0, 0, 0, 0, 0, SHAPE_INJECT | SEND_SIGUSR1);
  say("getpid returned pid=%d\n", r == getpid());
  dump("sig_pending");
  r = SITEM(SYS_getpid, 0, 0, 0, 0, 0, SHAPE_TAIL | SEND_SIGUSR1);
  say("tail getpid returned pid=%d\n", r == getpid());
  dump("sig_pending_tail");
}

/* T1c (standard signals only; see the Rust side). */
static void mode_rt_queue(void) {
  int sigs[] = {SIGUSR1, SIGUSR2, SIGHUP, SIGALRM, SIGURG, SIGTERM};
  for (unsigned i = 0; i < sizeof sigs / sizeof sigs[0]; i++)
    install(sigs[i], 0, handler);
  warm();
  long r = SITEM(SYS_getpid, 0, 0, 0, 0, 0, SHAPE_INJECT | SEND_QUEUE);
  say("getpid returned pid=%d\n", r == getpid());
  dump("queue");
  r = SITEM(SYS_getpid, 0, 0, 0, 0, 0, SHAPE_TAIL | SEND_QUEUE);
  say("tail getpid returned pid=%d\n", r == getpid());
  dump("queue_tail");
}

/* T1f */
static void mode_self_raise(void) {
  install(SIGUSR1, 0, handler);
  install(SIGPIPE, 0, handler);
  install(SIGUSR2, 0, handler);
  warm();
  long r = SITE(SYS_kill, getpid(), SIGUSR1, 0, 0, 0);
  report_result("kill", r);
  dump("kill");
  int fds[2];
  if (pipe(fds) != 0)
    die("pipe");
  close(fds[0]);
  char c = 'p';
  r = SITE(SYS_write, fds[1], &c, 1, 0, 0);
  report_result("write-epipe", r);
  dump("sigpipe");
  close(fds[1]);
  sigset_t set;
  sigemptyset(&set);
  sigaddset(&set, SIGUSR2);
  if (sigprocmask(SIG_BLOCK, &set, NULL) != 0)
    die("block");
  raise(SIGUSR2);
  r = SITE(SYS_rt_sigprocmask, SIG_UNBLOCK, &set, NULL, 8, 0);
  report_result("unblock", r);
  dump("unblock");
}

static void sigtrap_state(const char *tag) {
  struct sigaction sa;
  sigset_t set;
  if (sigaction(SIGTRAP, NULL, &sa) != 0 || sigprocmask(SIG_SETMASK, NULL, &set) != 0)
    die("read sigtrap state");
  const char *disp = sa.sa_handler == SIG_IGN ? "ign" : sa.sa_handler == SIG_DFL ? "dfl" : "fn";
  say("%s sigtrap=%s blocked=%d\n", tag, disp, sigismember(&set, SIGTRAP));
  raise(SIGUSR1);
  dump(tag);
}

static void sigtrap_reset(void) {
  struct sigaction sa;
  memset(&sa, 0, sizeof sa);
  sa.sa_handler = SIG_IGN;
  if (sigaction(SIGTRAP, &sa, NULL) != 0)
    die("ignore sigtrap");
  sigset_t set;
  sigemptyset(&set);
  sigaddset(&set, SIGTRAP);
  if (sigprocmask(SIG_BLOCK, &set, NULL) != 0)
    die("block sigtrap");
}

/* T2 */
static void mode_sigtrap_profile(void) {
  install(SIGUSR1, 0, handler);
  for (int i = 0; i < 3; i++)
    SITE(SYS_getppid, 0, 0, 0, 0, 0);
  static const struct {
    const char *name;
    long shape;
  } shapes[] = {
      {"inject", SHAPE_INJECT},        {"tail", SHAPE_TAIL},
      {"emulate", SHAPE_EMULATE},      {"private", SHAPE_PRIVATE},
      {"two-injects", SHAPE_TWO_INJECTS},
      {"two-private", SHAPE_TWO_PRIVATE},
  };
  for (unsigned i = 0; i < sizeof shapes / sizeof shapes[0]; i++) {
    sigtrap_reset();
    long r = SITEM(SYS_getppid, 0, 0, 0, 0, 0, shapes[i].shape);
    say("%s getppid=%s\n", shapes[i].name,
        r == getppid() ? "ppid" : r == getpid() ? "pid" : r == 4242 ? "4242" : "other");
    sigtrap_state(shapes[i].name);
  }
}

static void report_ts(const char *tag, long r, struct timespec *rem) {
  report_result(tag, r);
  /* The remaining time is compared in 10 ms buckets: the signal is already
   * pending when the sleep starts, so it lands within the first bucket. */
  say("%s rem=%ld.%02ld\n", tag, (long)rem->tv_sec, rem->tv_nsec / 10000000L);
  dump(tag);
}

/* T3 */
static void mode_restart(void) {
  install(SIGUSR1, 0, restart_handler);
  warm();
  struct timespec req = {0, 100 * 1000 * 1000}, rem = {0, 0};
  long r;

  /* RESTARTBLOCK: a suppressed signal restarts through restart_syscall. */
  r = SITEM(SYS_nanosleep, &req, &rem, 0, 0, 0, SEND_SIGWINCH);
  report_ts("nanosleep-suppressed", r, &rem);
  rem.tv_sec = rem.tv_nsec = 0;
  r = SITEM(SYS_nanosleep, &req, &rem, 0, 0, 0, SEND_SIGUSR1);
  report_ts("nanosleep-handled", r, &rem);
  rem.tv_sec = rem.tv_nsec = 0;
  r = SITEM(SYS_clock_nanosleep, CLOCK_MONOTONIC, 0, &req, &rem, 0, SEND_SIGWINCH);
  report_ts("clock_nanosleep-suppressed", r, &rem);
  rem.tv_sec = rem.tv_nsec = 0;
  r = SITEM(SYS_clock_nanosleep, CLOCK_MONOTONIC, 0, &req, &rem, 0, SEND_SIGUSR1);
  report_ts("clock_nanosleep-handled", r, &rem);
  rem.tv_sec = rem.tv_nsec = 0;
  r = SITEM(SYS_nanosleep, &req, &rem, 0, 0, 0, SHAPE_TAIL | SEND_SIGWINCH);
  report_ts("nanosleep-suppressed-tail", r, &rem);

  /* ERESTARTSYS: read on an empty pipe, without and with SA_RESTART. */
  int fds[2];
  char c;
  if (pipe(fds) != 0)
    die("pipe");
  restart_pipe_wr = fds[1];
  r = SITEM(SYS_read, fds[0], &c, 1, 0, 0, SEND_SIGUSR1);
  report_result("read-eintr", r);
  dump("read-eintr");
  if (read(fds[0], &c, 1) != 1)
    die("drain");
  install(SIGUSR1, SA_RESTART, restart_handler);
  r = SITEM(SYS_read, fds[0], &c, 1, 0, 0, SEND_SIGUSR1);
  report_result("read-restart", r);
  dump("read-restart");
  r = SITEM(SYS_read, fds[0], &c, 1, 0, 0, SHAPE_TAIL | SEND_SIGUSR1);
  report_result("read-restart-tail", r);
  dump("read-restart-tail");
  restart_pipe_wr = -1;

  /* ERESTARTSYS: a futex wait, with SA_RESTART (the handler changes the
   * word, so the restarted wait fails with EAGAIN) and without. */
  static int word;
  restart_futex = &word;
  word = 0;
  r = SITEM(SYS_futex, &word, FUTEX_WAIT_PRIVATE, 0, NULL, 0, SEND_SIGUSR1);
  report_result("futex-restart", r);
  dump("futex-restart");
  install(SIGUSR1, 0, restart_handler);
  word = 0;
  r = SITEM(SYS_futex, &word, FUTEX_WAIT_PRIVATE, 0, NULL, 0, SEND_SIGUSR1);
  report_result("futex-eintr", r);
  dump("futex-eintr");
  restart_futex = NULL;

  /* ERESTARTNOHAND: ppoll and epoll_pwait, then pause. */
  struct pollfd pfd = {fds[0], POLLIN, 0};
  r = SITEM(SYS_ppoll, &pfd, 1, NULL, NULL, 8, SEND_SIGUSR1);
  report_result("ppoll", r);
  dump("ppoll");
  int ep = epoll_create1(0);
  if (ep < 0)
    die("epoll_create1");
  struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 0}, out;
  if (epoll_ctl(ep, EPOLL_CTL_ADD, fds[0], &ev) != 0)
    die("epoll_ctl");
  r = SITEM(SYS_epoll_pwait, ep, &out, 1, -1, NULL, SEND_SIGUSR1);
  report_result("epoll_pwait", r);
  dump("epoll_pwait");
  r = SITEM(SYS_pause, 0, 0, 0, 0, 0, SEND_SIGUSR1);
  report_result("pause", r);
  dump("pause");
}

static int thread_ctid;

static void thread_entry(void) {
  /* A raw CLONE_THREAD child on its own stack: no libc, no TLS. */
  __asm__ volatile("mov $60, %%eax\n"
                   "xor %%edi, %%edi\n"
                   "syscall\n" ::
                       : "memory");
  __builtin_unreachable();
}

/* T6a */
static void mode_fork_family(void) {
  warm();
  long r;
  int status;

  /* fork through the patched site; the child forks a grandchild there. */
  r = SITE(SYS_fork, 0, 0, 0, 0, 0);
  if (r == 0) {
    say("fork child rcx=%s r11=%#lx\n", where(tp_z_rcx), tp_z_r11);
    long g = SITE(SYS_fork, 0, 0, 0, 0, 0);
    if (g == 0)
      _exit(7);
    int gs;
    if (waitpid(g, &gs, 0) != g)
      _exit(90);
    _exit(WIFEXITED(gs) ? WEXITSTATUS(gs) : 91);
  }
  if (waitpid(r, &status, 0) != r)
    die("wait fork");
  report_result("fork", r > 0 ? 1 : r);
  say("fork child status exited=%d code=%d\n", WIFEXITED(status), WEXITSTATUS(status));

  /* vfork: the child exits at once without touching the shared stack. */
  r = SITE(SYS_vfork, 0, 0, 0, 0, 0);
  if (r == 0) {
    __asm__ volatile("mov $231, %%eax\n"
                     "mov $7, %%edi\n"
                     "syscall\n" ::
                         : "memory");
  }
  say("vfork child rcx=%s r11=%#lx\n", where(tp_z_rcx), tp_z_r11);
  report_result("vfork", r > 0 ? 1 : r);
  if (waitpid(r, &status, 0) != r)
    die("wait vfork");
  say("vfork child status exited=%d code=%d\n", WIFEXITED(status), WEXITSTATUS(status));

  /* clone3 without CLONE_VM (fork-like). */
  struct {
    uint64_t flags, pidfd, child_tid, parent_tid, exit_signal, stack, stack_size, tls;
  } args = {0, 0, 0, 0, SIGCHLD, 0, 0, 0};
  r = SITE(SYS_clone3, &args, sizeof args, 0, 0, 0);
  if (r == 0) {
    say("clone3 child rcx=%s r11=%#lx\n", where(tp_z_rcx), tp_z_r11);
    _exit(7);
  }
  if (waitpid(r, &status, 0) != r)
    die("wait clone3");
  report_result("clone3", r > 0 ? 1 : r);
  say("clone3 child status exited=%d code=%d\n", WIFEXITED(status), WEXITSTATUS(status));
  site_bytes("before thread");

  /* A thread (CLONE_VM|CLONE_THREAD) through the site: the child returns
   * from tp_site_fn on its own stack into thread_entry. */
  size_t size = 64 * 1024;
  char *stack = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (stack == MAP_FAILED)
    die("mmap stack");
  uintptr_t *top = (uintptr_t *)(stack + size - 64);
  top[0] = (uintptr_t)thread_entry;
  thread_ctid = 1;
  long flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD | CLONE_SYSVSEM |
               CLONE_CHILD_CLEARTID;
  r = SITE(SYS_clone, flags, top, NULL, &thread_ctid, 0);
  report_result("thread", r > 0 ? 1 : r);
  while (__atomic_load_n(&thread_ctid, __ATOMIC_SEQ_CST) != 0)
    syscall(SYS_futex, &thread_ctid, FUTEX_WAIT, thread_ctid, NULL, NULL, 0);
  say("thread child rcx=%s r11=%#lx\n", where(tp_z_rcx), tp_z_r11);
  site_bytes("after thread");
  r = SITE(SYS_getpid, 0, 0, 0, 0, 0);
  say("getpid after thread pid=%d\n", r == getpid());
}

/* T7c */
static void mode_foreign_int80(void) {
  pid_t child = fork();
  if (child < 0)
    die("fork");
  if (child == 0) {
    /* No core file in the working directory; core dumping stays host
     * policy either way, and both backends see the same limit. */
    struct rlimit none = {0, 0};
    setrlimit(RLIMIT_CORE, &none);
    install(SIGSYS, 0, handler);
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGSYS);
    sigprocmask(SIG_BLOCK, &set, NULL);
    long ret;
    __asm__ volatile("int $0x80" : "=a"(ret) : "a"(20L) : "memory");
    /* Reached only if the IA-32 syscall was serviced. */
    say("int80 returned handler_ran=%d\n", nrec);
    _exit(0);
  }
  int status;
  if (waitpid(child, &status, 0) != child)
    die("waitpid");
  say("child signaled=%d termsig=%d coredump=%d exited=%d\n", WIFSIGNALED(status),
      WIFSIGNALED(status) ? WTERMSIG(status) : 0, WIFSIGNALED(status) ? WCOREDUMP(status) : 0,
      WIFEXITED(status));
}

/* A signal the tracer passes on its final resume of a patched stop. */
static void mode_resume_signal(void) {
  install(SIGUSR1, 0, handler);
  warm();
  long r = SITEM(SYS_getpid, 0, 0, 0, 0, 0, SHAPE_INJECT | SEND_RESUME);
  say("getpid returned pid=%d\n", r == getpid());
  dump("resume_inject");
  r = SITEM(SYS_getpid, 0, 0, 0, 0, 0, SHAPE_EMULATE | SEND_RESUME);
  say("emulated getpid returned %ld\n", r);
  dump("resume_emulate");
  r = SITEM(SYS_getpid, 0, 0, 0, 0, 0, SHAPE_TAIL | SEND_RESUME);
  say("tail getpid returned pid=%d\n", r == getpid());
  dump("resume_tail");
}

/* Guest code running the private page's traced slot outside any hop. */
static void mode_stray_slot(void) {
  warm();
  long ret;
  __asm__ volatile("call *%1" : "=a"(ret) : "r"(0x71000004L), "a"(39L) : "rcx", "r11", "memory");
  say("stray slot returned %ld\n", ret);
}

/* T8 */
static void mode_rcx_r11(void) {
  for (int i = 0; i < 4; i++) {
    long r = t8_fn(SYS_getpid, 0);
    say("t8 %d pid=%d rcx=%s r11=%#lx\n", i, r == getpid(), where(t8_rcx), t8_r11);
  }
  long r = t8_fn(SYS_getpid, 1);
  say("t8 df pid=%d rcx=%s r11=%#lx\n", r == getpid(), where(t8_rcx), t8_r11);
  r = t8_fn(SYS_getpid, 2);
  say("t8 ac pid=%d rcx=%s r11=%#lx\n", r == getpid(), where(t8_rcx), t8_r11);

  /* A handler interrupting a patched blocking read sees rcx/r11 too. */
  install(SIGUSR1, 0, handler);
  warm();
  int fds[2];
  char c;
  if (pipe(fds) != 0)
    die("pipe");
  r = SITEM(SYS_read, fds[0], &c, 1, 0, 0, SEND_SIGUSR1);
  report_result("read", r);
  dump("read");
  /* And a fork child created through the site. */
  r = SITE(SYS_fork, 0, 0, 0, 0, 0);
  if (r == 0) {
    say("fork child rcx=%s r11=%#lx\n", where(tp_z_rcx), tp_z_r11);
    _exit(0);
  }
  int status;
  waitpid(r, &status, 0);
  report_result("fork parent", r > 0 ? 1 : r);
}

int main(int argc, char **argv) {
  if (argc != 3) {
    fprintf(stderr, "usage: %s <mode> <report>\n", argv[0]);
    return 2;
  }
  report_fd = open(argv[2], O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0644);
  if (report_fd < 0)
    return 3;
  say("mode %s site=%#lx\n", argv[1], (long)tp_site);
  const char *m = argv[1];
  if (!strcmp(m, "sig_pending"))
    mode_sig_pending();
  else if (!strcmp(m, "rt_queue"))
    mode_rt_queue();
  else if (!strcmp(m, "self_raise"))
    mode_self_raise();
  else if (!strcmp(m, "sigtrap_profile"))
    mode_sigtrap_profile();
  else if (!strcmp(m, "restart"))
    mode_restart();
  else if (!strcmp(m, "fork_family"))
    mode_fork_family();
  else if (!strcmp(m, "foreign_int80"))
    mode_foreign_int80();
  else if (!strcmp(m, "rcx_r11"))
    mode_rcx_r11();
  else if (!strcmp(m, "resume_signal"))
    mode_resume_signal();
  else if (!strcmp(m, "stray_slot"))
    mode_stray_slot();
  else
    return 4;
  say("done\n");
  return 0;
}
