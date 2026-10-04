/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/*
 * Guest for the plain ptrace regression tests
 * (reverie-ptrace/src/plain_guest_regression_tests.rs).
 *
 * Usage: plain_guest_regression <mode> <report-file>
 *
 * Every observation is appended to the report file as text. Absolute
 * addresses are comparable because the tracer disables address
 * randomisation. PID values are never printed.
 *
 * `tp_site` is a shared generic syscall site: `tp_site_fn(nr, a1..a6)` issues
 * one `syscall` there. A call whose sixth argument (r9) carries TP_MAGIC asks
 * the test Tool for a handler shape or for tracer-side signals (see the Rust
 * side).
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <sched.h>
#include <signal.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>

#define TP_MAGIC 0x7e57000000000000L
/* Handler shapes (low byte), decoded by the test Tool. */
#define SHAPE_INJECT 0
#define SHAPE_TAIL 1

long tp_site_fn(long nr, long a1, long a2, long a3, long a4, long a5, long a6);
extern char tp_site[], tp_site_end[];
/* rcx and r11 right after `syscall` at tp_site, split by whether the call
 * returned zero (a new child) or not (a parent or any other result). */
long tp_nz_rcx, tp_nz_r11, tp_z_rcx, tp_z_r11;

__asm__(
    ".pushsection .text\n"
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

/* A second site, whose function is syscall user dispatch's allowed region
 * (so that tp_site lies outside it). rdi = nr. */
long t8_fn(long nr);
extern char t8_site[], t8_site_end[];
__asm__(
    ".pushsection .text\n"
    ".globl t8_fn\n"
    ".type t8_fn, @function\n"
    "t8_fn:\n"
    "  mov %rdi, %rax\n"
    ".globl t8_site\n"
    "t8_site:\n"
    "  syscall\n"
    ".globl t8_site_end\n"
    "t8_site_end:\n"
    "  ret\n"
    ".size t8_fn, .-t8_fn\n"
    ".popsection\n");

/* Five more generic sites, `tp_genN_fn(nr)`, one per unknown-number case. */
long tp_gen_rcx, tp_gen_r11;
#define GEN_SITE(n)                \
  ".globl tp_gen" #n               \
  "_fn\n"                          \
  ".type tp_gen" #n                \
  "_fn, @function\n"               \
  "tp_gen" #n                      \
  "_fn:\n"                         \
  "  mov %rdi, %rax\n"             \
  "  cmp %rax, %rax\n"             \
  ".globl tp_gen" #n               \
  "\n"                             \
  "tp_gen" #n                      \
  ":\n"                            \
  "  syscall\n"                    \
  "  mov %rcx, tp_gen_rcx(%rip)\n" \
  "  mov %r11, tp_gen_r11(%rip)\n" \
  "  ret\n"                        \
  ".size tp_gen" #n "_fn, .-tp_gen" #n "_fn\n"
long tp_gen0_fn(long nr), tp_gen1_fn(long nr), tp_gen2_fn(long nr),
    tp_gen3_fn(long nr), tp_gen4_fn(long nr);
extern char tp_gen0[], tp_gen1[], tp_gen2[], tp_gen3[], tp_gen4[];
__asm__(".pushsection .text\n" GEN_SITE(0) GEN_SITE(1) GEN_SITE(2) GEN_SITE(3)
            GEN_SITE(4) ".popsection\n");

#define SITE(nr, a1, a2, a3, a4, a5) \
  tp_site_fn(                        \
      (long)(nr),                    \
      (long)(a1),                    \
      (long)(a2),                    \
      (long)(a3),                    \
      (long)(a4),                    \
      (long)(a5),                    \
      0)
/* r9 carries TP_MAGIC, a sequence number (bits 16-47) and the action. The
 * sequence number lets the tool act once per call even when the kernel
 * restarts it. */
static long tp_seq;
#define SITEM(nr, a1, a2, a3, a4, a5, act) \
  tp_site_fn(                              \
      (long)(nr),                          \
      (long)(a1),                          \
      (long)(a2),                          \
      (long)(a3),                          \
      (long)(a4),                          \
      (long)(a5),                          \
      TP_MAGIC | ((++tp_seq) << 16) | (act))

static int report_fd = -1;

static void say(const char* fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  vdprintf(report_fd, fmt, ap);
  va_end(ap);
}

static void die(const char* what) {
  say("FATAL %s errno=%d\n", what, errno);
  _exit(99);
}

static const char* where(long rip) {
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

/* Runs getpid through tp_site three times. */
static void warm(void) {
  for (int i = 0; i < 3; i++) {
    if (SITE(SYS_getpid, 0, 0, 0, 0, 0) != getpid())
      die("warm getpid");
  }
}

static void install(int sig, int flags, void (*fn)(int, siginfo_t*, void*)) {
  struct sigaction sa;
  memset(&sa, 0, sizeof sa);
  sa.sa_sigaction = fn;
  sa.sa_flags = SA_SIGINFO | flags;
  sigemptyset(&sa.sa_mask);
  if (sigaction(sig, &sa, NULL) != 0)
    die("sigaction");
}

static void site_bytes(const char* tag) {
  unsigned char* p = (unsigned char*)tp_site;
  say("%s site bytes %02x %02x\n", tag, p[0], p[1]);
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

/* The guest's own SECCOMP_RET_TRACE for number 500, which no syscall table
 * knows; everything else allowed. */
static struct sock_filter trace_unknown_code[] = {
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 500, 0, 1),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRACE),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
};
static struct sock_fprog trace_unknown_filter = {
    sizeof trace_unknown_code / sizeof trace_unknown_code[0],
    trace_unknown_code,
};

/* A ptrace-stop for a number the tracer cannot decode. The tracer's own
 * filter does not trace 500; the guest's filter does. */
static void mode_guest_trace_unknown(void) {
  if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
    die("no_new_privs");
  long r =
      prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &trace_unknown_filter, 0, 0);
  say("install ret=%ld\n", r);
  say("unknown ret=%ld\n", syscall(500, 0, 0, 0, 0, 0, 0));
}

static volatile unsigned char sud_selector;
static volatile int sud_count, sud_syscall;
static volatile unsigned sud_arch;
static volatile long sud_call_addr;

static void sud_handler(int sig, siginfo_t* si, void* uc_) {
  ucontext_t* uc = uc_;
  (void)sig;
  sud_count++;
  sud_syscall = si->si_syscall;
  sud_arch = si->si_arch;
  sud_call_addr = (long)si->si_call_addr;
  uc->uc_mcontext.gregs[REG_RAX] = 1234;
  sud_selector = SYSCALL_DISPATCH_FILTER_ALLOW;
}

/* Syscall user dispatch, with the allowed region excluding the site. */
static void mode_sud(void) {
  install(SIGSYS, 0, sud_handler);
  warm();
  sud_selector = SYSCALL_DISPATCH_FILTER_ALLOW;
  long r = prctl(
      PR_SET_SYSCALL_USER_DISPATCH,
      PR_SYS_DISPATCH_ON,
      (long)t8_fn,
      (long)(t8_site_end - (char*)t8_fn),
      &sud_selector);
  say("sud on ret=%ld\n", r);
  site_bytes("after sud");
  sud_selector = SYSCALL_DISPATCH_FILTER_BLOCK;
  r = SITE(SYS_getpid, 0, 0, 0, 0, 0);
  sud_selector = SYSCALL_DISPATCH_FILTER_ALLOW;
  say("dispatched getpid ret=%ld count=%d syscall=%d arch=%#x call=%s\n",
      r,
      sud_count,
      sud_syscall,
      sud_arch,
      where(sud_call_addr));
  r = prctl(PR_SET_SYSCALL_USER_DISPATCH, PR_SYS_DISPATCH_OFF, 0, 0, 0);
  say("sud off ret=%ld\n", r);
  for (int i = 0; i < 3; i++)
    say("getpid %d pid=%d\n", i, SITE(SYS_getpid, 0, 0, 0, 0, 0) == getpid());
  chld_block();
  pid_t child = fork();
  if (child == 0) {
    for (int i = 0; i < 3; i++)
      say("child getpid %d pid=%d\n",
          i,
          SITE(SYS_getpid, 0, 0, 0, 0, 0) == getpid());
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

/* A CLONE_UNTRACED thread gets no new-child stop. `through`: "libc" clones
 * from libc's syscall(), "site" through the shared site itself. */
static void untraced_thread(const char* through) {
  warm();
  size_t size = 64 * 1024;
  char* stack = mmap(
      NULL, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (stack == MAP_FAILED)
    die("mmap stack");
  uintptr_t* top = (uintptr_t*)(stack + size - 64);
  top[0] = (uintptr_t)untraced_entry;
  long flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND |
      CLONE_THREAD | CLONE_SYSVSEM | CLONE_UNTRACED;
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
  say("untraced thread getpid ret=%ld rcx=%s\n",
      untraced_ret,
      where(untraced_rcx));
  site_bytes("after untraced");
}

/* What the untraced fork child saw, in memory shared across the fork. */
struct untraced_fork_view {
  long clone_rcx, clone_r11, ret, rcx;
  unsigned char bytes[2];
  int done;
};

/* A fork-like CLONE_UNTRACED child (no CLONE_VM), cloned through the shared
 * site: it gets no new-child stop and its own copy of the parent's text, and
 * starts right after the parent's `syscall`. The child's syscalls are
 * refused by the inherited filter (ENOSYS: no tracer), so it reports through
 * a shared mapping and spins until the parent kills it. */
static void untraced_fork(void) {
  warm();
  struct untraced_fork_view* view = mmap(
      NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
  if (view == MAP_FAILED)
    die("mmap shared");
  chld_block();
  long r = SITE(SYS_clone, CLONE_UNTRACED | SIGCHLD, NULL, NULL, NULL, 0);
  if (r == 0) {
    view->clone_rcx = tp_z_rcx;
    view->clone_r11 = tp_z_r11;
    view->ret = SITE(SYS_getpid, 0, 0, 0, 0, 0);
    view->rcx = tp_nz_rcx;
    view->bytes[0] = ((unsigned char*)tp_site)[0];
    view->bytes[1] = ((unsigned char*)tp_site)[1];
    __atomic_store_n(&view->done, 1, __ATOMIC_SEQ_CST);
    for (;;)
      __builtin_ia32_pause();
  }
  if (r < 0)
    die("clone untraced");
  say("untraced fork clone ok=1\n");
  /* Spin without syscalls, so that the stop sequence is the same in every
   * run; bounded (a few seconds) for a child that never gets that far. */
  for (long spins = 0;
       !__atomic_load_n(&view->done, __ATOMIC_SEQ_CST) && spins < (1L << 28);
       spins++)
    __builtin_ia32_pause();
  say("untraced fork child done=%d clone rcx=%s r11=%#lx getpid ret=%ld rcx=%s bytes %02x %02x\n",
      __atomic_load_n(&view->done, __ATOMIC_SEQ_CST),
      where(view->clone_rcx),
      view->clone_r11,
      view->ret,
      where(view->rcx),
      view->bytes[0],
      view->bytes[1]);
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

/* A traced parent signals its child while the child's call at the shared
 * site is parked. The Tool (at the site's seccomp stop) and the tracer's
 * pre-syscall hook (immediately before the call runs: the final resume of
 * the stop, or the exact inject's resume) each notify the parent with
 * SIGUSR2 and wait until the signal the call's tag names has arrived, so the
 * signal lands at a fixed logical point. */
#define TOOL_PARK(sig) ((long)(sig) << 32)
#define HOOK_PARK(sig) ((long)(sig) << 40)
/* The hook parks at its late point. */
#define HOOK_LATE (0x20L << 40)
/* park_run: the child handles SIGTSTP, SIGTTIN and SIGTTOU (else SIG_DFL). */
#define PARK_TSTP_HANDLER 4
/* A park_run `kills` entry: the parent sends SIGCONT to the process, then
 * `sig` (a positive, process-directed entry). */
#define CONT_THEN(sig) (0x100 | (sig))

struct park_rec {
  int sig, code, from_parent;
  long rip, rax;
};
static struct park_rec park_recs[8];
static volatile int npark;

static void park_handler(int sig, siginfo_t* si, void* uc_) {
  ucontext_t* uc = uc_;
  if (npark >= 8)
    return;
  struct park_rec* r = &park_recs[npark++];
  r->sig = sig;
  r->code = si->si_code;
  r->from_parent = si->si_code <= 0 && si->si_pid == getppid();
  r->rip = uc->uc_mcontext.gregs[REG_RIP];
  r->rax = uc->uc_mcontext.gregs[REG_RAX];
}

static void park_child(const char* tag, long act, int how, int from_parent,
                       int to_parent) {
  install(SIGCONT, 0, park_handler);
  if (how & PARK_TSTP_HANDLER) {
    install(SIGTSTP, 0, park_handler);
    install(SIGTTIN, 0, park_handler);
    install(SIGTTOU, 0, park_handler);
  }
  sigset_t mask;
  sigemptyset(&mask);
  if (sigprocmask(SIG_SETMASK, &mask, NULL) != 0)
    die("child sigprocmask");
  long r = SITEM(SYS_write, to_parent, "data", 4, 0, 0, act);
  if (write(to_parent, "after", 5) != 5)
    die("child write after");
  char token;
  if (read(from_parent, &token, 1) != 1)
    die("child read token");
  sigemptyset(&mask);
  if (sigprocmask(SIG_SETMASK, &mask, NULL) != 0)
    die("child unblock");
  say("%s child call ret=%ld byte=%d\n", tag, r, 0);
  for (int i = 0; i < npark; i++) {
    struct park_rec* p = &park_recs[i];
    say("%s child signal %d: sig=%d code=%d from-parent=%d rip=%s rax=%ld\n",
        tag,
        i,
        p->sig,
        p->code,
        p->from_parent,
        where(p->rip),
        p->rax);
  }
  _exit(7);
}

/* Waits up to `seconds` for the next notification; 1 if it came. */
static int park_notified(int seconds) {
  sigset_t usr2;
  sigemptyset(&usr2);
  sigaddset(&usr2, SIGUSR2);
  struct timespec timeout = {seconds, 0};
  return sigtimedwait(&usr2, NULL, &timeout) == SIGUSR2;
}

/* Forks a child that runs one tagged write at the shared site; the parent
 * sends `kills` (one per notification), then reports what its
 * waitpid(WUNTRACED | WCONTINUED) and SIGCHLD saw. */
static void
park_run(const char* tag, long act, int how, const int* kills, int nkills) {
  int down[2], up[2];
  if (pipe(down) != 0 || pipe(up) != 0)
    die("pipe");
  sigset_t block, old;
  sigemptyset(&block);
  sigaddset(&block, SIGUSR2);
  sigaddset(&block, SIGCHLD);
  if (sigprocmask(SIG_BLOCK, &block, &old) != 0)
    die("sigprocmask");
  pid_t child = fork();
  if (child < 0)
    die("fork");
  if (child == 0) {
    close(down[1]);
    close(up[0]);
    park_child(tag, act, how, down[0], up[1]);
  }
  close(down[0]);
  close(up[1]);
  int notified = 0;
  for (int i = 0; i < nkills; i++) {
    if (!park_notified(10))
      break;
    notified++;
    int sig = kills[i] & 0xff;
    if ((kills[i] & CONT_THEN(0)) && kill(child, SIGCONT) != 0)
      die("kill SIGCONT");
    if (kill(child, sig) != 0)
      die("kill");
  }
  char buf[8] = {0};
  /* The write is never restarted here, so `restarted` stays -1. */
  int restarted = -1;
  long data = read(up[0], buf, 4);
  long after = read(up[0], buf, 5);
  if (after == 5) {
    if (write(down[1], "t", 1) != 1)
      die("write token");
  }
  char statuses[256] = "";
  for (;;) {
    int status;
    pid_t w = waitpid(child, &status, WUNTRACED | WCONTINUED);
    if (w != child)
      die("waitpid");
    char one[64];
    if (WIFEXITED(status))
      snprintf(one, sizeof one, " exited:%d", WEXITSTATUS(status));
    else if (WIFSIGNALED(status))
      snprintf(one, sizeof one, " signaled:%d", WTERMSIG(status));
    else if (WIFSTOPPED(status))
      snprintf(one, sizeof one, " stopped:%d", WSTOPSIG(status));
    else if (WIFCONTINUED(status))
      snprintf(one, sizeof one, " continued");
    else
      snprintf(one, sizeof one, " raw:%#x", status);
    strncat(statuses, one, sizeof statuses - strlen(statuses) - 1);
    if (WIFEXITED(status) || WIFSIGNALED(status))
      break;
  }
  say("%s parent notified=%d data=%ld after=%ld restarted=%d waitpid%s\n",
      tag,
      notified,
      data,
      after,
      restarted,
      statuses);
  sigset_t chld;
  sigemptyset(&chld);
  sigaddset(&chld, SIGCHLD);
  struct timespec none = {0, 0};
  siginfo_t si;
  while (sigtimedwait(&chld, &si, &none) == SIGCHLD)
    say("%s parent SIGCHLD code=%d status=%d from-child=%d\n",
        tag,
        si.si_code,
        si.si_status,
        si.si_pid == child);
  close(down[1]);
  close(up[0]);
  /* Drop any notification that arrived after the parent stopped waiting. */
  while (park_notified(0))
    ;
  if (sigprocmask(SIG_SETMASK, &old, NULL) != 0)
    die("sigprocmask restore");
}

/* A SIGSTOP sent at the write's seccomp stop, then a SIGCONT sent at the
 * hook's late point, immediately before the write runs. The SIGCONT discards
 * the pending SIGSTOP, so neither is a stop. */
static void mode_sigstop_cont_late(void) {
  static const int kills[] = {SIGSTOP, SIGCONT};
  warm();
  park_run(
      "tail",
      SHAPE_TAIL | TOOL_PARK(SIGSTOP) | HOOK_PARK(SIGCONT) | HOOK_LATE,
      0,
      kills,
      2);
  park_run(
      "inject",
      SHAPE_INJECT | TOOL_PARK(SIGSTOP) | HOOK_PARK(SIGCONT) | HOOK_LATE,
      0,
      kills,
      2);
}

/* A SIGSTOP sent at the write's seccomp stop, then a SIGCONT and the stop
 * signal `sig` (handled) at the hook: `sig` discards the SIGCONT, which
 * discarded the SIGSTOP, so only `sig` is delivered, after the write. */
static void sigstop_cont_then(const char* tag, int sig) {
  const int kills[] = {SIGSTOP, CONT_THEN(sig)};
  warm();
  park_run(
      tag,
      SHAPE_TAIL | TOOL_PARK(SIGSTOP) | HOOK_PARK(sig),
      PARK_TSTP_HANDLER,
      kills,
      2);
}

/* The unknown-number cases: each runs through its own generic site. */
static void mode_unknown(void) {
  static const struct {
    const char* name;
    long nr;
    int prints_bytes;
  } cases[] = {
      {"nr500", 500, 1},
      {"nr-1", -1, 1},
      /* 337, not 335 or 336: x86_64 335 (uretprobe) and 336 (uprobe) are
       * passed through by seccomp without running the filter, by upstream
       * design. */
      {"gap337", 337, 1},
      {"high-getpid", 0x100000027L, 0},
      {"high500", 0x1000001f4L, 1},
  };
  long (*fns[])(long) = {
      tp_gen0_fn, tp_gen1_fn, tp_gen2_fn, tp_gen3_fn, tp_gen4_fn};
  char* sites[] = {tp_gen0, tp_gen1, tp_gen2, tp_gen3, tp_gen4};
  long pid = getpid();
  for (unsigned i = 0; i < sizeof cases / sizeof cases[0]; i++) {
    for (int j = 0; j < 3; j++)
      if (fns[i](SYS_getpid) != pid)
        die("warm generic site");
    long r = fns[i](cases[i].nr);
    char ret[32];
    if (r == pid)
      snprintf(ret, sizeof ret, "<pid>");
    else
      snprintf(ret, sizeof ret, "%ld", r);
    say("%s ret=%s rcx-next=%d r11=%#lx\n",
        cases[i].name,
        ret,
        tp_gen_rcx == (long)sites[i] + 2,
        tp_gen_r11);
    say("%s getpid after=%d\n", cases[i].name, fns[i](SYS_getpid) == pid);
    if (cases[i].prints_bytes) {
      unsigned char* p = (unsigned char*)sites[i];
      say("%s bytes after %02x %02x\n", cases[i].name, p[0], p[1]);
    }
  }
}

int main(int argc, char** argv) {
  if (argc != 3) {
    fprintf(stderr, "usage: %s <mode> <report>\n", argv[0]);
    return 2;
  }
  report_fd = open(argv[2], O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0644);
  if (report_fd < 0)
    return 3;
  say("mode %s site=%#lx\n", argv[1], (long)tp_site);
  const char* m = argv[1];
  if (!strcmp(m, "guest_trace_unknown"))
    mode_guest_trace_unknown();
  else if (!strcmp(m, "sud"))
    mode_sud();
  else if (!strcmp(m, "untraced_thread"))
    untraced_thread("libc");
  else if (!strcmp(m, "untraced_thread_site"))
    untraced_thread("site");
  else if (!strcmp(m, "untraced_fork"))
    untraced_fork();
  else if (!strcmp(m, "sigstop_cont_late"))
    mode_sigstop_cont_late();
  else if (!strcmp(m, "sigstop_cont_tstp"))
    sigstop_cont_then("tstp", SIGTSTP);
  else if (!strcmp(m, "sigstop_cont_ttin"))
    sigstop_cont_then("ttin", SIGTTIN);
  else if (!strcmp(m, "sigstop_cont_ttou"))
    sigstop_cont_then("ttou", SIGTTOU);
  else if (!strcmp(m, "unknown"))
    mode_unknown();
  else
    return 4;
  say("done\n");
  return 0;
}
