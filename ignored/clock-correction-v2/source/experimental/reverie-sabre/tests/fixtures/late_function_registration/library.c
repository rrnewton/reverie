#define _GNU_SOURCE
#include <errno.h>
#include <stddef.h>
#include <sys/types.h>
__attribute__((noinline, patchable_function_entry(32, 0)))
ssize_t late_probe(unsigned char *buf, size_t len, unsigned flags) {
  if (len != 4 || flags != 0x1357) { errno = EINVAL; return -1; }
  buf[0] = 0x6a;
  errno = EDOM;
  return 37;
}
__attribute__((noinline, patchable_function_entry(32, 0)))
ssize_t late_probe_two(unsigned char *buf, size_t len, unsigned flags) {
  return late_probe(buf, len, flags);
}
__attribute__((noinline)) long late_raw_getpid(void) {
  long result;
  __asm__ volatile("syscall" : "=a"(result) : "a"(39) : "rcx", "r11", "memory");
  return result;
}

extern __typeof(late_probe) late_probe_alias __attribute__((alias("late_probe")));
