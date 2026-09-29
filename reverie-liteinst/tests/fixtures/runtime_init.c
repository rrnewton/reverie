#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <link.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/syscall.h>
#include <unistd.h>

/* The fixture neither loads the runtime nor calls its initializer. The
 * controller must do both after the initial dynamic-loader pass. */
static unsigned early_runtime_objects;
static unsigned preinit_calls;
static volatile int *preinit_errno;
enum { ERRNO_WITNESS = 0x5a17 };

#if defined(PREINIT_ENTRY_TIMER) || defined(PREINIT_MAPPING_TIMER)
extern void runtime_init_preinit_marker(void);
#endif
#ifdef PREINIT_MAPPING_TIMER
static volatile long preinit_mapping_result = -1;
#endif

#ifdef INTERPOSE_SYSCONF
static const char *callback_marker;

static long raw_syscall(long number, long a0, long a1, long a2, long a3) {
  register long r10 __asm__("r10") = a3;
  long result;
  __asm__ volatile("syscall"
                   : "=a"(result)
                   : "a"(number), "D"(a0), "S"(a1), "d"(a2), "r"(r10)
                   : "rcx", "r11", "memory");
  return result;
}

/* A private initializer must not invoke executable-owned interposition. The
 * callback makes a durable witness and exits even if its caller would later
 * turn the failure into an apparently correct activation refusal. */
long sysconf(int name) {
  (void)name;
  if (callback_marker != NULL) {
    long fd = raw_syscall(SYS_openat, AT_FDCWD, (long)callback_marker,
                          O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd >= 0) {
      (void)raw_syscall(SYS_write, fd, (long)"sysconf\n", 8, 0);
      (void)raw_syscall(SYS_close, fd, 0, 0, 0);
    }
  }
  (void)raw_syscall(SYS_exit_group, 98, 0, 0, 0);
  __builtin_unreachable();
}
#endif

static int observe_initial_objects(struct dl_phdr_info *info, size_t size,
                                   void *unused) {
  (void)size;
  (void)unused;
  if (strstr(info->dlpi_name, "liteinst") != NULL ||
      strstr(info->dlpi_name, "/memfd:") != NULL)
    ++early_runtime_objects;
  return 0;
}

static void observe_preinit(int argc, char **argv, char **envp) {
  (void)envp;
#ifdef INTERPOSE_SYSCONF
  if (argc == 4) callback_marker = argv[3];
#else
  (void)argc;
  (void)argv;
#endif
  ++preinit_calls;
  dl_iterate_phdr(observe_initial_objects, NULL);
  preinit_errno = &errno;
  *preinit_errno = ERRNO_WITNESS;
#if defined(PREINIT_ENTRY_TIMER) || defined(PREINIT_MAPPING_TIMER)
  /* Last preinit operation: no libc call may hide the errno witness or add
   * an unaccounted operation between the marker and precision boundary. */
  runtime_init_preinit_marker();
#endif
}

__attribute__((section(".preinit_array"), used))
static void (*const preinit_observer)(int, char **, char **) = observe_preinit;

#if defined(PREINIT_ENTRY_TIMER) || defined(PREINIT_MAPPING_TIMER)
__asm__(".text\n"
        ".global runtime_init_preinit_marker\n"
        ".type runtime_init_preinit_marker,@function\n"
        "runtime_init_preinit_marker:\n"
        "push %r15\n"
        "movabs $0x004213579bdf2468, %r15\n"
        "mov $186, %eax\n"
        ".global runtime_init_preinit_gettid_site\n"
        "runtime_init_preinit_gettid_site:\n"
        "syscall\n"
#ifdef PREINIT_MAPPING_TIMER
        /* One controlled branch starts the twelve-instruction suffix. The
         * nonzero r15 witness forces the taken path; no zero-RCB request or
         * instruction-count tolerance is needed. */
        "test %r15, %r15\n"
        "jnz 1f\n"
        "ud2\n"
        "1:\n"
        /* Exactly twelve guest instructions after that branch: four setup
         * instructions, one unsubscribed syscall, seven NOPs. */
        "mov $10, %eax\n"
        "xor %edi, %edi\n"
        "xor %esi, %esi\n"
        "xor %edx, %edx\n"
        "syscall\n"
        ".rept 7\n nop\n .endr\n"
        ".global runtime_init_mapping_timer_expected\n"
        "runtime_init_mapping_timer_expected:\n"
        "mov %rax, preinit_mapping_result(%rip)\n"
#endif
        "pop %r15\n"
        "ret\n"
        ".size runtime_init_preinit_marker, .-runtime_init_preinit_marker\n");
#endif

#ifdef TIMER_BOUNDARY
/* This entry shim has no function calls or syscalls. A timer armed by the
 * post-exec Tool callback must cross controller initialization and fire at the
 * same loop iteration as ptrace. Saving r15 preserves the original ABI. */
__asm__(".text\n"
        ".global runtime_init_entry\n"
        ".type runtime_init_entry,@function\n"
        "runtime_init_entry:\n"
        "push %r15\n"
        "mov $2000000, %r15\n"
        ".global runtime_init_timer_loop\n"
        "runtime_init_timer_loop:\n"
        "dec %r15\n"
        "jnz runtime_init_timer_loop\n"
        ".global runtime_init_timer_end\n"
        "runtime_init_timer_end:\n"
        "pop %r15\n"
        "jmp _start\n"
        ".size runtime_init_entry, .-runtime_init_entry\n");
#endif

__asm__(".data\n"
        ".p2align 4\n"
        ".global runtime_init_expected_xmm\n"
        "runtime_init_expected_xmm:\n"
        ".quad 0x0123456789abcdef, 0xfedcba9876543210\n"
        ".global runtime_init_observed_xmm\n"
        "runtime_init_observed_xmm:\n.zero 16\n"
        ".text\n"
        ".global runtime_init_getpid\n"
        ".type runtime_init_getpid,@function\n"
        "runtime_init_getpid:\n"
        "push %r12\n"
        "movabs $0x00123456789abcde, %r12\n"
        "movdqu runtime_init_expected_xmm(%rip), %xmm0\n"
        "mov $39, %eax\n"
        ".p2align 6\n"
        ".global runtime_init_getpid_site\n"
        "runtime_init_getpid_site:\n"
        "syscall\n"
        ".rept 6\n nop\n .endr\n"
        "movdqu %xmm0, runtime_init_observed_xmm(%rip)\n"
        "pop %r12\n"
        "ret\n"
        ".size runtime_init_getpid, .-runtime_init_getpid\n");

extern long runtime_init_getpid(void);
extern const unsigned char runtime_init_expected_xmm[16];
extern unsigned char runtime_init_observed_xmm[16];

static unsigned hex_digit(char byte) {
  if (byte >= '0' && byte <= '9') return (unsigned)(byte - '0');
  if (byte >= 'a' && byte <= 'f') return (unsigned)(byte - 'a') + 10;
  return 16;
}

static int expected_environment(void) {
  static const char *const names[] = {
      "REVERIE_LITEINST_HOST_RUNTIME", "REVERIE_LITEINST_TOOL",
      "REVERIE_PRELOAD_TOOL", "REVERIE_LITEINST_STRADDLER_STALENESS_TICKS"};
  if (getenv("LD_PRELOAD") != NULL) return 0;
  for (unsigned i = 0; i < sizeof(names) / sizeof(names[0]); ++i) {
    const char *value = getenv(names[i]);
    if (value == NULL || strcmp(value, "runtime-init-poison") != 0) return 0;
  }
  return 1;
}

int main(int argc, char **argv) {
  /* Read the preinit-resolved TLS address directly, before any libc call can
   * alter errno or conceal a controller-helper side effect. */
  if (preinit_errno == NULL || *preinit_errno != ERRNO_WITNESS) return 80;
#ifdef PREINIT_MAPPING_TIMER
  if (preinit_mapping_result != 0) return 81;
#endif
#ifdef INTERPOSE_SYSCONF
  if (argc != 4) return 70;
#else
  if (argc != 3) return 70;
#endif
  /* This is the first application-entry side effect. Negative cases must
   * refuse activation before creating this file, even if later checks fail. */
  int marker = open(argv[2], O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
  if (marker < 0) return 71;
  if (write(marker, "entered\n", 8) != 8 || close(marker) != 0) return 72;
#ifdef INTERPOSE_SYSCONF
  /* Native control proves that the exported callback and its witness work. */
  (void)sysconf(_SC_PAGESIZE);
  return 96;
#endif
  if (preinit_calls != 1 || early_runtime_objects != 0) return 73;
  if (!expected_environment()) return 74;

  const unsigned char *entry = (const unsigned char *)getauxval(AT_ENTRY);
  if (entry == NULL || strlen(argv[1]) != 16) return 75;
  for (unsigned i = 0; i < 8; ++i) {
    unsigned high = hex_digit(argv[1][2 * i]);
    unsigned low = hex_digit(argv[1][2 * i + 1]);
    if (high == 16 || low == 16 || entry[i] != high * 16 + low) return 76;
  }

  volatile uint64_t stack_canary[2] = {
      UINT64_C(0x932164785abcde01), UINT64_C(0xfedcb98765432101)};
  for (unsigned call = 0; call < 2; ++call) {
    /* This exceeds Linux's PID_MAX_LIMIT, so the native negative control
     * cannot accidentally produce the Tool's selected result. */
    if (runtime_init_getpid() != 305419896) return 77;
    if (memcmp(runtime_init_expected_xmm, runtime_init_observed_xmm, 16) != 0)
      return 78;
    if (stack_canary[0] != UINT64_C(0x932164785abcde01) ||
        stack_canary[1] != UINT64_C(0xfedcb98765432101))
      return 79;
  }
  puts("calls=2 result=305419896 entry=restored env=unchanged preinit=clean simd=preserved errno=preserved");
  return 0;
}
