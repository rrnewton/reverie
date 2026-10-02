#define _GNU_SOURCE
#include <signal.h>
#include <sys/syscall.h>
#include <unistd.h>

/* The separately compiled original source is unchanged except main's symbol. */
extern int continuation_original_main(int argc, char **argv);

int main(int argc, char **argv) {
    sigset_t inherited;
    if (sigprocmask(SIG_SETMASK, 0, &inherited)
        || sigismember(&inherited, SIGSTKFLT) != 0) return 125;
    int original = continuation_original_main(argc, argv);
    if (original != 0) return original;

    long arm_result, done_result;
    __asm__ volatile(
        "mov $1,%%eax\n\tmov $882,%%edi\n\txor %%esi,%%esi\n\txor %%edx,%%edx\n\t"
        "syscall\n\tmov %%rax,%0\n\t"
        "xor %%r10d,%%r10d\n\tjz 1f\n1:\n\t"
        ".rept 256\n\tnop\n\t.endr\n\t"
        "mov $1,%%eax\n\tmov $883,%%edi\n\txor %%esi,%%esi\n\txor %%edx,%%edx\n\t"
        "syscall\n\tmov %%rax,%1\n\t"
        : "=m"(arm_result), "=m"(done_result)
        :
        : "rax", "rdi", "rsi", "rdx", "rcx", "r11", "r10", "memory", "cc");
    /* Normal exit precedes the Rust parent's exact post-reap status oracle. */
    if (arm_result != 0) return 123;
    if (done_result != 0) return 124;
    return 0;
}
