#define _GNU_SOURCE
#include <dlfcn.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

__asm__(".data\n"
        ".global reverie_liteinst_hybrid_flags\n"
        ".p2align 3\n"
        "reverie_liteinst_hybrid_flags:\n"
        ".quad 0\n"
        ".text\n"
        ".p2align 4\n"
        ".global reverie_liteinst_hybrid_getpid\n"
        ".type reverie_liteinst_hybrid_getpid,@function\n"
        "reverie_liteinst_hybrid_getpid:\n"
        "push %r12\n"
        "movabs $0x00123456789abcde, %r12\n"
        "mov $39, %eax\n"
        ".global reverie_liteinst_hybrid_getpid_site\n"
        "reverie_liteinst_hybrid_getpid_site:\n"
        "syscall\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "pushfq\n"
        "pop %rcx\n"
        "mov %rcx, reverie_liteinst_hybrid_flags(%rip)\n"
        "pop %r12\n"
        "ret\n"
        ".size reverie_liteinst_hybrid_getpid, .-reverie_liteinst_hybrid_getpid\n");

extern long reverie_liteinst_hybrid_getpid(void);
extern unsigned char reverie_liteinst_hybrid_getpid_site;
extern uint64_t reverie_liteinst_hybrid_flags;

typedef uint64_t (*count_fn)(uint64_t);
typedef void (*trap_fn)(void *);

static count_fn load_count(const char *name) {
  count_fn function = (count_fn)dlsym(RTLD_DEFAULT, name);
  if (function == NULL) {
    fprintf(stderr, "missing %s: %s\n", name, dlerror());
    exit(20);
  }
  return function;
}

int main(void) {
  long expected = -1;
  for (unsigned i = 0; i < 32; ++i) {
    long observed = reverie_liteinst_hybrid_getpid();
    if ((reverie_liteinst_hybrid_flags & (UINT64_C(1) << 18)) != 0) {
      return 22;
    }
    if (expected == -1) {
      expected = observed;
    } else if (expected != observed) {
      return 21;
    }
  }

  uint64_t address = (uint64_t)(uintptr_t)&reverie_liteinst_hybrid_getpid_site;
  uint64_t traps = load_count("reverie_liteinst_site_trap_count")(address);
  uint64_t hooks = load_count("reverie_liteinst_site_hook_count")(address);

  int spoof_attempts = 0;
  trap_fn trap = (trap_fn)dlsym(RTLD_DEFAULT, "reverie_liteinst_host_syscall_trap");
  if (trap == NULL) {
    return 24;
  }
  trap((void *)1);
  ++spoof_attempts;
  __asm__ volatile("movabs $0x7265766c69000004, %%rax\n\tint3"
                   :
                   :
                   : "rax", "memory");
  ++spoof_attempts;
  if (spoof_attempts != 2) {
    return 25;
  }

  printf("calls=32 traps=%" PRIu64 " hooks=%" PRIu64
         " ac=0 spoofs=%d\n",
         traps, hooks, spoof_attempts);
  return 0;
}
