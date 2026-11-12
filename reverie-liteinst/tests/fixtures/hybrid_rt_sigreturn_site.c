#define _GNU_SOURCE
#include <dlfcn.h>
#include <inttypes.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SA_RESTORER
#define SA_RESTORER 0x04000000
#endif

__asm__(".text\n"
        ".p2align 4\n"
        ".global reverie_liteinst_rt_sigreturn_syscall\n"
        ".type reverie_liteinst_rt_sigreturn_syscall,@function\n"
        "reverie_liteinst_rt_sigreturn_syscall:\n"
        "mov %rdi, %rax\n"
        ".global reverie_liteinst_rt_sigreturn_site\n"
        "reverie_liteinst_rt_sigreturn_site:\n"
        "syscall\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "ret\n"
        ".size reverie_liteinst_rt_sigreturn_syscall, "
        ".-reverie_liteinst_rt_sigreturn_syscall\n"
        ".p2align 4\n"
        ".global reverie_liteinst_rt_sigreturn_restorer\n"
        ".type reverie_liteinst_rt_sigreturn_restorer,@function\n"
        "reverie_liteinst_rt_sigreturn_restorer:\n"
        "mov $15, %rax\n"
        "jmp reverie_liteinst_rt_sigreturn_site\n"
        ".size reverie_liteinst_rt_sigreturn_restorer, "
        ".-reverie_liteinst_rt_sigreturn_restorer\n");

extern long reverie_liteinst_rt_sigreturn_syscall(long number);
extern unsigned char reverie_liteinst_rt_sigreturn_site;
extern void reverie_liteinst_rt_sigreturn_restorer(void);

typedef uint64_t (*count_fn)(uint64_t);

struct kernel_sigaction {
  void (*handler)(int);
  unsigned long flags;
  void (*restorer)(void);
  uint64_t mask;
};

static volatile sig_atomic_t handler_calls;
static volatile long first_getpid;
static volatile long second_getpid;

static void handle_signal(int signal) {
  if (signal != SIGUSR1) {
    _exit(40);
  }
  ++handler_calls;
  first_getpid = reverie_liteinst_rt_sigreturn_syscall(SYS_getpid);
  second_getpid = reverie_liteinst_rt_sigreturn_syscall(SYS_getpid);
}

static count_fn load_count(const char *name) {
  count_fn function = (count_fn)dlsym(RTLD_DEFAULT, name);
  if (function == NULL) {
    fprintf(stderr, "missing %s: %s\n", name, dlerror());
    _exit(41);
  }
  return function;
}

int main(void) {
  struct kernel_sigaction action = {
      .handler = handle_signal,
      .flags = SA_RESTORER,
      .restorer = reverie_liteinst_rt_sigreturn_restorer,
      .mask = 0,
  };
  if (syscall(SYS_rt_sigaction, SIGUSR1, &action, NULL, sizeof(action.mask)) !=
      0) {
    return 20;
  }

  long tid = syscall(SYS_gettid);
  if (tid <= 0 || syscall(SYS_tgkill, tid, tid, SIGUSR1) != 0) {
    return 21;
  }
  if (handler_calls != 1 || first_getpid != tid || second_getpid != tid) {
    return 22;
  }

  const unsigned char *site = &reverie_liteinst_rt_sigreturn_site;
  if (site[0] != 0x0f || site[1] != 0x05) {
    return 23;
  }
  uint64_t address = (uint64_t)(uintptr_t)site;
  uint64_t traps = load_count("reverie_liteinst_site_trap_count")(address);
  uint64_t hooks = load_count("reverie_liteinst_site_hook_count")(address);
  printf("rt-sigreturn-restored calls=2 traps=%" PRIu64
         " hooks=%" PRIu64 " original=1 handler=1\n",
         traps, hooks);
  return 0;
}
