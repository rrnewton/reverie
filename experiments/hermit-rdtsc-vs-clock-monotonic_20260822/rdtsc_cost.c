/* rdtsc-heavy probe: how expensive does reading the clock become?

Candidate A routes every rdtsc through a scheduler RPC. That is correctness at
a price, and the price should be measured rather than hand-waved, because
rdtsc is an instruction a guest may execute in a tight loop.

Prints the wall time (from the host's perspective it is just the process
runtime) after N rdtsc reads, plus the first and last value so the caller can
check the clock still advances.

Build: gcc -O2 -static -nostdlib -nostartfiles -ffreestanding \
           -o rdtsc-cost rdtsc_cost.c
*/

typedef unsigned long u64;
typedef long i64;

static inline i64 sys3(i64 number, i64 a, i64 b, i64 c)
{
    i64 result;
    __asm__ volatile("syscall"
                     : "=a"(result)
                     : "a"(number), "D"(a), "S"(b), "d"(c)
                     : "rcx", "r11", "memory");
    return result;
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
        i64 n = sys3(1, 1, (i64)(buffer + written), (i64)(length - written));
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

void _start(void)
{
    const u64 reads = 20000;
    u64 first = rdtsc();
    u64 last = first;
    for (u64 i = 0; i < reads; i++)
        last = rdtsc();

    char line[128];
    u64 at = 0;
    at += put_u64(line + at, reads);
    line[at++] = ' ';
    at += put_u64(line + at, first);
    line[at++] = ' ';
    at += put_u64(line + at, last);
    line[at++] = '\n';
    write_all(line, at);
    sys3(60, 0, 0, 0);
    __builtin_unreachable();
}
