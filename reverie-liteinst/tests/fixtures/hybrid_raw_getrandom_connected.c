#define _GNU_SOURCE

#include <stddef.h>
#include <stdint.h>
#include <sys/syscall.h>

#if !defined(__x86_64__)
#error "the connected raw getrandom control requires x86-64"
#endif
#if SYS_getrandom != 318
#error "the connected raw getrandom control binds x86-64 syscall 318"
#endif

static const char success_line[] = "raw-getrandom-connected\n";

static long raw_syscall6(long number, long arg1, long arg2, long arg3,
                         long arg4, long arg5, long arg6) {
  register long r10 __asm__("r10") = arg4;
  register long r8 __asm__("r8") = arg5;
  register long r9 __asm__("r9") = arg6;
  long result;
  __asm__ volatile("syscall"
                   : "=a"(result)
                   : "a"(number), "D"(arg1), "S"(arg2), "d"(arg3),
                     "r"(r10), "r"(r8), "r"(r9)
                   : "rcx", "r11", "memory");
  return result;
}

__asm__(".text\n"
        ".p2align 4\n"
        ".global ordinary_text_getrandom\n"
        ".type ordinary_text_getrandom,@function\n"
        "ordinary_text_getrandom:\n"
        "xor %edx,%edx\n"
        "mov $318,%eax\n"
        ".global ordinary_text_getrandom_syscall\n"
        "ordinary_text_getrandom_syscall:\n"
        "syscall\n"
        "ret\n"
        ".size ordinary_text_getrandom,.-ordinary_text_getrandom\n");

extern long ordinary_text_getrandom(unsigned char *buffer, size_t len);

int main(void) {
  unsigned char random_bytes[32];
  if (ordinary_text_getrandom(random_bytes, sizeof(random_bytes)) !=
      (long)sizeof(random_bytes)) {
    return 40;
  }
  if (raw_syscall6(SYS_write, 1, (long)(uintptr_t)success_line,
                   (long)(sizeof(success_line) - 1), 0, 0, 0) !=
      (long)(sizeof(success_line) - 1)) {
    return 41;
  }
  return 0;
}
