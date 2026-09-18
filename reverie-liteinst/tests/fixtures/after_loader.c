/* Fixed single-loader fixture for the strict after-loader runner test. */
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define TOOL_GETPID_SENTINEL 0x4c495445L

static volatile unsigned stage;
static unsigned char first_random[16];
static unsigned long first_canary;
static _Thread_local unsigned long guest_tls = 0x13579bdf;

static unsigned long canary(void) {
    unsigned long value;
    __asm__ volatile("mov %%fs:0x28,%0" : "=r"(value));
    return value;
}
static void sample(const char *label) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t)) _exit(91);
    printf("%s time=%lld.%09ld canary=%016lx random=", label,
           (long long)t.tv_sec, t.tv_nsec, canary());
    const unsigned char *r = (const unsigned char *)getauxval(AT_RANDOM);
    if (!r) _exit(92);
    for (unsigned i = 0; i < 16; ++i) printf("%02x", r[i]);
    putchar('\n');
}
/* GNU ELF preinit receives the original argv/envp. Do not rely on environ
   already having been assigned by __libc_start_main here. */
static void preinit(int argc, char **argv, char **envp) {
    (void)argc; (void)argv; (void)envp;
    if (stage++ != 0) _exit(93);
    const unsigned char *r = (const unsigned char *)getauxval(AT_RANDOM);
    if (!r) _exit(94);
    memcpy(first_random, r, sizeof(first_random));
    first_canary = canary();
    sample("preinit");
    guest_tls = 0x2468ace0;
    errno = E2BIG;
}
__attribute__((section(".preinit_array"), used))
static void (*const preinit_slot)(int, char **, char **) = preinit;
__attribute__((constructor)) static void construct(void) {
    if (stage++ != 1) _exit(95);
    if (guest_tls != 0x2468ace0 || errno != E2BIG) _exit(101);
    sample("constructor");
}
int main(void) {
    if (stage++ != 2) return 96;
    sample("main");
    const char *keys[] = {"LITEINST_CALLER_SENTINEL", "LD_PRELOAD",
                         "REVERIE_LITEINST_HOST_RUNTIME", "REVERIE_LITEINST_TOOL"};
    for (unsigned i = 0; i < sizeof(keys)/sizeof(keys[0]); ++i) {
        const char *v = getenv(keys[i]);
        printf("env %s=%s\n", keys[i], v ? v : "<absent>");
    }
    if (memcmp(first_random, (void *)getauxval(AT_RANDOM), 16)) return 97;
    if (canary() != first_canary) return 98;
    /* Each return must carry a value the real getpid syscall cannot produce. */
    for (unsigned i = 0; i < 4; ++i) {
        long value;
        __asm__ volatile(".p2align 4\n\tsyscall" : "=a"(value) : "a"(SYS_getpid)
                         : "rcx", "r11", "memory");
        if (value != TOOL_GETPID_SENTINEL) return 99;
    }
    printf("getpid=%lx stages=%u\n", TOOL_GETPID_SENTINEL, stage);
    return fflush(stdout) ? 100 : 0;
}
