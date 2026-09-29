#define _GNU_SOURCE
#include <fcntl.h>
#include <link.h>
#include <sys/syscall.h>

#ifndef CALLBACK_MARKER
#error "CALLBACK_MARKER must identify the test-owned evidence file"
#endif

static long raw_syscall(long number, long a0, long a1, long a2, long a3) {
  register long r10 __asm__("r10") = a3;
  long result;
  __asm__ volatile("syscall"
                   : "=a"(result)
                   : "a"(number), "D"(a0), "S"(a1), "d"(a2), "r"(r10)
                   : "rcx", "r11", "memory");
  return result;
}

static void record_callback(void) {
  long fd = raw_syscall(SYS_openat, AT_FDCWD, (long)CALLBACK_MARKER,
                        O_WRONLY | O_CREAT | O_APPEND, 0600);
  if (fd < 0) {
    (void)raw_syscall(SYS_exit_group, 97, 0, 0, 0);
    __builtin_unreachable();
  }
  (void)raw_syscall(SYS_write, fd, (long)"callback\n", 9, 0);
  (void)raw_syscall(SYS_close, fd, 0, 0, 0);
}

#ifdef HOSTILE_AUDIT
unsigned int la_version(unsigned int version) {
  (void)version;
  record_callback();
  return LAV_CURRENT;
}
#else
__attribute__((constructor)) static void hostile_constructor(void) {
  record_callback();
}
#endif
