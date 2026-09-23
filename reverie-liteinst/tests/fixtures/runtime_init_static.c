#include <stdint.h>

static long raw_syscall(long number, long a0, long a1, long a2, long a3) {
  register long r10 __asm__("r10") = a3;
  long result;
  __asm__ volatile("syscall"
                   : "=a"(result)
                   : "a"(number), "D"(a0), "S"(a1), "d"(a2), "r"(r10)
                   : "rcx", "r11", "memory");
  return result;
}

__attribute__((noreturn))
void runtime_init_static_entry(const uintptr_t *stack) {
  if (stack[0] == 3) {
    const char *marker = (const char *)stack[3];
    long fd = raw_syscall(257, -100, (long)marker, 1 | 64 | 128, 0600);
    if (fd >= 0) {
      (void)raw_syscall(1, fd, (long)"entered\n", 8, 0);
      (void)raw_syscall(3, fd, 0, 0, 0);
    }
  }
  (void)raw_syscall(231, 99, 0, 0, 0);
  __builtin_unreachable();
}

__asm__(".text\n.global _start\n.type _start,@function\n"
        "_start:\n mov %rsp, %rdi\n and $-16, %rsp\n"
        "call runtime_init_static_entry\n ud2\n"
        ".size _start, .-_start\n");
