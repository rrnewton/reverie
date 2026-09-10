#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/ucontext.h>
#include <unistd.h>

#define XSAVE_SIGNAL_SIZE 836
#define XMM15_OFFSET (160 + 15 * 16)

static int mode;
static _Alignas(64) unsigned char alternate_fpstate[XSAVE_SIGNAL_SIZE];

static void handler(int signo, siginfo_t *info, void *raw_context) {
  (void)info;
  if (signo != SIGUSR1) _exit(40);
  ucontext_t *context = (ucontext_t *)raw_context;
  context->uc_mcontext.gregs[REG_RAX] = 0x5a;
  if (mode == 1) {
    context->uc_mcontext.fpregs = 0;
    return;
  }
  if (mode == 3) {
    context->uc_mcontext.fpregs = (void *)0x1000;
    return;
  }
  memcpy(alternate_fpstate, context->uc_mcontext.fpregs, XSAVE_SIGNAL_SIZE);
  for (unsigned i = 0; i < 16; ++i) {
    alternate_fpstate[XMM15_OFFSET + i] = (unsigned char)(0xa0 + i);
  }
  context->uc_mcontext.fpregs = (void *)alternate_fpstate;
}

int main(int argc, char **argv) {
  if (argc != 2) return 41;
  mode = argv[1][0] == 'n' ? 1 : argv[1][0] == 'i' ? 3 : 2;
  struct sigaction action = {0};
  action.sa_sigaction = handler;
  action.sa_flags = SA_SIGINFO;
  sigemptyset(&action.sa_mask);
  if (sigaction(SIGUSR1, &action, 0) != 0) return 42;

  static const unsigned char original_xmm[16] = {
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
    0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
  };
  unsigned char restored_xmm[16];
  unsigned short control = 0x077f;
  unsigned short restored_control;
  unsigned int mxcsr = 0x3f80;
  unsigned int restored_mxcsr;
  __asm__ volatile("fldcw %0" : : "m"(control));
  __asm__ volatile("ldmxcsr %0" : : "m"(mxcsr));
  __asm__ volatile("movdqu %0, %%xmm15" : : "m"(original_xmm) : "xmm15");
  int result = kill(getpid(), SIGUSR1);
  __asm__ volatile("movdqu %%xmm15, %0" : "=m"(restored_xmm));
  __asm__ volatile("fnstcw %0" : "=m"(restored_control));
  __asm__ volatile("stmxcsr %0" : "=m"(restored_mxcsr));
  if (result != 0x5a) return 43;
  if (mode == 1) {
    static const unsigned char zero[16];
    if (memcmp(restored_xmm, zero, 16) != 0) return 44;
    if (restored_control != 0x037f) return 45;
    if (restored_mxcsr != 0x1f80) return 46;
  } else {
    unsigned char expected[16];
    for (unsigned i = 0; i < 16; ++i) expected[i] = (unsigned char)(0xa0 + i);
    if (memcmp(restored_xmm, expected, 16) != 0) return 47;
  }
  return 0;
}
