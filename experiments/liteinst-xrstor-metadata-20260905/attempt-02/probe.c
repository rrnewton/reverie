#define _GNU_SOURCE
#include <cpuid.h>
#include <inttypes.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/time.h>
#include <unistd.h>

enum { IMAGE_SIZE = 2440, HIGH_OFFSET = 1408, HIGH_SIZE = 1024 };

extern void capture_pair(void *before, void *after, int nonzero, int restore);

static uint64_t word(const unsigned char *image, size_t offset) {
    uint64_t value;
    memcpy(&value, image + offset, sizeof(value));
    return value;
}

static int cpu_guard(void) {
    unsigned int eax, ebx, ecx, edx;
    if (__get_cpuid_max(0, NULL) < 0xd)
        return 0;
    __cpuid(0, eax, ebx, ecx, edx);
    printf("vendor_words=%08x:%08x:%08x\n", ebx, edx, ecx);
    __cpuid(1, eax, ebx, ecx, edx);
    printf("cpuid1=%08x:%08x:%08x:%08x\n", eax, ebx, ecx, edx);
    if ((ecx & (7u << 26)) != (7u << 26))
        return 0;
    __cpuid_count(7, 0, eax, ebx, ecx, edx);
    printf("cpuid7=%08x:%08x:%08x:%08x\n", eax, ebx, ecx, edx);
    if ((ebx & ((1u << 5) | (1u << 16))) != ((1u << 5) | (1u << 16)))
        return 0;
    unsigned int xcr_low, xcr_high;
    __asm__ volatile("xgetbv" : "=a"(xcr_low), "=d"(xcr_high) : "c"(0));
    printf("xcr0=%08x:%08x\n", xcr_high, xcr_low);
    if (xcr_low != 0x2e7 || xcr_high != 0)
        return 0;
    __cpuid_count(0xd, 0, eax, ebx, ecx, edx);
    printf("cpuid_d_0=%08x:%08x:%08x:%08x\n", eax, ebx, ecx, edx);
    if (eax != 0x2e7 || edx != 0 || ebx != IMAGE_SIZE || ecx != IMAGE_SIZE)
        return 0;
    const unsigned int components[][2] = {
        {2, 256}, {5, 64}, {6, 512}, {7, HIGH_SIZE}, {9, 8}
    };
    unsigned int previous_end = 576;
    for (size_t index = 0; index < sizeof(components) / sizeof(components[0]); ++index) {
        __cpuid_count(0xd, components[index][0], eax, ebx, ecx, edx);
        printf("cpuid_d_%u=%08x:%08x:%08x:%08x\n",
               components[index][0], eax, ebx, ecx, edx);
        if (eax != components[index][1] || ebx < previous_end ||
            ebx > IMAGE_SIZE - eax || (ecx & 1) ||
            (components[index][0] == 7 && ebx != HIGH_OFFSET))
            return 0;
        previous_end = ebx + eax;
    }
    return 1;
}

static int quiet_guard(void) {
    sigset_t blocked, pending;
    if (sigfillset(&blocked) || sigprocmask(SIG_SETMASK, &blocked, NULL) || sigpending(&pending))
        return 0;
    for (int signal_number = 1; signal_number < NSIG; ++signal_number)
        if (sigismember(&pending, signal_number) == 1)
            return 0;
    for (int which = ITIMER_REAL; which <= ITIMER_PROF; ++which) {
        struct itimerval timer;
        if (getitimer(which, &timer) || timer.it_value.tv_sec || timer.it_value.tv_usec ||
            timer.it_interval.tv_sec || timer.it_interval.tv_usec)
            return 0;
    }
    return 1;
}

static int save_image(const char *directory, const char *name, const unsigned char *image) {
    char path[4096];
    int length = snprintf(path, sizeof(path), "%s/%s.bin", directory, name);
    if (length < 0 || (size_t)length >= sizeof(path))
        return 0;
    FILE *stream = fopen(path, "wb");
    if (!stream)
        return 0;
    int written = fwrite(image, 1, IMAGE_SIZE, stream) == IMAGE_SIZE;
    int closed = fclose(stream) == 0;
    return written && closed;
}

int main(int argc, char **argv) {
    if (argc != 4 || (strcmp(argv[1], "capture") && strcmp(argv[1], "restore")) ||
        (strcmp(argv[2], "zero") && strcmp(argv[2], "nonzero")))
        return 64;
    int restore = strcmp(argv[1], "restore") == 0;
    int nonzero = strcmp(argv[2], "nonzero") == 0;
    printf("mode=%s profile=%s pid=%ld image_size=%d mask=0x2e7\n",
           argv[1], argv[2], (long)getpid(), IMAGE_SIZE);
    if (!cpu_guard() || !quiet_guard()) {
        puts("qualification=unsupported_cpu_or_async_state");
        return 78;
    }
    unsigned char *before = NULL, *after = NULL;
    if (posix_memalign((void **)&before, 64, IMAGE_SIZE) ||
        posix_memalign((void **)&after, 64, IMAGE_SIZE))
        return 74;
    memset(before, 0, IMAGE_SIZE);
    memset(after, 0, IMAGE_SIZE);
    if (((uintptr_t)before % 64) || ((uintptr_t)after % 64))
        return 78;
    struct rusage usage_before, usage_after;
    if (getrusage(RUSAGE_SELF, &usage_before))
        return 74;
    capture_pair(before, after, nonzero, restore);
    if (getrusage(RUSAGE_SELF, &usage_after))
        return 74;
    int quiet = quiet_guard();
    if (!save_image(argv[3], "before", before) || !save_image(argv[3], "after", after))
        return 74;
    printf("before_xstate_bv=0x%" PRIx64 " after_xstate_bv=0x%" PRIx64 "\n",
           word(before, 512), word(after, 512));
    printf("before_xcomp_bv=0x%" PRIx64 " after_xcomp_bv=0x%" PRIx64 "\n",
           word(before, 520), word(after, 520));
    printf("voluntary_switches=%ld involuntary_switches=%ld quiet_after=%d\n",
           usage_after.ru_nvcsw - usage_before.ru_nvcsw,
           usage_after.ru_nivcsw - usage_before.ru_nivcsw, quiet);
    int high_before = 1, high_after = 1;
    for (size_t offset = HIGH_OFFSET; offset < HIGH_OFFSET + HIGH_SIZE; ++offset) {
        high_before &= before[offset] == (nonzero ? 0xff : 0);
        high_after &= after[offset] == (nonzero ? 0xff : 0);
    }
    size_t differences = 0;
    for (size_t offset = 0; offset < IMAGE_SIZE; ++offset) {
        if (before[offset] != after[offset]) {
            printf("difference offset=%zu before=0x%02x after=0x%02x\n",
                   offset, before[offset], after[offset]);
            ++differences;
        }
    }
    int qualified = word(before, 512) == 0x2a7 && word(before, 520) == 0 && high_before && quiet;
    printf("high_before_matches=%d high_after_matches=%d qualified=%d differences=%zu whole_image_equal=%d\n",
           high_before, high_after, qualified, differences, differences == 0);
    free(before);
    free(after);
    return !qualified ? 78 : (differences ? 1 : 0);
}
