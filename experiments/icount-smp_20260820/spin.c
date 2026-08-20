/*
 * Fixed-work guest load, freestanding.
 *
 * Two reasons it is written this way rather than as an ordinary C program:
 *   - This host has no static glibc (glibc-static is not installed), and a
 *     dynamically linked binary cannot run in this initramfs.
 *   - The prebuilt worker2 from the neighbouring experiment IS static, but it
 *     was compiled for this host's Zen 5 and dies inside the guest with
 *     "trap invalid opcode" against QEMU's default emulated CPU model, which
 *     silently turns a load arm into a zero-load arm.
 *
 * usage: spin CPU ITERATIONS      CPU < 0 means do not pin.
 */

static long sys3(long n, long a, long b, long c)
{
    long r;
    __asm__ volatile("syscall"
                     : "=a"(r)
                     : "a"(n), "D"(a), "S"(b), "d"(c)
                     : "rcx", "r11", "memory");
    return r;
}

#define SYS_write            1
#define SYS_sched_setaffinity 203
#define SYS_getcpu           309
#define SYS_exit_group       231

static unsigned long parse_ul(const char *s, int *neg)
{
    unsigned long v = 0;
    *neg = 0;
    if (*s == '-') { *neg = 1; s++; }
    while (*s >= '0' && *s <= '9') { v = v * 10 + (unsigned long)(*s - '0'); s++; }
    return v;
}

static int put_ul(char *buf, unsigned long v)
{
    char tmp[24];
    int i = 0, j = 0;
    if (v == 0) { buf[0] = '0'; return 1; }
    while (v) { tmp[i++] = (char)('0' + (v % 10)); v /= 10; }
    while (i) buf[j++] = tmp[--i];
    return j;
}

static int put_s(char *buf, const char *s)
{
    int j = 0;
    while (*s) buf[j++] = *s++;
    return j;
}

void cmain(long *sp)
{
    long argc = sp[0];
    char **argv = (char **)(sp + 1);
    char out[192];
    int j = 0, neg = 0;
    unsigned long iters, i, acc = 0;
    long cpu;
    unsigned int gotcpu = 0xffffffffu, gotnode = 0;
    unsigned long mask;

    if (argc != 3) {
        j = put_s(out, "spin: usage: spin CPU ITERATIONS\n");
        sys3(SYS_write, 2, (long)out, j);
        sys3(SYS_exit_group, 2, 0, 0);
    }

    cpu = (long)parse_ul(argv[1], &neg);
    if (neg) cpu = -1;
    iters = parse_ul(argv[2], &neg);

    if (cpu >= 0) {
        mask = 1UL << cpu;
        if (sys3(SYS_sched_setaffinity, 0, sizeof(mask), (long)&mask) != 0) {
            j = put_s(out, "spin: sched_setaffinity FAILED\n");
            sys3(SYS_write, 2, (long)out, j);
            sys3(SYS_exit_group, 1, 0, 0);
        }
    }

    for (i = 0; i < iters; i++)
        acc += i ^ (acc >> 3);

    sys3(SYS_getcpu, (long)&gotcpu, (long)&gotnode, 0);

    j  = put_s(out, "spin pinned_to=");
    if (cpu < 0) j += put_s(out + j, "none"); else j += put_ul(out + j, (unsigned long)cpu);
    j += put_s(out + j, " finished_on_cpu=");
    j += put_ul(out + j, gotcpu);
    j += put_s(out + j, " iters=");
    j += put_ul(out + j, iters);
    j += put_s(out + j, " acc=");
    j += put_ul(out + j, acc);
    j += put_s(out + j, "\n");
    sys3(SYS_write, 1, (long)out, j);
    sys3(SYS_exit_group, 0, 0, 0);
}

__asm__(".globl _start\n"
        "_start:\n"
        "\tmov %rsp, %rdi\n"
        "\tcall cmain\n"
        "\thlt\n");
