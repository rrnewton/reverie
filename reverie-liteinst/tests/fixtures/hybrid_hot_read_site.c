#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

/* One read(2) site, entered 32 times. The Tool decides whether LiteInst may
 * patch it, so its own trap and hook counts are the observable. */
__asm__(
    ".text\n"
    ".p2align 4\n"
    ".global reverie_liteinst_hybrid_read\n"
    ".type reverie_liteinst_hybrid_read,@function\n"
    "reverie_liteinst_hybrid_read:\n"
    ".cfi_startproc\n"
    "xor %eax, %eax\n"
    ".global reverie_liteinst_hybrid_read_site\n"
    "reverie_liteinst_hybrid_read_site:\n"
    "syscall\n"
    "nop\n"
    "nop\n"
    "nop\n"
    "ret\n"
    ".cfi_endproc\n"
    ".size reverie_liteinst_hybrid_read, .-reverie_liteinst_hybrid_read\n");

extern long reverie_liteinst_hybrid_read(long fd, void* buffer, long length);
extern unsigned char reverie_liteinst_hybrid_read_site;

typedef uint64_t (*count_fn)(uint64_t);

static count_fn load_count(const char* name) {
  count_fn function = (count_fn)dlsym(RTLD_DEFAULT, name);
  if (function == NULL) {
    fprintf(stderr, "missing %s: %s\n", name, dlerror());
    exit(20);
  }
  return function;
}

int main(void) {
  int fd = open("/dev/zero", O_RDONLY);
  if (fd < 0) {
    return 21;
  }
  for (unsigned i = 0; i < 32; ++i) {
    unsigned char byte = 0xff;
    if (reverie_liteinst_hybrid_read(fd, &byte, 1) != 1 || byte != 0) {
      return 22;
    }
  }

  uint64_t address = (uint64_t)(uintptr_t)&reverie_liteinst_hybrid_read_site;
  uint64_t traps = load_count("reverie_liteinst_site_trap_count")(address);
  uint64_t hooks = load_count("reverie_liteinst_site_hook_count")(address);
  printf("reads=32 traps=%" PRIu64 " hooks=%" PRIu64 "\n", traps, hooks);
  return 0;
}
