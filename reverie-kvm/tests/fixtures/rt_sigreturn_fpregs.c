#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/ucontext.h>
#include <unistd.h>

#define XSAVE_SIGNAL_SIZE 836
#define XMM15_OFFSET (160 + 15 * 16)
#define FP_SW_RESERVED_OFFSET 464
#define XSTATE_BV_OFFSET 512
#define XSTATE_SIZE_WITHOUT_YMM 576
#define TEST_FP_XSTATE_MAGIC1 UINT32_C(0x46505853)
#define TEST_FP_XSTATE_MAGIC2 UINT32_C(0x46505845)
#define XFEATURE_X87_SSE UINT64_C(3)

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
  if (mode == 5) {
    context->uc_stack.ss_flags = 0x1234;
  }
  memcpy(alternate_fpstate, context->uc_mcontext.fpregs, XSAVE_SIGNAL_SIZE);
  if (mode == 4) {
    // Linux accepts the legacy 512-byte FXSAVE form when magic1 is zero.
    memset(alternate_fpstate + FP_SW_RESERVED_OFFSET, 0,
           512 - FP_SW_RESERVED_OFFSET);
  } else if (mode == 6) {
    // A bounded standard-format x87/SSE-only image needs no YMM payload.
    uint32_t magic1 = TEST_FP_XSTATE_MAGIC1;
    uint32_t magic2 = TEST_FP_XSTATE_MAGIC2;
    uint32_t extended_size = XSTATE_SIZE_WITHOUT_YMM + sizeof(uint32_t);
    uint32_t xstate_size = XSTATE_SIZE_WITHOUT_YMM;
    uint64_t features = XFEATURE_X87_SSE;
    memcpy(alternate_fpstate + FP_SW_RESERVED_OFFSET, &magic1, 4);
    memcpy(alternate_fpstate + FP_SW_RESERVED_OFFSET + 4, &extended_size, 4);
    memcpy(alternate_fpstate + FP_SW_RESERVED_OFFSET + 8, &features, 8);
    memcpy(alternate_fpstate + FP_SW_RESERVED_OFFSET + 16, &xstate_size, 4);
    memset(alternate_fpstate + FP_SW_RESERVED_OFFSET + 20, 0,
           512 - (FP_SW_RESERVED_OFFSET + 20));
    memcpy(alternate_fpstate + XSTATE_BV_OFFSET, &features, 8);
    memset(alternate_fpstate + XSTATE_BV_OFFSET + 8, 0,
           XSTATE_SIZE_WITHOUT_YMM - (XSTATE_BV_OFFSET + 8));
    memcpy(alternate_fpstate + XSTATE_SIZE_WITHOUT_YMM, &magic2, 4);
  }
  for (unsigned i = 0; i < 16; ++i) {
    alternate_fpstate[XMM15_OFFSET + i] = (unsigned char)(0xa0 + i);
  }
  context->uc_mcontext.fpregs = (void *)alternate_fpstate;
}

int main(int argc, char **argv) {
  if (argc != 2) return 41;
  mode = argv[1][0] == 'n' ? 1
       : argv[1][0] == 'i' ? 3
       : argv[1][0] == 'l' ? 4
       : argv[1][0] == 'u' ? 5
       : argv[1][0] == 's' ? 6
       : 2;
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
