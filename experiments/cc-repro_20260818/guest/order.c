/* Records the ORDER of guest events across two vCPUs, not just lost updates.
 *
 * Two processes on a shared page take turns appending their own id to a shared
 * trace using an atomic fetch-and-add index. The resulting string IS the
 * interleaving. Unlike a lost-update counter this registers even when the two
 * never overlap in real time, so "no overlap" and "different interleaving" can
 * be told apart.
 */
typedef unsigned long u64;
typedef long s64;

static inline s64 sys(s64 n, s64 a, s64 b, s64 c)
{
    s64 r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c)
                     : "rcx", "r11", "memory");
    return r;
}
static inline s64 sys6(s64 n, s64 a, s64 b, s64 c, s64 d, s64 e, s64 f)
{
    s64 r;
    register s64 r10 __asm__("r10") = d;
    register s64 r8 __asm__("r8") = e;
    register s64 r9 __asm__("r9") = f;
    __asm__ volatile("syscall" : "=a"(r)
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
    while (s[n]) n++;
    sys(SYS_write, 1, (s64)s, n);
}
static void putu(u64 v)
{
    char b[24]; int i = 23; b[i--] = 0;
    if (!v) b[i--] = '0';
    while (v) { b[i--] = '0' + (v % 10); v /= 10; }
    sys(SYS_write, 1, (s64)&b[i + 1], 22 - i);
}

#define SLOTS 2000
#define SPIN 4000

struct shm { volatile u64 idx; volatile unsigned char trace[SLOTS]; };

void _start(void)
{
    struct shm *m = (struct shm *)sys6(SYS_mmap, 0, 65536, 3, 0x21, -1, 0);
    m->idx = 0;

    s64 pid = sys(SYS_fork, 0, 0, 0);
    unsigned long mask = pid == 0 ? 2UL : 1UL;
    sys(SYS_sched_setaffinity, 0, sizeof(mask), (s64)&mask);
    unsigned char me = pid == 0 ? 'B' : 'A';

    for (int k = 0; k < SLOTS / 2; k++) {
        u64 slot = __atomic_fetch_add(&m->idx, 1, __ATOMIC_SEQ_CST);
        if (slot < SLOTS) m->trace[slot] = me;
        for (volatile int z = 0; z < SPIN; z++) { }
    }

    if (pid == 0) sys(SYS_exit, 0, 0, 0);
    sys6(SYS_wait4, pid, 0, 0, 0, 0, 0);

    /* Summarise: how many times the trace switches between A and B. */
    u64 n = m->idx < SLOTS ? m->idx : SLOTS, sw = 0, a = 0, b = 0;
    for (u64 i = 0; i < n; i++) {
        if (m->trace[i] == 'A') a++; else if (m->trace[i] == 'B') b++;
        if (i && m->trace[i] != m->trace[i - 1]) sw++;
    }
    put("ORDER slots="); putu(n);
    put(" A="); putu(a);
    put(" B="); putu(b);
    put(" switches="); putu(sw);
    put(" head=");
    sys(SYS_write, 1, (s64)m->trace, n < 80 ? n : 80);
    put("\n");
    sys(SYS_exit, 0, 0, 0);
}
