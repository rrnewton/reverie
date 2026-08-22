/* Freestanding two-clock probe: CLOCK_MONOTONIC and rdtsc, no libc.
 *
 * This host has no static glibc (the neighbouring icount-smp experiment hit
 * the same wall and solved it the same way), and the QEMU guest's busybox
 * initramfs has no dynamic loader for a normal binary, so the QEMU arm of this
 * comparison needs a binary that depends on nothing.
 *
 * Prints one line per sample: <index> <monotonic_ns> <tsc>
 * then a final line "END".
 *
 * Build: gcc -O2 -static -nostdlib -nostartfiles -ffreestanding \
 *            -o clockprobe-free clockprobe_free.c
 */

typedef unsigned long u64;
typedef long i64;

struct timespec_t {
    i64 tv_sec;
    i64 tv_nsec;
};

static inline i64 sys3(i64 number, i64 a, i64 b, i64 c)
{
    i64 result;
    __asm__ volatile("syscall"
                     : "=a"(result)
                     : "a"(number), "D"(a), "S"(b), "d"(c)
                     : "rcx", "r11", "memory");
    return result;
}

/* Raw clock_gettime(2), not the vDSO: the vDSO route would read the clock in
 * userspace and a ptrace-based tracer would never see it. Going through the
 * syscall keeps every arm of the comparison on the same path. */
static inline void monotonic(struct timespec_t *out)
{
    sys3(228 /* clock_gettime */, 1 /* CLOCK_MONOTONIC */, (i64)out, 0);
}

static inline u64 rdtsc(void)
{
    unsigned int low, high;
    __asm__ volatile("rdtsc" : "=a"(low), "=d"(high));
    return ((u64)high << 32) | low;
}

static void write_all(const char *buffer, u64 length)
{
    u64 written = 0;
    while (written < length) {
        i64 n = sys3(1 /* write */, 1 /* stdout */,
                     (i64)(buffer + written), (i64)(length - written));
        if (n <= 0)
            return;
        written += (u64)n;
    }
}

static u64 put_u64(char *out, u64 value)
{
    char digits[24];
    u64 count = 0;
    if (value == 0)
        digits[count++] = '0';
    while (value) {
        digits[count++] = (char)('0' + (value % 10));
        value /= 10;
    }
    for (u64 i = 0; i < count; i++)
        out[i] = digits[count - 1 - i];
    return count;
}

/* Fixed work between samples. `volatile` keeps the loop from being optimised
 * away; the retired-branch count is what makes the interval reproducible. */
static u64 burn(u64 iterations)
{
    volatile u64 sink = 0;
    for (u64 i = 0; i < iterations; i++)
        sink += i * 2654435761u;
    return sink;
}

void _start(void)
{
    const u64 samples = 4;
    const u64 work = 5000000;
    char line[128];

    for (u64 i = 0; i < samples; i++) {
        struct timespec_t now;
        monotonic(&now);
        u64 cycles = rdtsc();

        u64 at = 0;
        at += put_u64(line + at, i);
        line[at++] = ' ';
        at += put_u64(line + at, (u64)now.tv_sec * 1000000000UL + (u64)now.tv_nsec);
        line[at++] = ' ';
        at += put_u64(line + at, cycles);
        line[at++] = '\n';
        write_all(line, at);

        burn(work);
    }
    write_all("END\n", 4);
    sys3(60 /* exit */, 0, 0, 0);
    __builtin_unreachable();
}
