#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/signalfd.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <ucontext.h>
static volatile sig_atomic_t handled, bad;
static char alternate[32768], changed_alternate[32768];
static char *expected_alternate = alternate;
static int expected_blocked;
static void handler(int sig, siginfo_t *info, void *ctx) {
  char here;
  ucontext_t *context = ctx;
  if (sig != SIGALRM || info->si_signo != SIGALRM || info->si_code != SI_KERNEL ||
      (uintptr_t)&here < (uintptr_t)expected_alternate || (uintptr_t)&here >= (uintptr_t)(expected_alternate + sizeof alternate)) bad = 1;
  sigset_t mask; if (sigprocmask(SIG_SETMASK, NULL, &mask) || sigismember(&mask, SIGALRM) != expected_blocked) bad = 2;
  if (context->uc_stack.ss_sp != expected_alternate ||
      sigismember(&context->uc_sigmask, SIGALRM) != expected_blocked) bad = 3;
  handled++;
}
int main(int argc, char **argv) {
  if (argc != 2) return 1;
  int mode = atoi(argv[1]);
  if (mode == 16) { expected_alternate = changed_alternate; expected_blocked = 1; }
  stack_t changed = {.ss_sp=changed_alternate, .ss_size=sizeof changed_alternate};
  stack_t alt = {.ss_sp=alternate, .ss_size=sizeof alternate};
  if (sigaltstack(&alt, NULL)) return 2;
  struct sigaction action = {.sa_sigaction=handler, .sa_flags=SA_SIGINFO|SA_ONSTACK|SA_NODEFER|SA_RESTART};
  sigemptyset(&action.sa_mask);
  if (mode == 0) action.sa_handler = SIG_IGN;
  if (mode == 4) action.sa_handler = SIG_DFL;
  if (sigaction(SIGALRM, &action, NULL)) return 3;
  sigset_t set; sigemptyset(&set); sigaddset(&set, SIGALRM);
  int fd = -1;
  if ((mode >= 6 && mode <= 10) || mode == 13) {
    if (sigprocmask(SIG_BLOCK, &set, NULL)) return 4;
    fd = signalfd(-1, &set, SFD_NONBLOCK); if (fd < 0) return 5;
  }
  errno = 0;
  long result = syscall(SYS_getpid, 0x7061726b, mode, &set, fd, &changed);
  if (mode == 8 || mode == 9 || mode == 13) {
    if (result != 123) return 6;
    errno = 0;
    result = (mode == 8 || mode == 13) ? syscall(SYS_read, fd, (void*)1, 128) : syscall(SYS_rt_sigtimedwait, &set, (void*)1, NULL, 8);
    if (result != -1 || errno != EFAULT) return 7;
    result = 123;
  }
  if (mode == 2 || mode == 3 || mode == 12 || mode == 16) {
    if (result != -1 || errno != (mode == 12 ? EFAULT : EINTR) || handled != 1 || bad) return 8;
    if (syscall(SYS_getpid) != 1) return 9;
    sigset_t restored; stack_t restored_stack;
    if (sigprocmask(SIG_SETMASK, NULL, &restored) ||
        sigismember(&restored, SIGALRM) != expected_blocked ||
        sigaltstack(NULL, &restored_stack) ||
        restored_stack.ss_sp != expected_alternate ||
        (restored_stack.ss_flags & SS_ONSTACK)) return 13;
  } else if (mode == 4 || mode == 5 || mode == 10 || mode == 11 || mode == 13) return 10;
  else if (result != 123 || handled || bad) return 11;
  sigset_t pending; if (sigpending(&pending) || sigismember(&pending, SIGALRM)) return 12;
  puts("parked-signal-checked");
  return 0;
}
