#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

__asm__(".data\n"
        ".global reverie_liteinst_hybrid_flags\n"
        ".p2align 3\n"
        "reverie_liteinst_hybrid_flags:\n"
        ".quad 0\n"
        ".global reverie_liteinst_hybrid_simd_expected\n"
        ".p2align 4\n"
        "reverie_liteinst_hybrid_simd_expected:\n"
        ".quad 0x0123456789abcdef, 0xfedcba9876543210\n"
        ".global reverie_liteinst_hybrid_simd_observed\n"
        ".p2align 4\n"
        "reverie_liteinst_hybrid_simd_observed:\n"
        ".zero 16\n"
        ".text\n"
        ".p2align 4\n"
        ".global reverie_liteinst_hybrid_getpid\n"
        ".type reverie_liteinst_hybrid_getpid,@function\n"
        "reverie_liteinst_hybrid_getpid:\n"
        "push %r12\n"
        "movabs $0x00123456789abcde, %r12\n"
        "movdqu reverie_liteinst_hybrid_simd_expected(%rip), %xmm0\n"
        "mov $39, %eax\n"
        ".global reverie_liteinst_hybrid_getpid_site\n"
        "reverie_liteinst_hybrid_getpid_site:\n"
        "syscall\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "movdqu %xmm0, reverie_liteinst_hybrid_simd_observed(%rip)\n"
        "pushfq\n"
        "pop %rcx\n"
        "mov %rcx, reverie_liteinst_hybrid_flags(%rip)\n"
        "pop %r12\n"
        "ret\n"
        ".size reverie_liteinst_hybrid_getpid, .-reverie_liteinst_hybrid_getpid\n");

extern long reverie_liteinst_hybrid_getpid(void);
extern unsigned char reverie_liteinst_hybrid_getpid_site;
extern uint64_t reverie_liteinst_hybrid_flags;
extern unsigned char reverie_liteinst_hybrid_simd_expected[16];
extern unsigned char reverie_liteinst_hybrid_simd_observed[16];

typedef uint64_t (*count_fn)(uint64_t);
typedef void (*trap_fn)(void *);
typedef trap_fn (*trap_address_fn)(void);

struct host_syscall_frame {
  uint64_t flags;
  uint64_t r15;
  uint64_t r14;
  uint64_t r13;
  uint64_t r12;
  uint64_t r11;
  uint64_t r10;
  uint64_t r9;
  uint64_t r8;
  uint64_t rdi;
  uint64_t rsi;
  uint64_t rbp;
  uint64_t rbx;
  uint64_t rdx;
  uint64_t rcx;
  uint64_t rax;
  uint64_t rsp;
  uint64_t rip;
};

static count_fn load_count(const char *name) {
  count_fn function = (count_fn)dlsym(RTLD_DEFAULT, name);
  if (function == NULL) {
    fprintf(stderr, "missing %s: %s\n", name, dlerror());
    exit(20);
  }
  return function;
}

static int write_all(int fd, const char *bytes, size_t length) {
  while (length != 0) {
    ssize_t written = write(fd, bytes, length);
    if (written < 0 && errno == EINTR) {
      continue;
    }
    if (written <= 0) {
      return -1;
    }
    bytes += (size_t)written;
    length -= (size_t)written;
  }
  return 0;
}

static int write_pid_file(const char *path) {
  int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
  if (fd < 0) {
    return -1;
  }
  char pid[32];
  int length = snprintf(pid, sizeof(pid), "%ld\n", syscall(SYS_gettid));
  int failed = length <= 0 || (size_t)length >= sizeof(pid) ||
               write_all(fd, pid, (size_t)length) != 0;
  failed |= close(fd) != 0;
  return failed ? -1 : 0;
}

int main(int argc, char **argv) {
  const char *spoof_mode = NULL;
  const char *spoof_proof = NULL;
  int spoof_proof_fd = -1;
  trap_fn trap = NULL;
  if (argc == 5) {
    spoof_mode = argv[1];
    if (strcmp(spoof_mode, "raw-trap") == 0) {
      spoof_proof =
          "calls=32 traps=1 hooks=31 ac=0 simd=1 mode=raw-trap\n";
    } else if (strcmp(spoof_mode, "forged-frame") == 0) {
      spoof_proof =
          "calls=32 traps=1 hooks=31 ac=0 simd=1 mode=forged-frame\n";
    } else if (strcmp(spoof_mode, "marker-int3") == 0) {
      spoof_proof =
          "calls=32 traps=1 hooks=31 ac=0 simd=1 mode=marker-int3\n";
    } else {
      return 24;
    }
    if (write_pid_file(argv[2]) != 0) {
      return 25;
    }
    spoof_proof_fd =
        open(argv[3], O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
    if (spoof_proof_fd < 0) {
      return 27;
    }
    trap_address_fn trap_address = (trap_address_fn)dlsym(
        RTLD_DEFAULT, "reverie_liteinst_host_syscall_trap_address");
    if (trap_address == NULL) {
      return 28;
    }
    trap = trap_address();
  } else if (argc != 1) {
    return 26;
  }

  long expected = -1;
  for (unsigned i = 0; i < 32; ++i) {
    long observed = reverie_liteinst_hybrid_getpid();
    if ((reverie_liteinst_hybrid_flags & (UINT64_C(1) << 18)) != 0) {
      return 22;
    }
    if (memcmp(reverie_liteinst_hybrid_simd_expected,
               reverie_liteinst_hybrid_simd_observed, 16) != 0) {
      return 23;
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

  if (spoof_mode == NULL) {
    printf("calls=32 traps=%" PRIu64 " hooks=%" PRIu64
           " ac=0 simd=1 spoofs=0\n",
           traps, hooks);
    return 0;
  }
  if (traps != 1 || hooks != 31 ||
      write_all(spoof_proof_fd, spoof_proof, strlen(spoof_proof)) != 0 ||
      close(spoof_proof_fd) != 0) {
    return 27;
  }
  while (access(argv[4], F_OK) != 0) {
    if (errno != ENOENT || (usleep(1000) != 0 && errno != EINTR)) {
      return 29;
    }
  }

  if (strcmp(spoof_mode, "raw-trap") == 0) {
    trap((void *)1);
  } else if (strcmp(spoof_mode, "forged-frame") == 0) {
    struct host_syscall_frame forged = {0};
    forged.rax = SYS_getpid;
    forged.rsp = (uint64_t)(uintptr_t)&forged;
    forged.rip = address;
    trap(&forged);
  } else {
    __asm__ volatile("movabs $0x7265766c69000004, %%rax\n\tint3"
                     :
                     :
                     : "rax", "memory");
  }
  return 30;
}
