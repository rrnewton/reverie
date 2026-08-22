/* Freestanding guest-side interleaving detector (no libc available on host).
 *
 * Two processes sharing an anonymous MAP_SHARED page, each pinned to one of the
 * two guest vCPUs, hammer a shared counter with no synchronisation. The final
 * value depends on how the guest kernel interleaves them across the two CPUs.
 * Lost increments are the signal: 0 lost means no interleaving was observed,
 * a varying non-zero count means execution is being steered.
 */
typedef unsigned long u64;
typedef long s64;

static inline s64 sys(s64 n, s64 a, s64 b, s64 c)
{
    s64 r;
    __asm__ volatile("syscall"
                     : "=a"(r)
                     : "a"(n), "D"(a), "S"(b), "d"(c)
                     : "rcx", "r11", "memory");
    return r;
}
static inline s64 sys6(s64 n, s64 a, s64 b, s64 c, s64 d, s64 e, s64 f)
{
    s64 r;
    register s64 r10 __asm__("r10") = d;
    register s64 r8 __asm__("r8") = e;
    register s64 r9 __asm__("r9") = f;
    __asm__ volatile("syscall"
                     : "=a"(r)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return r;
}

#define SYS_write 1
#define SYS_mmap 9
#define SYS_fork 57
#define SYS_exit 60
#define SYS_wait4 61
#define SYS_sched_setaffinity 203

static void put(const char *s)
{
    int n = 0;
    while (s[n])
        n++;
    sys(SYS_write, 1, (s64)s, n);
}
static void putu(u64 v)
{
    char b[24];
    int i = 23;
    b[i--] = 0;
    if (!v)
        b[i--] = '0';
    while (v) {
        b[i--] = '0' + (v % 10);
        v /= 10;
    }
    sys(SYS_write, 1, (s64)&b[i + 1], 22 - i);
}

#define ITERS 5000000

void _start(void)
{
    volatile u64 *shared = (volatile u64 *)sys6(
        SYS_mmap, 0, 4096, 3 /*RW*/, 0x21 /*SHARED|ANON*/, -1, 0);
    shared[0] = 0;

    s64 pid = sys(SYS_fork, 0, 0, 0);
    unsigned long mask = pid == 0 ? 2UL : 1UL; /* child -> cpu1, parent -> cpu0 */
    sys(SYS_sched_setaffinity, 0, sizeof(mask), (s64)&mask);

    for (int i = 0; i < ITERS; i++) {
        u64 v = shared[0]; /* racy read  */
        shared[0] = v + 1; /* racy write */
    }

    if (pid == 0)
        sys(SYS_exit, 0, 0, 0);

    sys6(SYS_wait4, pid, 0, 0, 0, 0, 0);
    put("RACER shared=");
    putu(shared[0]);
    put(" expected=");
    putu((u64)ITERS * 2);
    put(" lost=");
    putu((u64)ITERS * 2 - shared[0]);
    put("\n");
    sys(SYS_exit, 0, 0, 0);
}
