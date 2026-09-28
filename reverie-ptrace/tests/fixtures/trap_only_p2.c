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
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/futex.h>
#include <linux/seccomp.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <spawn.h>
#include <stddef.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/shm.h>
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
static char **main_argv;
extern char **environ;

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

/* What a sleep's remaining time says, without its host-dependent digits.
 * rem is zeroed before each call, so "unwritten" means the kernel never
 * copied a remaining time out (the sleep was not interrupted). The tracer
 * sends the signal before the call, so an interrupted sleep ends at once and
 * the kernel reports about the whole request: "whole" is more than half and
 * at most one and a half times the request. Timer slack can put the exact
 * value either side of the request, which is why a 10 ms bucket there
 * flaked (rem=0.09 against rem=0.10); these bounds lie 50 ms from it.
 * Anything else ("partial": interrupted after more than half the request
 * elapsed; "invalid": not a normalized timespec or beyond the upper bound)
 * is named without digits too, so it compares exactly across runs. */
static const char *rem_class(const struct timespec *req, const struct timespec *rem) {
  if (rem->tv_sec == 0 && rem->tv_nsec == 0)
    return "unwritten";
  if (rem->tv_sec < 0 || rem->tv_nsec < 0 || rem->tv_nsec >= 1000000000L)
    return "invalid";
  long long want = (long long)req->tv_sec * 1000000000LL + req->tv_nsec;
  long long left = (long long)rem->tv_sec * 1000000000LL + rem->tv_nsec;
  if (left > want + want / 2)
    return "invalid";
  if (left > want / 2)
    return "whole";
  return "partial";
}

static void report_ts(const char *tag, long r, const struct timespec *req,
                      struct timespec *rem) {
  report_result(tag, r);
  say("%s rem=%s\n", tag, rem_class(req, rem));
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
  report_ts("nanosleep-suppressed", r, &req, &rem);
  rem.tv_sec = rem.tv_nsec = 0;
  r = SITEM(SYS_nanosleep, &req, &rem, 0, 0, 0, SEND_SIGUSR1);
  report_ts("nanosleep-handled", r, &req, &rem);
  rem.tv_sec = rem.tv_nsec = 0;
  r = SITEM(SYS_clock_nanosleep, CLOCK_MONOTONIC, 0, &req, &rem, 0, SEND_SIGWINCH);
  report_ts("clock_nanosleep-suppressed", r, &req, &rem);
  rem.tv_sec = rem.tv_nsec = 0;
  r = SITEM(SYS_clock_nanosleep, CLOCK_MONOTONIC, 0, &req, &rem, 0, SEND_SIGUSR1);
  report_ts("clock_nanosleep-handled", r, &req, &rem);
  rem.tv_sec = rem.tv_nsec = 0;
  r = SITEM(SYS_nanosleep, &req, &rem, 0, 0, 0, SHAPE_TAIL | SEND_SIGWINCH);
  report_ts("nanosleep-suppressed-tail", r, &req, &rem);

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
  /* SIGCHLD stays blocked (and is inherited blocked) until the end, so its
   * one delivery lands at a fixed point in the syscall stream instead of
   * wherever a child's exit happens to overtake its parent. */
  sigset_t chld;
  sigemptyset(&chld);
  sigaddset(&chld, SIGCHLD);
  if (sigprocmask(SIG_BLOCK, &chld, NULL) != 0)
    die("block SIGCHLD");

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
  /* Join without syscalls, so whether the thread has already exited does
   * not change the parent's syscall stream (a futex wait did). */
  while (__atomic_load_n(&thread_ctid, __ATOMIC_SEQ_CST) != 0)
    __builtin_ia32_pause();
  say("thread child rcx=%s r11=%#lx\n", where(tp_z_rcx), tp_z_r11);
  site_bytes("after thread");
  r = SITE(SYS_getpid, 0, 0, 0, 0, 0);
  say("getpid after thread pid=%d\n", r == getpid());
  if (sigprocmask(SIG_UNBLOCK, &chld, NULL) != 0)
    die("unblock SIGCHLD");
}

/* T7c */
static void mode_foreign_int80(void) {
  /* SIGCHLD stays blocked across the fork and the wait, so its one delivery
   * lands at the unblock below instead of wherever the child's death
   * happens to overtake the parent's wait4. */
  sigset_t chld;
  sigemptyset(&chld);
  sigaddset(&chld, SIGCHLD);
  if (sigprocmask(SIG_BLOCK, &chld, NULL) != 0)
    die("block SIGCHLD");
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
  if (sigprocmask(SIG_UNBLOCK, &chld, NULL) != 0)
    die("unblock SIGCHLD");
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
  /* And a fork child created through the site. SIGCHLD stays blocked
   * across the fork and the wait, so its one delivery lands at the unblock
   * instead of wherever the child's exit happens to overtake the parent. */
  sigset_t chld;
  sigemptyset(&chld);
  sigaddset(&chld, SIGCHLD);
  if (sigprocmask(SIG_BLOCK, &chld, NULL) != 0)
    die("block SIGCHLD");
  r = SITE(SYS_fork, 0, 0, 0, 0, 0);
  if (r == 0) {
    say("fork child rcx=%s r11=%#lx\n", where(tp_z_rcx), tp_z_r11);
    _exit(0);
  }
  int status;
  if (waitpid(r, &status, 0) != r)
    die("wait fork");
  report_result("fork parent", r > 0 ? 1 : r);
  if (sigprocmask(SIG_UNBLOCK, &chld, NULL) != 0)
    die("unblock SIGCHLD");
}


/* ---- P2c: site-table lifecycle (T6b-T6d) and guest installs (T7a-T7b) ---- */

/* Runs this fixture again, in `mode`, with the same report file. */
static void exec_self(const char *mode) {
  char *args[] = {"/proc/self/exe", (char *)mode, main_argv[2], NULL};
  execve(args[0], args, environ);
  die("execve");
}

/* SIGCHLD stays blocked (and is inherited blocked) from chld_block() to
 * chld_unblock(), so that its one delivery lands at the unblock instead of
 * wherever a child's exit happens to overtake the parent's wait. */
static void chld_block(void) {
  sigset_t chld;
  sigemptyset(&chld);
  sigaddset(&chld, SIGCHLD);
  if (sigprocmask(SIG_BLOCK, &chld, NULL) != 0)
    die("block SIGCHLD");
}

static void chld_unblock(void) {
  sigset_t chld;
  sigemptyset(&chld);
  sigaddset(&chld, SIGCHLD);
  if (sigprocmask(SIG_UNBLOCK, &chld, NULL) != 0)
    die("unblock SIGCHLD");
}

/* The image an exec installs: its table starts empty, so warm() patches the
 * site again (under trap-only) at the same -no-pie address. */
static void mode_exec_image(void) {
  warm();
  say("exec image getpid ok\n");
}

/* T6b: the leader execs. */
static void mode_exec_leader(void) {
  warm();
  exec_self("exec_image");
}

/* Set by the leader after its last syscall (the tail of pthread_create). */
static volatile int exec_leader_done;

static void *exec_thread_entry(void *arg) {
  (void)arg;
  /* Exec only once the leader has made its last syscall, so that its stop
   * sequence is the same in every run: the leader publishes the flag after
   * pthread_create has returned, and never enters the kernel again. Both
   * sides spin without syscalls, so the order is causal, not timed. */
  while (!__atomic_load_n(&exec_leader_done, __ATOMIC_SEQ_CST))
    __builtin_ia32_pause();
  exec_self("exec_image");
  return NULL;
}

/* T6b: a non-leader thread execs; the kernel kills the leader first. The
 * leader spins in user space, without syscalls, until the exec kills it:
 * blocking in pause() instead would make its last stops depend on whether
 * the exec landed before or after the tracer let it into the call. */
static void mode_exec_thread(void) {
  warm();
  pthread_t thread;
  if (pthread_create(&thread, NULL, exec_thread_entry, NULL) != 0)
    die("pthread_create");
  __atomic_store_n(&exec_leader_done, 1, __ATOMIC_SEQ_CST);
  for (;;)
    __builtin_ia32_pause();
}

/* x86_64 code `mov $nr, %eax; syscall; ret` after `pad` nops; the syscall is
 * at code + pad + 5. */
static void emit(unsigned char *code, int pad, int nr) {
  for (int i = 0; i < pad; i++)
    code[i] = 0x90;
  unsigned char body[] = {0xb8, (unsigned char)nr, 0, 0, 0, 0x0f, 0x05, 0xc3};
  memcpy(code + pad, body, sizeof body);
}

static unsigned char *map_at(unsigned long address, int prot, int extra) {
  void *p = mmap((void *)address, 4096, prot, MAP_PRIVATE | MAP_ANONYMOUS | extra, -1, 0);
  if (p != (void *)address)
    die("mmap jit");
  return p;
}

static long run_code(unsigned char *code) {
  return ((long (*)(void))code)();
}

static void protect(void *address, int prot) {
  if (mprotect(address, 4096, prot) != 0)
    die("mprotect");
}

static void jit_bytes(const char *tag, unsigned char *site) {
  say("%s bytes %02x %02x\n", tag, site[0], site[1]);
}

/* T6c: JIT code; its site is patched only while its page is r-xp, and every
 * mapping change restores the bytes before it runs. */
static void mode_jit(void) {
  const int rw = PROT_READ | PROT_WRITE, rx = PROT_READ | PROT_EXEC;
  int status;

  unsigned char *a = map_at(0x50000000, rw, MAP_FIXED_NOREPLACE);
  emit(a, 0, SYS_getpid);
  protect(a, rx);
  for (int i = 0; i < 3; i++)
    say("jit a %d pid=%d\n", i, run_code(a) == getpid());
  /* A fork child changes its own copy of the page: the parent's patch, in
   * the parent's own table, must stay live. */
  chld_block();
  pid_t child = fork();
  if (child == 0) {
    protect(a, rw);
    jit_bytes("jit fork child a", a + 5);
    _exit(0);
  }
  if (waitpid(child, &status, 0) != child)
    die("wait jit child");
  say("jit fork child exited=%d code=%d\n", WIFEXITED(status), WEXITSTATUS(status));
  chld_unblock();
  say("jit a after fork pid=%d\n", run_code(a) == getpid());
  /* Writable again: the guest reads its own bytes, then rewrites the code. */
  protect(a, rw);
  jit_bytes("jit a after mprotect rw", a + 5);
  emit(a, 2, SYS_getppid);
  protect(a, rx);
  for (int i = 0; i < 3; i++)
    say("jit a2 %d ppid=%d\n", i, run_code(a) > 0);

  /* munmap of a patched page, then new code at the same address. */
  unsigned char *b = map_at(0x50010000, rw, MAP_FIXED_NOREPLACE);
  emit(b, 0, SYS_getpid);
  protect(b, rx);
  for (int i = 0; i < 3; i++)
    say("jit b %d pid=%d\n", i, run_code(b) == getpid());
  if (munmap(b, 4096) != 0)
    die("munmap");
  b = map_at(0x50010000, rw, MAP_FIXED_NOREPLACE);
  jit_bytes("jit b after munmap", b + 5);
  emit(b, 0, SYS_gettid);
  protect(b, rx);
  for (int i = 0; i < 3; i++)
    say("jit b2 %d tid=%d\n", i, run_code(b) == gettid());

  /* mmap(MAP_FIXED) over a patched page. */
  unsigned char *d = map_at(0x50050000, rw, MAP_FIXED_NOREPLACE);
  emit(d, 0, SYS_getpid);
  protect(d, rx);
  for (int i = 0; i < 3; i++)
    say("jit d %d pid=%d\n", i, run_code(d) == getpid());
  d = map_at(0x50050000, rw, MAP_FIXED);
  jit_bytes("jit d after mmap fixed", d + 5);

  /* mremap moves a patched page. */
  unsigned char *c = map_at(0x50020000, rw, MAP_FIXED_NOREPLACE);
  emit(c, 0, SYS_getpid);
  protect(c, rx);
  for (int i = 0; i < 3; i++)
    say("jit c %d pid=%d\n", i, run_code(c) == getpid());
  unsigned char *moved =
      mremap(c, 4096, 4096, MREMAP_MAYMOVE | MREMAP_FIXED, (void *)0x50030000);
  if (moved != (void *)0x50030000)
    die("mremap");
  jit_bytes("jit c after mremap", moved + 5);
  for (int i = 0; i < 3; i++)
    say("jit c2 %d pid=%d\n", i, run_code(moved) == getpid());

  /* A writable and executable page is never patched. */
  unsigned char *e = map_at(0x50040000, rw | PROT_EXEC, MAP_FIXED_NOREPLACE);
  emit(e, 0, SYS_getpid);
  for (int i = 0; i < 3; i++)
    say("jit e %d pid=%d\n", i, run_code(e) == getpid());
  jit_bytes("jit e", e + 5);

  /* madvise(MADV_DONTNEED) on the fixture's own patched text page. */
  warm();
  if (madvise((void *)((unsigned long)tp_site & ~4095UL), 4096, MADV_DONTNEED) != 0)
    die("madvise");
  site_bytes("after madvise");
  warm();
}

/* T6c, continued. A one-byte store through /proc/self/mem (which no
 * lifecycle stop sees) over a patched JIT site, then an mprotect that
 * restores what is left of the patch: the guest reads its own byte and the
 * original second byte. Then an mremap(MREMAP_FIXED) of another page onto a
 * patched page, which retires the patched page's site. */
static void mode_jit_more(void) {
  const int rw = PROT_READ | PROT_WRITE, rx = PROT_READ | PROT_EXEC;

  unsigned char *a = map_at(0x51000000, rw, MAP_FIXED_NOREPLACE);
  emit(a, 0, SYS_getpid);
  protect(a, rx);
  for (int i = 0; i < 3; i++)
    say("more a %d pid=%d\n", i, run_code(a) == getpid());
  int fd = open("/proc/self/mem", O_RDWR);
  if (fd < 0)
    die("open /proc/self/mem");
  unsigned char nop = 0x90;
  if (pwrite(fd, &nop, 1, (off_t)(unsigned long)(a + 5)) != 1)
    die("pwrite /proc/self/mem");
  close(fd);
  protect(a, rw);
  jit_bytes("more a after self write", a + 5);

  unsigned char *f = map_at(0x51010000, rw, MAP_FIXED_NOREPLACE);
  emit(f, 0, SYS_getpid);
  protect(f, rx);
  for (int i = 0; i < 3; i++)
    say("more f %d pid=%d\n", i, run_code(f) == getpid());
  unsigned char *g = map_at(0x51020000, rw, MAP_FIXED_NOREPLACE);
  emit(g, 2, SYS_gettid);
  protect(g, rx);
  unsigned char *moved = mremap(g, 4096, 4096, MREMAP_MAYMOVE | MREMAP_FIXED, (void *)f);
  if (moved != f)
    die("mremap onto a patched page");
  jit_bytes("more f after mremap", f + 5);
  for (int i = 0; i < 3; i++)
    say("more f2 %d tid=%d\n", i, run_code(f) == gettid());
}

/* T6a, undecided: a fork through the patched site whose clone flags the
 * test makes the tracer forget, as for a Tool-injected clone, or record as
 * CLONE_VM against kcmp. Both copies of the address space read the original
 * bytes afterwards. */
static void mode_fork_undecided(void) {
  warm();
  chld_block();
  int status;
  long r = SITE(SYS_fork, 0, 0, 0, 0, 0);
  if (r == 0) {
    site_bytes("undecided fork child");
    _exit(7);
  }
  if (r < 0)
    die("fork");
  if (waitpid(r, &status, 0) != r)
    die("wait undecided fork");
  say("undecided fork child exited=%d code=%d\n", WIFEXITED(status), WEXITSTATUS(status));
  chld_unblock();
  site_bytes("undecided fork parent");
  warm();
}

/* T6a, undecided: a thread (CLONE_VM|CLONE_THREAD) through the patched
 * site, whose recorded clone flags the test makes disagree with kcmp (no
 * CLONE_VM). The thread returns from tp_site_fn on its own stack into
 * thread_entry and exits; the parent joins it without syscalls. */
static void mode_thread_mismatch(void) {
  warm();
  size_t size = 64 * 1024;
  char *stack = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (stack == MAP_FAILED)
    die("mmap stack");
  uintptr_t *top = (uintptr_t *)(stack + size - 64);
  top[0] = (uintptr_t)thread_entry;
  thread_ctid = 1;
  long flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD | CLONE_SYSVSEM |
               CLONE_CHILD_CLEARTID;
  long r = SITE(SYS_clone, flags, top, NULL, &thread_ctid, 0);
  report_result("mismatch thread", r > 0 ? 1 : r);
  while (__atomic_load_n(&thread_ctid, __ATOMIC_SEQ_CST) != 0)
    __builtin_ia32_pause();
  site_bytes("mismatch thread parent");
  warm();
}

/* T6d: posix_spawn and system() (both vfork-style) from a process with warm
 * sites; the parent's sites stay patched after the children exec. */
static void mode_vfork_spawn(void) {
  warm();
  /* Both children's SIGCHLDs coalesce into one delivery at the unblock.
   * system() saves and restores this mask around its own wait. */
  chld_block();
  pid_t child;
  char *args[] = {"/proc/self/exe", "exec_image", main_argv[2], NULL};
  if (posix_spawn(&child, args[0], NULL, NULL, args, environ) != 0)
    die("posix_spawn");
  int status;
  if (waitpid(child, &status, 0) != child)
    die("wait spawn");
  say("spawn child exited=%d code=%d\n", WIFEXITED(status), WEXITSTATUS(status));
  warm();
  status = system("exit 3");
  say("system exited=%d code=%d\n", WIFEXITED(status), WEXITSTATUS(status));
  chld_unblock();
  warm();
  say("parent getpid ok\n");
}

/* A filter: KILL_PROCESS for any arch other than x86_64, EPERM for getppid,
 * everything else allowed. */
static struct sock_filter guest_filter_code[] = {
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getppid, 0, 1),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
};
static struct sock_fprog guest_filter = {
    sizeof guest_filter_code / sizeof guest_filter_code[0],
    guest_filter_code,
};

static int tsync_pipe[2];
static long tsync_thread_getppid;

/* Test-only ordering knob: spins without syscalls (a few hundred
 * milliseconds) when TP_ORDER equals `when`, so that the other thread or
 * process almost surely finishes first. The runs must be equal either way. */
static void order_delay(const char *when) {
  const char *order = getenv("TP_ORDER");
  if (!order || strcmp(order, when))
    return;
  for (volatile long i = 0; i < 300L * 1000 * 1000; i++)
    ;
}

static void *tsync_thread_entry(void *arg) {
  (void)arg;
  char c;
  if (read(tsync_pipe[0], &c, 1) != 1)
    _exit(97);
  tsync_thread_getppid = SITE(SYS_getppid, 0, 0, 0, 0, 0);
  order_delay("late");
  return NULL;
}

/* T7a. how: "site" installs with seccomp() through the patched site (an I386
 * stop), "tsync" from a two-thread process through libc, "prctl" with
 * prctl(PR_SET_SECCOMP) through libc (x86_64 stops). */
static void guest_seccomp(const char *how) {
  install(SIGSYS, 0, handler);
  warm();
  if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
    die("no_new_privs");
  pthread_t thread;
  long r;
  if (!strcmp(how, "tsync")) {
    if (pipe(tsync_pipe) != 0)
      die("pipe");
    if (pthread_create(&thread, NULL, tsync_thread_entry, NULL) != 0)
      die("pthread_create");
    r = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, &guest_filter);
  } else if (!strcmp(how, "prctl")) {
    r = prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &guest_filter, 0, 0);
  } else {
    r = SITE(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &guest_filter, 0, 0);
  }
  say("install %s ret=%ld\n", how, r);
  site_bytes("after install");
  for (int i = 0; i < 3; i++)
    say("getpid %d pid=%d\n", i, SITE(SYS_getpid, 0, 0, 0, 0, 0) == getpid());
  say("getppid ret=%ld\n", SITE(SYS_getppid, 0, 0, 0, 0, 0));
  if (!strcmp(how, "tsync")) {
    if (write(tsync_pipe[1], "x", 1) != 1)
      die("write");
    /* Join without syscalls. pthread_join's futex wait is skipped, returns
     * 0, or returns EAGAIN depending on whether the thread's exit (the
     * kernel clearing its tid) came before the tid load, after the futex
     * call, or between them: host scheduling that changed the Tool-visible
     * stream. pthread_tryjoin_np returns EBUSY without a syscall while the
     * tid is set, and once it is clear it joins without a futex call. */
    order_delay("early");
    while (pthread_tryjoin_np(thread, NULL) == EBUSY)
      __builtin_ia32_pause();
    say("thread getppid ret=%ld\n", tsync_thread_getppid);
  }
  /* SIGCHLD stays blocked (and is inherited blocked) across the fork and the
   * wait, so its one delivery lands at the unblock below instead of wherever
   * the child's exit happens to overtake the parent's wait4. */
  sigset_t chld;
  sigemptyset(&chld);
  sigaddset(&chld, SIGCHLD);
  if (sigprocmask(SIG_BLOCK, &chld, NULL) != 0)
    die("block SIGCHLD");
  /* A fork child calls the site, then execs: the new image inherits the
   * filter, so its table must start disabled as well. */
  pid_t child = fork();
  if (child == 0) {
    for (int i = 0; i < 3; i++)
      say("child getpid %d pid=%d\n", i, SITE(SYS_getpid, 0, 0, 0, 0, 0) == getpid());
    order_delay("late");
    exec_self("exec_image");
  }
  order_delay("early");
  int status;
  if (waitpid(child, &status, 0) != child)
    die("wait child");
  say("child exited=%d code=%d signaled=%d sig=%d\n", WIFEXITED(status), WEXITSTATUS(status),
      WIFSIGNALED(status), WIFSIGNALED(status) ? WTERMSIG(status) : 0);
  if (sigprocmask(SIG_UNBLOCK, &chld, NULL) != 0)
    die("unblock SIGCHLD");
  say("sigsys handled=%d\n", nrec);
}

static volatile unsigned char sud_selector;
static volatile int sud_count, sud_syscall;
static volatile unsigned sud_arch;
static volatile long sud_call_addr;

static void sud_handler(int sig, siginfo_t *si, void *uc_) {
  ucontext_t *uc = uc_;
  (void)sig;
  sud_count++;
  sud_syscall = si->si_syscall;
  sud_arch = si->si_arch;
  sud_call_addr = (long)si->si_call_addr;
  uc->uc_mcontext.gregs[REG_RAX] = 1234;
  sud_selector = SYSCALL_DISPATCH_FILTER_ALLOW;
}

/* T7b: syscall user dispatch, with the allowed region excluding the site. */
static void mode_sud(void) {
  install(SIGSYS, 0, sud_handler);
  warm();
  sud_selector = SYSCALL_DISPATCH_FILTER_ALLOW;
  long r = prctl(PR_SET_SYSCALL_USER_DISPATCH, PR_SYS_DISPATCH_ON, (long)t8_fn,
                 (long)(t8_site_end - (char *)t8_fn), &sud_selector);
  say("sud on ret=%ld\n", r);
  site_bytes("after sud");
  sud_selector = SYSCALL_DISPATCH_FILTER_BLOCK;
  r = SITE(SYS_getpid, 0, 0, 0, 0, 0);
  sud_selector = SYSCALL_DISPATCH_FILTER_ALLOW;
  say("dispatched getpid ret=%ld count=%d syscall=%d arch=%#x call=%s\n", r, sud_count,
      sud_syscall, sud_arch, where(sud_call_addr));
  r = prctl(PR_SET_SYSCALL_USER_DISPATCH, PR_SYS_DISPATCH_OFF, 0, 0, 0);
  say("sud off ret=%ld\n", r);
  for (int i = 0; i < 3; i++)
    say("getpid %d pid=%d\n", i, SITE(SYS_getpid, 0, 0, 0, 0, 0) == getpid());
  chld_block();
  pid_t child = fork();
  if (child == 0) {
    for (int i = 0; i < 3; i++)
      say("child getpid %d pid=%d\n", i, SITE(SYS_getpid, 0, 0, 0, 0, 0) == getpid());
    _exit(0);
  }
  int status;
  if (waitpid(child, &status, 0) != child)
    die("wait child");
  say("child exited=%d code=%d\n", WIFEXITED(status), WEXITSTATUS(status));
  chld_unblock();
  say("sud handled=%d\n", sud_count);
}

static volatile long untraced_ret, untraced_rcx;
static volatile int untraced_done;

static void untraced_entry(void) {
  /* An untraced thread: every syscall it makes is refused by the inherited
   * filter (ENOSYS), so it records one call through the shared site and
   * then spins until exit_group ends it. */
  untraced_ret = SITE(SYS_getpid, 0, 0, 0, 0, 0);
  untraced_rcx = tp_nz_rcx;
  __atomic_store_n(&untraced_done, 1, __ATOMIC_SEQ_CST);
  for (;;)
    __asm__ volatile("pause");
}

/* A CLONE_UNTRACED thread gets no new-child stop, so the site must already
 * be restored when the clone runs. `through`: "libc" clones from libc's
 * syscall() (never patched), "site" through the patched site itself. */
static void untraced_thread(const char *through) {
  warm();
  size_t size = 64 * 1024;
  char *stack = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (stack == MAP_FAILED)
    die("mmap stack");
  uintptr_t *top = (uintptr_t *)(stack + size - 64);
  top[0] = (uintptr_t)untraced_entry;
  long flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD | CLONE_SYSVSEM |
               CLONE_UNTRACED;
  long r;
  if (!strcmp(through, "site"))
    r = SITE(SYS_clone, flags, top, NULL, NULL, 0);
  else
    r = syscall(SYS_clone, flags, top, NULL, NULL, 0);
  say("untraced clone ok=%d\n", r > 0);
  /* Spin without syscalls, so that the stop sequence is the same in every
   * run. */
  while (!__atomic_load_n(&untraced_done, __ATOMIC_SEQ_CST))
    __asm__ volatile("pause");
  say("untraced thread getpid ret=%ld rcx=%s\n", untraced_ret, where(untraced_rcx));
  site_bytes("after untraced");
}

/* What the untraced fork child saw, in memory shared across the fork. */
struct untraced_fork_view {
  long clone_rcx, clone_r11, ret, rcx;
  unsigned char bytes[2];
  int done;
};

/* A fork-like CLONE_UNTRACED child (no CLONE_VM), cloned through the patched
 * site: it gets no new-child stop and its own copy of the parent's text, so
 * the site must already be restored when the clone runs, and the clone must
 * run at the site. The child's syscalls are refused by the inherited filter
 * (ENOSYS: no tracer), so it reports through a shared mapping and spins
 * until the parent kills it. */
static void untraced_fork(void) {
  warm();
  struct untraced_fork_view *view =
      mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
  if (view == MAP_FAILED)
    die("mmap shared");
  chld_block();
  long r = SITE(SYS_clone, CLONE_UNTRACED | SIGCHLD, NULL, NULL, NULL, 0);
  if (r == 0) {
    view->clone_rcx = tp_z_rcx;
    view->clone_r11 = tp_z_r11;
    view->ret = SITE(SYS_getpid, 0, 0, 0, 0, 0);
    view->rcx = tp_nz_rcx;
    view->bytes[0] = ((unsigned char *)tp_site)[0];
    view->bytes[1] = ((unsigned char *)tp_site)[1];
    __atomic_store_n(&view->done, 1, __ATOMIC_SEQ_CST);
    for (;;)
      __builtin_ia32_pause();
  }
  if (r < 0)
    die("clone untraced");
  say("untraced fork clone ok=1\n");
  /* Spin without syscalls, so that the stop sequence is the same in every
   * run; bounded (a few seconds) for a child that never gets that far. */
  for (long spins = 0; !__atomic_load_n(&view->done, __ATOMIC_SEQ_CST) && spins < (1L << 28);
       spins++)
    __builtin_ia32_pause();
  say("untraced fork child done=%d clone rcx=%s r11=%#lx getpid ret=%ld rcx=%s bytes %02x %02x\n",
      __atomic_load_n(&view->done, __ATOMIC_SEQ_CST), where(view->clone_rcx), view->clone_r11,
      view->ret, where(view->rcx), view->bytes[0], view->bytes[1]);
  if (kill(r, SIGKILL) != 0)
    die("kill untraced");
  siginfo_t info;
  memset(&info, 0, sizeof info);
  if (waitid(P_PID, r, &info, WEXITED) != 0)
    die("waitid untraced");
  say("untraced fork child code=%d status=%d\n", info.si_code, info.si_status);
  chld_unblock();
  site_bytes("after untraced fork");
}

int main(int argc, char **argv) {
  if (argc != 3) {
    fprintf(stderr, "usage: %s <mode> <report>\n", argv[0]);
    return 2;
  }
  report_fd = open(argv[2], O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0644);
  if (report_fd < 0)
    return 3;
  main_argv = argv;
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
  else if (!strcmp(m, "exec_image"))
    mode_exec_image();
  else if (!strcmp(m, "exec_leader"))
    mode_exec_leader();
  else if (!strcmp(m, "exec_thread"))
    mode_exec_thread();
  else if (!strcmp(m, "jit"))
    mode_jit();
  else if (!strcmp(m, "jit_more"))
    mode_jit_more();
  else if (!strcmp(m, "fork_undecided"))
    mode_fork_undecided();
  else if (!strcmp(m, "thread_mismatch"))
    mode_thread_mismatch();
  else if (!strcmp(m, "vfork_spawn"))
    mode_vfork_spawn();
  else if (!strcmp(m, "guest_seccomp"))
    guest_seccomp("site");
  else if (!strcmp(m, "guest_seccomp_tsync"))
    guest_seccomp("tsync");
  else if (!strcmp(m, "guest_seccomp_prctl"))
    guest_seccomp("prctl");
  else if (!strcmp(m, "sud"))
    mode_sud();
  else if (!strcmp(m, "untraced_thread"))
    untraced_thread("libc");
  else if (!strcmp(m, "untraced_thread_site"))
    untraced_thread("site");
  else if (!strcmp(m, "untraced_fork"))
    untraced_fork();
  else
    return 4;
  say("done\n");
  return 0;
}
