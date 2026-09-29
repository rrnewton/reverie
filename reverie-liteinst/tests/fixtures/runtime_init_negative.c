#include <stdint.h>
#include <fcntl.h>
#include <signal.h>
#include <sys/syscall.h>

#ifndef INITIALIZER_RESULT
#define INITIALIZER_RESULT 0
#endif

/* Structurally valid ordinary exports deliberately provide no Begin/Ready
 * handshake. A zero return alone must never authorize application entry. */
__attribute__((visibility("default"), used))
const uint64_t reverie_liteinst_host_runtime_abi = 1;

struct host_config {
  uint64_t version;
  uint64_t straddler_staleness_ticks;
};

#if defined(INITIALIZER_SIGNAL) || defined(INITIALIZER_CANCEL)
static long raw_syscall(long number, long a0, long a1, long a2, long a3) {
  register long r10 __asm__("r10") = a3;
  long result;
  __asm__ volatile("syscall"
                   : "=a"(result)
                   : "a"(number), "D"(a0), "S"(a1), "d"(a2), "r"(r10)
                   : "rcx", "r11", "memory");
  return result;
}
#endif

__attribute__((visibility("default"), noinline))
int reverie_liteinst_initialize_host(const struct host_config *config) {
  (void)config;
#ifdef INITIALIZER_SIGNAL
  /* getpid is subscribed by the Tool. Private helper syscalls must still run
   * natively: returning the Tool's fake PID would prevent this exact signal. */
  long pid = raw_syscall(SYS_getpid, 0, 0, 0, 0);
  long tid = raw_syscall(SYS_gettid, 0, 0, 0, 0);
  (void)raw_syscall(SYS_tgkill, pid, tid, SIGUSR1, 0);
  return -2;
#elif defined(INITIALIZER_CANCEL)
  const uint64_t receipt[2] = {
      UINT64_C(0x4c49544543414e43),
      (uint64_t)raw_syscall(SYS_getpid, 0, 0, 0, 0)};
  long fd = raw_syscall(SYS_openat, AT_FDCWD, (long)CANCELLATION_MARKER,
                        O_WRONLY | O_CREAT | O_EXCL, 0600);
  if (fd < 0 ||
      raw_syscall(SYS_write, fd, (long)receipt, sizeof(receipt), 0) !=
          (long)sizeof(receipt) ||
      raw_syscall(SYS_close, fd, 0, 0, 0) != 0) {
    (void)raw_syscall(SYS_exit_group, 97, 0, 0, 0);
    __builtin_unreachable();
  }
  /* The controller cancels only after independently reading the receipt.
   * There is no timeout or successful initializer return to race that proof. */
  for (;;) __asm__ volatile("pause" ::: "memory");
#else
  return INITIALIZER_RESULT;
#endif
}
