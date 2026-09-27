// Checks that FS and GS base changes made by arch_prctl are still in effect
// after the syscall returns. The syscall is made from three call sites: a raw
// `syscall` instruction, glibc's arch_prctl wrapper, and glibc's syscall(3).
// Each site runs many times. Under LiteInst the first subscribed call at a site
// traps and installs it, and every later call there goes through the
// patched-site hook. The two generic sites (raw and syscall(3)) are first
// warmed with getpid, so a Tool that subscribes getpid but not arch_prctl still
// reaches them through the hook.
//
// To test FS, switch to a new TLS block and then back to the original one.
// The new thread pointer is the first byte of a writable page placed directly
// after a PROT_NONE page. A static-TLS access below the thread pointer, such
// as the preload runtime reading its own thread_local state, therefore faults
// instead of silently reading the wrong block; so does a dynamic-model access,
// because the new block's DTV pointer is null. The TCB header, including the
// stack guard at %fs:0x28, is copied so this program's own stack-protector
// checks stay valid. glibc's two wrappers do not touch TLS when they succeed,
// and they are called through pointers from dlsym so that no lazy PLT binding
// runs on the new block.
//
// If the kernel's FS change is lost, a patched site can no longer restore the
// original FS either, so the failure report avoids TLS entirely: it formats by
// hand, writes with a raw syscall and leaves with exit_group.
//
// The program prints one line and exits 0 only if every check passed.
#define _GNU_SOURCE
#include <asm/prctl.h>
#include <dlfcn.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

__asm__(".text\n"
        ".p2align 4\n"
        ".global reverie_liteinst_fsgs_raw_syscall\n"
        ".type reverie_liteinst_fsgs_raw_syscall,@function\n"
        "reverie_liteinst_fsgs_raw_syscall:\n"
        "mov %rdi, %rax\n"
        "mov %rsi, %rdi\n"
        "mov %rdx, %rsi\n"
        ".global reverie_liteinst_fsgs_raw_site\n"
        "reverie_liteinst_fsgs_raw_site:\n"
        "syscall\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "ret\n"
        ".size reverie_liteinst_fsgs_raw_syscall, "
        ".-reverie_liteinst_fsgs_raw_syscall\n");

extern long reverie_liteinst_fsgs_raw_syscall(long nr, unsigned long arg0,
                                              unsigned long arg1);
extern unsigned char reverie_liteinst_fsgs_raw_site;

enum site { SITE_RAW, SITE_LIBC_ARCH_PRCTL, SITE_LIBC_SYSCALL, SITE_COUNT };
static const char *const SITE_NAMES[SITE_COUNT] = {"raw", "libc_arch_prctl",
                                                   "libc_syscall"};

#define ROUNDS 8
#define WARMUP_GETPIDS 4
#define TCB_COPY_BYTES 256

typedef int (*arch_prctl_fn)(int, unsigned long);
typedef long (*syscall_fn)(long, ...);

static arch_prctl_fn libc_arch_prctl;
static syscall_fn libc_syscall;

// Failures are recorded here while FS may point at the new block, then reported
// without TLS.
static volatile uint64_t failures;
static volatile uint64_t first_failure;
static volatile uint64_t first_failure_actual;
static volatile uint64_t first_failure_expected;
static uint64_t gs_word = UINT64_C(0x6773776f72646773);
static uint64_t calls[SITE_COUNT];

static void fail(uint64_t code, uint64_t actual, uint64_t expected) {
  if (failures++ == 0) {
    first_failure = code;
    first_failure_actual = actual;
    first_failure_expected = expected;
  }
}

static char *append_text(char *out, const char *text) {
  while (*text != '\0') {
    *out++ = *text++;
  }
  return out;
}

static char *append_number(char *out, uint64_t value, unsigned base) {
  char digits[24];
  unsigned count = 0;
  do {
    digits[count++] = "0123456789abcdef"[value % base];
    value /= base;
  } while (value != 0);
  while (count != 0) {
    *out++ = digits[--count];
  }
  return out;
}

static void __attribute__((noreturn)) report_failures_without_tls(void) {
  static char line[160];
  char *out = append_text(line, "fsgs failures=");
  out = append_number(out, failures, 10);
  out = append_text(out, " first=");
  out = append_number(out, first_failure, 10);
  out = append_text(out, " actual=0x");
  out = append_number(out, first_failure_actual, 16);
  out = append_text(out, " expected=0x");
  out = append_number(out, first_failure_expected, 16);
  *out++ = '\n';
  long ignored;
  __asm__ volatile("syscall"
                   : "=a"(ignored)
                   : "a"((long)SYS_write), "D"(1L), "S"(line),
                     "d"((long)(out - line))
                   : "rcx", "r11", "memory");
  __asm__ volatile("syscall"
                   :
                   : "a"((long)SYS_exit_group), "D"(1L)
                   : "rcx", "r11", "memory");
  __builtin_unreachable();
}

static long call_site(enum site site, int code, unsigned long addr) {
  calls[site]++;
  switch (site) {
  case SITE_RAW:
    return reverie_liteinst_fsgs_raw_syscall(SYS_arch_prctl, code, addr);
  case SITE_LIBC_ARCH_PRCTL:
    return libc_arch_prctl(code, addr);
  case SITE_LIBC_SYSCALL:
    return libc_syscall(SYS_arch_prctl, code, addr);
  default:
    return -1;
  }
}

static uint64_t read_fs0(void) {
  uint64_t value;
  __asm__ volatile("mov %%fs:0, %0" : "=r"(value));
  return value;
}

static uint64_t read_gs0(void) {
  uint64_t value;
  __asm__ volatile("mov %%gs:0, %0" : "=r"(value));
  return value;
}

// Failure codes are site * 100 + check.
static void check_fs(enum site site, uint64_t expected, uint64_t check) {
  uint64_t got = 0;
  uint64_t base = (uint64_t)site * 100 + check;
  if (call_site(site, ARCH_GET_FS, (unsigned long)&got) != 0) {
    fail(base, 1, 0);
  }
  if (got != expected) {
    fail(base + 1, got, expected);
  }
  // The TCB self pointer: proves the CPU's FS base, not only the kernel's
  // saved copy, is the expected block.
  uint64_t self = read_fs0();
  if (self != expected) {
    fail(base + 2, self, expected);
  }
}

static void check_gs(enum site site, uint64_t expected, uint64_t check) {
  uint64_t got = 0;
  uint64_t base = (uint64_t)site * 100 + check;
  if (call_site(site, ARCH_GET_GS, (unsigned long)&got) != 0) {
    fail(base, 1, 0);
  }
  if (got != expected) {
    fail(base + 1, got, expected);
  } else if (expected != 0) {
    // Only dereference GS once the kernel reports the expected base; a lost
    // update leaves GS at 0 and the read would fault.
    uint64_t word = read_gs0();
    if (word != gs_word) {
      fail(base + 2, word, gs_word);
    }
  }
}

static void run_site(enum site site, uint64_t original_fs, uint64_t new_fs) {
  uint64_t gs_target = (uint64_t)(uintptr_t)&gs_word;
  for (unsigned round = 0; round < ROUNDS; ++round) {
    // Switch to the new TLS block. Nothing here may touch TLS until FS is
    // back on the original block.
    if (call_site(site, ARCH_SET_FS, new_fs) != 0) {
      fail((uint64_t)site * 100 + 10, 1, 0);
    }
    check_fs(site, new_fs, 20);
    if (call_site(site, ARCH_SET_FS, original_fs) != 0) {
      fail((uint64_t)site * 100 + 30, 1, 0);
    }
    check_fs(site, original_fs, 40);

    if (call_site(site, ARCH_SET_GS, gs_target) != 0) {
      fail((uint64_t)site * 100 + 50, 1, 0);
    }
    check_gs(site, gs_target, 60);
    if (call_site(site, ARCH_SET_GS, 0) != 0) {
      fail((uint64_t)site * 100 + 70, 1, 0);
    }
    check_gs(site, 0, 80);

    if (failures != 0) {
      report_failures_without_tls();
    }
  }
}

// Find the first `syscall` instruction in a glibc wrapper. The scan must run
// before the wrapper's first call, because LiteInst later overwrites the site.
static uint64_t find_syscall_instruction(const void *function) {
  const unsigned char *bytes = function;
  for (unsigned i = 0; i + 1 < 64; ++i) {
    if (bytes[i] == 0x0f && bytes[i + 1] == 0x05) {
      return (uint64_t)(uintptr_t)(bytes + i);
    }
  }
  return 0;
}

typedef uint64_t (*count_fn)(uint64_t);

static long long hook_count(count_fn count, uint64_t address) {
  if (count == NULL || address == 0) {
    return -1;
  }
  return (long long)count(address);
}

int main(void) {
  // dlsym returns libc's definitions; in this non-PIE program `&syscall`
  // would be the PLT stub.
  libc_arch_prctl = (arch_prctl_fn)dlsym(RTLD_NEXT, "arch_prctl");
  libc_syscall = (syscall_fn)dlsym(RTLD_NEXT, "syscall");
  if (libc_arch_prctl == NULL || libc_syscall == NULL) {
    fprintf(stderr, "dlsym: %s\n", dlerror());
    return 14;
  }
  uint64_t site_addresses[SITE_COUNT] = {
      (uint64_t)(uintptr_t)&reverie_liteinst_fsgs_raw_site,
      find_syscall_instruction((const void *)libc_arch_prctl),
      find_syscall_instruction((const void *)libc_syscall),
  };
  long pid = getpid();
  for (int i = 0; i < WARMUP_GETPIDS; ++i) {
    if (reverie_liteinst_fsgs_raw_syscall(SYS_getpid, 0, 0) != pid ||
        libc_syscall(SYS_getpid) != pid) {
      return 13;
    }
  }

  uint64_t original_fs = read_fs0();
  long page = sysconf(_SC_PAGESIZE);
  unsigned char *mapping = mmap(NULL, (size_t)page * 2, PROT_NONE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (mapping == MAP_FAILED) {
    perror("mmap");
    return 10;
  }
  unsigned char *new_tcb = mapping + page;
  if (mprotect(new_tcb, (size_t)page, PROT_READ | PROT_WRITE) != 0) {
    perror("mprotect");
    return 11;
  }
  memcpy(new_tcb, (const void *)(uintptr_t)original_fs, TCB_COPY_BYTES);
  uint64_t new_fs = (uint64_t)(uintptr_t)new_tcb;
  // tcbhead_t.tcb and tcbhead_t.self both point at the TCB itself. Clear
  // tcbhead_t.dtv so that a dynamic-model TLS access (__tls_get_addr) faults
  // too, instead of following the copied pointer back to the original block.
  uint64_t no_dtv = 0;
  memcpy(new_tcb, &new_fs, sizeof(new_fs));
  memcpy(new_tcb + 8, &no_dtv, sizeof(no_dtv));
  memcpy(new_tcb + 16, &new_fs, sizeof(new_fs));

  for (int site = 0; site < SITE_COUNT; ++site) {
    run_site((enum site)site, original_fs, new_fs);
  }
  if (read_fs0() != original_fs) {
    fail(900, read_fs0(), original_fs);
    report_failures_without_tls();
  }

  count_fn count =
      (count_fn)dlsym(RTLD_DEFAULT, "reverie_liteinst_site_hook_count");
  printf("fsgs ok rounds=%d", ROUNDS);
  for (int site = 0; site < SITE_COUNT; ++site) {
    printf(" %s_calls=%" PRIu64 " %s_hooks=%lld", SITE_NAMES[site],
           calls[site], SITE_NAMES[site],
           hook_count(count, site_addresses[site]));
  }
  printf("\n");
  return 0;
}
