#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <unistd.h>

/* The syscall number of `reverie_liteinst_restorer`'s site. */
long reverie_liteinst_restorer_nr;

/* Two syscall sites, which the first use of each, a getpid, patches. The
   first makes getpid with its argument in rdi, which tells the test's Tool
   whether to request a timer. The second makes the syscall numbered by
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

extern long reverie_liteinst_getpid(long request);
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
static volatile unsigned long wrong;
static volatile unsigned long handled;

/* Requests the timer, and returns `before` branches later. */
static void handler(int signal) {
  (void)signal;
  long pid = reverie_liteinst_getpid(1);
  branches(before);
  wrong += pid != ANSWER;
  ++handled;
}

/* Arguments: the branches from the handler's getpid to its return, the
   number of signals, and the branches after each. */
int main(int argc, char **argv) {
  if (argc != 4) {
    return 2;
  }
  before = strtoul(argv[1], NULL, 0);
  unsigned long rounds = strtoul(argv[2], NULL, 0);
  unsigned long after = strtoul(argv[3], NULL, 0);
  wrong += reverie_liteinst_getpid(0) != ANSWER;
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
  /* The Tool answers getpid, so address the signal by thread alone. */
  pid_t tid = gettid();
  for (unsigned long i = 0; i < rounds; ++i) {
    if (syscall(SYS_tkill, tid, SIGUSR1)) {
      return 4;
    }
    branches(after);
  }
  printf("rounds=%lu handled=%lu wrong=%lu\n", rounds, handled, wrong);
  return 0;
}
