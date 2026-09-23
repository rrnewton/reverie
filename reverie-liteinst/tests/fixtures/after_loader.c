/* Fixed single-loader fixture for the strict after-loader runner test. */
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define TOOL_GETPID_SENTINEL 0x4c495445L

_Static_assert(SYS_getpid == 39, "fixture assembly requires x86-64 getpid");

/* The jump is ordinary guest control flow after the syscall. The controller's
   eight-byte prefix scanner must instead encounter the reserved 0f 04 encoding
   at offset four and retain this site on the authenticated ptrace fallback. */
__asm__(".text\n"
        ".p2align 4\n"
        ".global reverie_liteinst_unpatchable_getpid\n"
        ".hidden reverie_liteinst_unpatchable_getpid\n"
        ".type reverie_liteinst_unpatchable_getpid,@function\n"
        "reverie_liteinst_unpatchable_getpid:\n"
        "mov $39, %eax\n"
        ".global reverie_liteinst_unpatchable_getpid_site\n"
        ".hidden reverie_liteinst_unpatchable_getpid_site\n"
        "reverie_liteinst_unpatchable_getpid_site:\n"
        "syscall\n"
        "jmp 1f\n"
        ".rept 12\n"
        ".byte 0x0f, 0x04\n"
        ".endr\n"
        "1:\n"
        "ret\n"
        ".size reverie_liteinst_unpatchable_getpid, "
        ".-reverie_liteinst_unpatchable_getpid\n");

extern long reverie_liteinst_unpatchable_getpid(void);

/* Keep the application stack pointer under fixture control at one patchable
   syscall site.  R12 retains the real call stack across the syscall and is
   restored before return.  Installed instrumentation must not dereference the
   supplied RSP: later calls deliberately point it into PROT_NONE guards. */
__asm__(".text\n"
        ".p2align 4\n"
        ".global reverie_liteinst_stack_getpid\n"
        ".hidden reverie_liteinst_stack_getpid\n"
        ".type reverie_liteinst_stack_getpid,@function\n"
        "reverie_liteinst_stack_getpid:\n"
        "push %r12\n"
        "mov %rsp, %r12\n"
        "mov %rdi, %rsp\n"
        "mov $39, %eax\n"
        ".global reverie_liteinst_stack_getpid_site\n"
        ".hidden reverie_liteinst_stack_getpid_site\n"
        "reverie_liteinst_stack_getpid_site:\n"
        "syscall\n"
        "mov %r12, %rsp\n"
        "pop %r12\n"
        "ret\n"
        ".size reverie_liteinst_stack_getpid, "
        ".-reverie_liteinst_stack_getpid\n");

extern long reverie_liteinst_stack_getpid(void *application_rsp);

static volatile unsigned stage;
static unsigned char first_random[16];
static unsigned long first_canary;
static _Thread_local unsigned long guest_tls = 0x13579bdf;
static struct timespec previous_time;
static int sampled_time;
static int canonical_output;

static void write_all_or_exit(int fd, const char *bytes, size_t length,
                              int exit_code) {
    while (length != 0) {
        ssize_t written = write(fd, bytes, length);
        if (written > 0) {
            bytes += written;
            length -= (size_t)written;
            continue;
        }
        if (written < 0 && errno == EINTR) continue;
        _exit(exit_code);
    }
}

static int envp_contains(char **envp, const char *entry) {
    for (; *envp != NULL; ++envp) {
        if (!strcmp(*envp, entry)) return 1;
    }
    return 0;
}

static unsigned long canary(void) {
    unsigned long value;
    __asm__ volatile("mov %%fs:0x28,%0" : "=r"(value));
    return value;
}

static unsigned char stack_pattern(size_t index) {
    return (unsigned char)(0x5aU ^ (unsigned char)(index * 131U));
}

static int exercise_patchable_getpid(unsigned calls) {
    long page = sysconf(_SC_PAGESIZE);
    if (page != 4096) return 112;
    size_t page_size = (size_t)page;
    size_t allocation_size = 3 * page_size;
    unsigned char *allocation = mmap(NULL, allocation_size, PROT_NONE,
                                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (allocation == MAP_FAILED) return 113;
    unsigned char *usable = allocation + page_size;
    if (mprotect(usable, page_size, PROT_READ | PROT_WRITE)) return 114;
    for (size_t i = 0; i < page_size; ++i) usable[i] = stack_pattern(i);

    void *stack_pointers[4] = {
        usable + page_size - 16,
        usable + page_size - 16,
        allocation + page_size - 16,
        usable + page_size,
    };
    for (unsigned call = 0; call < calls; ++call) {
        if (reverie_liteinst_stack_getpid(stack_pointers[call]) !=
            TOOL_GETPID_SENTINEL)
            return 99;
        for (size_t i = 0; i < page_size; ++i)
            if (usable[i] != stack_pattern(i)) return 115;
    }
    if (munmap(allocation, allocation_size)) return 116;
    return 0;
}

static void sample(const char *label) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t)) _exit(91);
    const unsigned char *r = (const unsigned char *)getauxval(AT_RANDOM);
    if (!r) _exit(92);
    if (sampled_time &&
        (t.tv_sec < previous_time.tv_sec ||
         (t.tv_sec == previous_time.tv_sec &&
          t.tv_nsec < previous_time.tv_nsec)))
        _exit(104);
    if (memcmp(first_random, r, sizeof(first_random))) _exit(105);
    if (canary() != first_canary) _exit(106);
    previous_time = t;
    sampled_time = 1;
    if (canonical_output) return;
    printf("%s time=%lld.%09ld canary=%016lx random=", label,
           (long long)t.tv_sec, t.tv_nsec, canary());
    for (unsigned i = 0; i < 16; ++i) printf("%02x", r[i]);
    putchar('\n');
}
/* GNU ELF preinit receives the original argv/envp. Do not rely on environ
   already having been assigned by __libc_start_main here. */
static void preinit(int argc, char **argv, char **envp) {
    static const char entered[] = "guest-entered-v1\n";
    (void)argc; (void)argv;
    if (stage++ != 0) _exit(93);
    write_all_or_exit(STDOUT_FILENO, entered, sizeof(entered) - 1, 107);
    canonical_output = envp_contains(
        envp, "LITEINST_CALLER_OUTPUT=canonical-v1");
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
    static const char canonical_tail[] =
        "restoration=preinit-constructor-main-ok\n"
        "environment=sentinel-preserved-loader-selectors-absent\n"
        "getpid=4c495445 calls=4 stages=3\n";
    if (stage++ != 2) return 96;
    sample("main");
    const char *keys[] = {"LITEINST_CALLER_SENTINEL", "LD_PRELOAD",
                         "REVERIE_LITEINST_HOST_RUNTIME", "REVERIE_LITEINST_TOOL"};
    const char *sentinel = getenv(keys[0]);
    if (!sentinel || strcmp(sentinel, "preserved")) return 108;
    for (unsigned i = 1; i < sizeof(keys)/sizeof(keys[0]); ++i)
        if (getenv(keys[i]) != NULL) return 109;
    if (!canonical_output) {
        for (unsigned i = 0; i < sizeof(keys)/sizeof(keys[0]); ++i) {
            const char *v = getenv(keys[i]);
            printf("env %s=%s\n", keys[i], v ? v : "<absent>");
        }
    }
    if (memcmp(first_random, (void *)getauxval(AT_RANDOM), 16)) return 97;
    if (canary() != first_canary) return 98;
    unsigned getpid_calls = 4;
    const char *configured_calls = getenv("LITEINST_CALLER_CALLS");
    if (configured_calls) {
        if (strcmp(configured_calls, "1")) return 102;
        getpid_calls = 1;
    }
    const char *configured_site = getenv("LITEINST_CALLER_SITE");
    int unpatchable_site = configured_site != NULL;
    if (unpatchable_site &&
        (strcmp(configured_site, "unpatchable") || getpid_calls != 1)) return 103;
    if (canonical_output &&
        (getpid_calls != 4 || configured_calls != NULL || unpatchable_site))
        return 110;
    /* Each return must carry a value the real getpid syscall cannot produce. */
    if (unpatchable_site) {
        if (reverie_liteinst_unpatchable_getpid() != TOOL_GETPID_SENTINEL) return 99;
    } else {
        int result = exercise_patchable_getpid(getpid_calls);
        if (result) return result;
    }
    if (canonical_output) {
        write_all_or_exit(STDOUT_FILENO, canonical_tail,
                          sizeof(canonical_tail) - 1, 111);
        return 0;
    } else if (getpid_calls == 4)
        printf("getpid=%lx stages=%u\n", TOOL_GETPID_SENTINEL, stage);
    else if (unpatchable_site)
        printf("getpid=%lx calls=1 site=unpatchable stages=%u\n",
               TOOL_GETPID_SENTINEL, stage);
    else
        printf("getpid=%lx calls=%u stages=%u\n",
               TOOL_GETPID_SENTINEL, getpid_calls, stage);
    return fflush(stdout) ? 100 : 0;
}
