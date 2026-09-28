#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <unistd.h>

/* The syscall number of `reverie_liteinst_restorer`'s site. */
long reverie_liteinst_restorer_nr;

/* Two syscall sites, which the first use of each, a getpid, patches. The
   first makes getpid with its arguments in rdi, which tells the test's Tool
   whether to request a timer, and rsi, the round the request is for. The
   second makes the syscall numbered by
   `reverie_liteinst_restorer_nr`, and becomes the signal handler's
   restorer, whose rt_sigreturn is then a LiteInst hook trap. */
__asm__(".text\n"
        ".p2align 4\n"
        ".global reverie_liteinst_getpid\n"
        ".type reverie_liteinst_getpid,@function\n"
        "reverie_liteinst_getpid:\n"
        "mov $39, %eax\n"
        "syscall\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "ret\n"
        ".size reverie_liteinst_getpid, .-reverie_liteinst_getpid\n"
        ".p2align 4\n"
        ".global reverie_liteinst_restorer\n"
        ".type reverie_liteinst_restorer,@function\n"
        "reverie_liteinst_restorer:\n"
        "mov reverie_liteinst_restorer_nr(%rip), %rax\n"
        "syscall\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "ret\n"
        ".size reverie_liteinst_restorer, .-reverie_liteinst_restorer\n");

extern long reverie_liteinst_getpid(long request, unsigned long round);
extern long reverie_liteinst_restorer(void);

/* Retires exactly `rounds` conditional branches, for `rounds` > 0. */
static void branches(unsigned long rounds) {
  __asm__ volatile("1: dec %0; jnz 1b" : "+r"(rounds) : : "cc");
}

/* The value that the test's Tool gives every getpid. */
#define ANSWER 0x4242

/* The kernel's flag for a caller-supplied restorer, which glibc does not
   export. */
#define SA_RESTORER 0x04000000

/* The kernel's `struct sigaction` for x86-64 rt_sigaction. */
struct kernel_sigaction {
  void (*handler)(int);
  unsigned long flags;
  void (*restorer)(void);
  unsigned long mask;
};

static unsigned long before;
static unsigned long leads = 1;
static unsigned long stride;
static volatile unsigned long round_index;
static volatile unsigned long wrong;
static volatile unsigned long handled;

/* Requests the timer for the current round `i`, and returns
   `before + (i % leads) * stride` branches later. */
static void handler(int signal) {
  (void)signal;
  unsigned long i = round_index;
  unsigned long lead = before + (i % leads) * stride;
  long pid = reverie_liteinst_getpid(1, i);
  branches(lead);
  wrong += pid != ANSWER;
  ++handled;
}

/* Arguments: the branches from the handler's getpid to its return, the
   number of signals, and the branches after each; optionally then `leads`
   and `stride`, to add `(i % leads) * stride` branches before round `i`'s
   return, and then a flag that, if nonzero, blocks the timer's signal,
   SIGSTKFLT, for the whole run, so that no notification delivers an event. */
int main(int argc, char **argv) {
  if (argc != 4 && argc != 6 && argc != 7) {
    return 2;
  }
  before = strtoul(argv[1], NULL, 0);
  unsigned long rounds = strtoul(argv[2], NULL, 0);
  unsigned long after = strtoul(argv[3], NULL, 0);
  if (argc >= 6) {
    leads = strtoul(argv[4], NULL, 0);
    stride = strtoul(argv[5], NULL, 0);
    if (leads == 0) {
      return 2;
    }
  }
  wrong += reverie_liteinst_getpid(0, 0) != ANSWER;
  reverie_liteinst_restorer_nr = SYS_getpid;
  wrong += reverie_liteinst_restorer() != ANSWER;
  reverie_liteinst_restorer_nr = SYS_rt_sigreturn;
  struct kernel_sigaction action = {
      .handler = handler,
      .flags = SA_RESTORER,
      .restorer = (void (*)(void))reverie_liteinst_restorer,
      .mask = 0,
  };
  if (syscall(SYS_rt_sigaction, SIGUSR1, &action, NULL, sizeof(action.mask))) {
    return 3;
  }
  if (argc == 7 && strtoul(argv[6], NULL, 0) != 0) {
    sigset_t timer;
    sigemptyset(&timer);
    sigaddset(&timer, SIGSTKFLT);
    if (sigprocmask(SIG_BLOCK, &timer, NULL)) {
      return 5;
    }
  }
  /* The Tool answers getpid, so address the signal by thread alone. */
  pid_t tid = gettid();
  for (unsigned long i = 0; i < rounds; ++i) {
    round_index = i;
    if (syscall(SYS_tkill, tid, SIGUSR1)) {
      return 4;
    }
    branches(after);
  }
  printf("rounds=%lu handled=%lu wrong=%lu\n", rounds, handled, wrong);
  return 0;
}
