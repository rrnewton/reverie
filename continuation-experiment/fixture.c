/* Standalone controlled native experiment. Not linked into Reverie. */
#define _GNU_SOURCE
#include "abi.h"
#include <cpuid.h>
#include <errno.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <ucontext.h>
#include <unistd.h>

#ifndef SYS_SECCOMP
#define SYS_SECCOMP 1
#endif
#define COOKIE 0x4c49U
#define SA_RESTORER_LOCAL 0x04000000UL
#define PKRU_BIT (UINT64_C(1) << 9)
#define STACK_SIZE (256U * 1024U)
#define MAGIC1 0x46505853U
#define MAGIC2 0x46505845U
#define DF (UINT64_C(1) << 10)
#define ARITH_FLAGS UINT64_C(0xed7)

struct effect_result { long result; uint32_t pkru; uint32_t padding; };
extern long trusted_call(long, long, long, long, long, long, long);
extern struct effect_result guest_effect(long, const uint64_t *, uint32_t);
extern void enter_guest(void *);
extern void clobber_guest_fp(int, int);
extern void signal_entry(int, siginfo_t *, void *);
extern void signal_restorer(void);
extern void callback_entry(void);
extern void completion_request(void);
extern void wrong_site(void);
extern unsigned char guest_syscall[], guest_resume[], completion_syscall[];
extern unsigned char completion_return[], trusted_return[], effect_return[];
extern unsigned char restorer_syscall[];
uint64_t restorer_requests;

struct region { uintptr_t lo, hi; };
struct component { uint32_t size, offset; };
struct frame_record {
    uint64_t address, fp_address, handler_sp, uc_flags, mask;
    stack_t stack;
    uint32_t size, extended;
    uint64_t features, present;
    uint64_t gregs[19];
    _Alignas(64) unsigned char fp[XSAVE_CAPACITY];
};
struct machine {
    uint64_t f[64];
    _Alignas(64) unsigned char observed[XSAVE_CAPACITY];
    struct frame_record entry, completion;
    struct component components[64];
    struct region control, guest, observer, callback, alt[2];
    uint64_t owner_tid, generation, phase, handler_entries, entry_accepted;
    uint64_t completion_accepted, callback_observed, callback_finished;
    uint64_t callback_sp, callback_pkru, callback_flags, post_pkru;
    uint64_t completion_metadata_preserved, completion_original_flags;
    uint64_t final_mask, final_alt_sp, final_alt_size, final_alt_flags;
    uint64_t stack_mapping_none, real_effects, native_mode, case_kind;
    uint64_t cpuid_xsave_size, observer_key, pid, seeded_features, omit_xstate;
    uint64_t rseq_initial_size, rseq_area, rseq_unregistered, rseq_registered_after;
    long effect_value;
};
static struct machine *m;
static unsigned char output[800000];
static size_t output_used;

_Static_assert(offsetof(struct machine, observed) == XSAVE_OFFSET, "assembly XSAVE offset");
_Static_assert(offsetof(ucontext_t, uc_flags) == 0, "LP64 uc_flags");
_Static_assert(offsetof(ucontext_t, uc_stack) == 16, "LP64 uc_stack");
_Static_assert(offsetof(ucontext_t, uc_mcontext) == 40, "LP64 mcontext");
_Static_assert(offsetof(ucontext_t, uc_sigmask) == 296, "kernel mask offset");
_Static_assert(offsetof(mcontext_t, fpregs) == 184, "kernel FP pointer");
_Static_assert(REG_RSP == 15 && REG_RIP == 16 && REG_EFL == 17 && REG_CSGSFS == 18, "greg order");
_Static_assert(sizeof(stack_t) == 24 && sizeof(greg_t) == 8, "LP64 ABI");
_Static_assert(sizeof(struct effect_result) == 16 && _Alignof(struct effect_result) == 8, "two integer gate ABI");

static long raw(long nr, long a, long b, long c, long d, long e, long f) {
    return trusted_call(nr, a, b, c, d, e, f);
}
static void copy_bytes(void *to, const void *from, size_t n) {
    unsigned char *d = to; const unsigned char *s = from;
    for (size_t i = 0; i < n; ++i) d[i] = s[i];
}
static int equal_bytes(const void *a, const void *b, size_t n) {
    const unsigned char *x = a, *y = b;
    for (size_t i = 0; i < n; ++i) if (x[i] != y[i]) return 0;
    return 1;
}
static uint32_t get32(const void *p) { uint32_t v; copy_bytes(&v, p, 4); return v; }
static uint64_t get64(const void *p) { uint64_t v; copy_bytes(&v, p, 8); return v; }
static void put32(void *p, uint32_t v) { copy_bytes(p, &v, 4); }
static void put64(void *p, uint64_t v) { copy_bytes(p, &v, 8); }
static uint32_t rdpkru(void) {
    uint32_t a, d;
    __asm__ volatile("rdpkru" : "=a"(a), "=d"(d) : "c"(0) : "memory");
    return a;
}
static uint64_t flags(void) {
    uint64_t value; __asm__ volatile("pushfq; pop %0" : "=r"(value) : : "memory"); return value;
}
static void text(const char *s) {
    while (*s) { if (output_used == sizeof(output)) __builtin_trap(); output[output_used++] = (unsigned char)*s++; }
}
static void number(uint64_t n) {
    char digits[24]; size_t count = 0;
    do { digits[count++] = (char)('0' + n % 10); n /= 10; } while (n);
    while (count) { char one[2] = {digits[--count], 0}; text(one); }
}
static void field(const char *name, uint64_t value) { text("\""); text(name); text("\":"); number(value); }
static void hex_bytes(const unsigned char *p, size_t n) {
    const char *digits = "0123456789abcdef";
    text("\"");
    for (size_t i = 0; i < n; ++i) { char h[3] = {digits[p[i] >> 4], digits[p[i] & 15], 0}; text(h); }
    text("\"");
}
static void flush(void) {
    size_t done = 0;
    while (done < output_used) {
        long n = raw(SYS_write, 1, (long)(output + done), (long)(output_used - done), 0, 0, 0);
        if (n <= 0) break;
        done += (size_t)n;
    }
}
__attribute__((noreturn)) static void fail(unsigned code) {
    output_used = 0; text("{"); field("fatal", code);
    if (m) {
        text(","); field("handler_entries", m->handler_entries);
        text(","); field("restorer_requests", restorer_requests);
        text(","); field("callback_observed", m->callback_observed);
        text(","); field("guest_observed", m->f[F_GUEST_REACHED]);
        text(","); field("phase", m->phase);
    }
    text("}\n"); flush(); raw(SYS_exit_group, code == 77 ? 77 : 90, 0, 0, 0, 0, 0); __builtin_unreachable();
}
static void require(int condition, unsigned code) { if (!condition) fail(code); }
static int contains(struct region r, uintptr_t address, size_t n) {
    return address >= r.lo && address <= r.hi && n <= r.hi - address;
}
static uint64_t current_mask(void) {
    uint64_t value = 0;
    require(raw(SYS_rt_sigprocmask, SIG_SETMASK, 0, (long)&value, 8, 0, 0) == 0, 101);
    return value;
}
static void set_current_metadata(void) {
    /* Completion must preserve metadata that differs from the entry frame. */
    uint64_t mask = UINT64_C(1) << (SIGUSR2 - 1);
    stack_t stack = { .ss_sp = (void *)m->alt[1].lo, .ss_flags = 0, .ss_size = STACK_SIZE };
    require(raw(SYS_rt_sigprocmask, SIG_SETMASK, (long)&mask, 0, 8, 0, 0) == 0, 102);
    require(raw(SYS_sigaltstack, (long)&stack, 0, 0, 0, 0, 0) == 0, 103);
}
static uint32_t frame_pkru(const unsigned char *fp) {
    return (get64(fp + 512) & PKRU_BIT) ? get32(fp + m->components[9].offset) : 0;
}
static void set_frame_pkru(unsigned char *fp, uint32_t value) {
    put32(fp + m->components[9].offset, value);
    /* Padding belongs to the existing fresh frame; it is not PKRU rights. */
    put64(fp + 512, get64(fp + 512) | PKRU_BIT);
}
static void read_frame(ucontext_t *uc, struct frame_record *record, unsigned alt_index) {
    uintptr_t local = (uintptr_t)&local;
    struct region alt = m->alt[alt_index];
    uintptr_t fp = (uintptr_t)uc->uc_mcontext.fpregs;
    require(contains(alt, local, sizeof(local)), 110);
    require(contains(alt, (uintptr_t)uc - 8, 440), 111);
    require(contains(alt, fp, 576) && (fp & 63) == 0, 112);
    unsigned char *p = (unsigned char *)fp;
    uint32_t size = get32(p + 480), extended = get32(p + 468);
    uint64_t features = get64(p + 472), present = get64(p + 512);
    require(get32(p + 464) == MAGIC1 && size >= 576 && size <= XSAVE_CAPACITY - 4, 113);
    require(extended == size + 4 && contains(alt, fp, extended) && get32(p + size) == MAGIC2, 114);
    require((features & ~m->f[F_XCR0]) == 0 && (features & PKRU_BIT) != 0, 115);
    require((present & ~features) == 0 && get64(p + 520) == 0, 116);
    for (unsigned i = 528; i < 576; i += 8) require(get64(p + i) == 0, 117);
    for (unsigned i = 2; i < 64; ++i) if (features & (UINT64_C(1) << i)) {
        struct component c = m->components[i];
        require(c.size && c.offset >= 576 && c.offset <= size && c.size <= size - c.offset, 118);
    }
    record->address = (uintptr_t)uc - 8; record->fp_address = fp; record->handler_sp = local;
    record->uc_flags = uc->uc_flags; record->mask = get64(&uc->uc_sigmask); record->stack = uc->uc_stack;
    record->size = size; record->extended = extended; record->features = features; record->present = present;
    for (unsigned i = 0; i < 19; ++i) record->gregs[i] = (uint64_t)uc->uc_mcontext.gregs[i];
    copy_bytes(record->fp, p, extended);
}
static void restore_payload(unsigned char *fresh) {
    if (m->omit_xstate) {
        /* Negative control: retain actual clobbered callback FP state. */
        set_frame_pkru(fresh, (uint32_t)m->post_pkru);
        return;
    }
    const unsigned char *saved = m->entry.fp;
    uint64_t available = get64(fresh + 472);
    uint64_t active = get64(saved + 512);
    require((active & ~available) == 0, 120);
    /* Architectural legacy bytes only: software/reserved frame metadata stays fresh. */
    copy_bytes(fresh, saved, 416);
    for (unsigned i = 2; i < 64; ++i) if (available & (UINT64_C(1) << i)) {
        struct component c = m->components[i];
        if (active & (UINT64_C(1) << i)) copy_bytes(fresh + c.offset, saved + c.offset, c.size);
        /* Inactive payload bytes are ignored by XRSTOR with this bit cleared. */
    }
    put64(fresh + 512, active);
    set_frame_pkru(fresh, (uint32_t)m->post_pkru);
}

void handle_sigsys(int sig, siginfo_t *info, void *context) {
    ucontext_t *uc = context;
    ++m->handler_entries;
    require(sig == SIGSYS && info->si_signo == SIGSYS && info->si_code == SYS_SECCOMP, 130);
    require(info->si_arch == AUDIT_ARCH_X86_64 && (unsigned)info->si_errno == COOKIE, 131);
    require((uint64_t)raw(SYS_gettid, 0, 0, 0, 0, 0, 0) == m->owner_tid && m->generation == 1, 132);
    require(contains(m->alt[0], (uintptr_t)uc - 8, 440) || contains(m->alt[1], (uintptr_t)uc - 8, 440), 217);
    require((uintptr_t)info == (uintptr_t)uc + 304, 153);
    require(get64((unsigned char *)uc - 8) == (uintptr_t)signal_restorer, 154);
    uintptr_t ip = (uintptr_t)uc->uc_mcontext.gregs[REG_RIP];
    require((uintptr_t)info->si_call_addr == ip, 133);
    if (m->phase == 1) {
        require(ip == (uintptr_t)guest_resume && info->si_syscall == (int)m->f[F_NR], 134);
        require(guest_resume == guest_syscall + 2 && guest_syscall[0] == 0x0f && guest_syscall[1] == 0x05, 135);
        require((uint64_t)uc->uc_mcontext.gregs[REG_RAX] == m->f[F_NR], 136);
        read_frame(uc, &m->entry, 0);
        require(frame_pkru(m->entry.fp) == m->f[F_GUEST_PKRU], 137);
        require(m->entry.mask == (UINT64_C(1) << (SIGUSR1 - 1)), 138);
        require(m->entry.gregs[REG_RSP] == m->f[F_GUEST_SP], 139);
        require((m->entry.gregs[REG_EFL] & DF) != 0, 140);
        ++m->entry_accepted; m->phase = 2;
        uc->uc_mcontext.gregs[REG_RIP] = (greg_t)(uintptr_t)callback_entry;
        uc->uc_mcontext.gregs[REG_RSP] = (greg_t)((m->callback.hi - 64) & ~(uintptr_t)15);
        /* The saved guest DF remains one; the ordinary SysV C callback requires zero. */
        uc->uc_mcontext.gregs[REG_EFL] &= ~(greg_t)DF;
        set_frame_pkru((unsigned char *)uc->uc_mcontext.fpregs, 0);
        return;
    }
    require(m->phase == 3, 141);
    require(ip == (uintptr_t)completion_return && info->si_syscall == SYS_getpid, 142);
    require(completion_return == completion_syscall + 2 && completion_syscall[0] == 0x0f && completion_syscall[1] == 0x05, 143);
    require(uc->uc_mcontext.gregs[REG_RAX] == SYS_getpid && m->callback_finished == 1, 144);
    read_frame(uc, &m->completion, 1);
    require(m->completion.address != m->entry.address && m->completion.fp_address != m->entry.fp_address, 145);
    require(m->completion.mask == (UINT64_C(1) << (SIGUSR2 - 1)), 146);
    require((uintptr_t)m->completion.stack.ss_sp == m->alt[1].lo && m->completion.stack.ss_size == STACK_SIZE, 147);
    require(frame_pkru(m->completion.fp) == 0, 148);
    unsigned char metadata[304]; copy_bytes(metadata, uc, sizeof(metadata));
    unsigned char *fp = (unsigned char *)uc->uc_mcontext.fpregs;
    m->completion_original_flags = (uint64_t)uc->uc_mcontext.gregs[REG_EFL];
    restore_payload(fp);
    for (unsigned i = 0; i < 19; ++i) uc->uc_mcontext.gregs[i] = (greg_t)m->entry.gregs[i];
    uc->uc_mcontext.gregs[REG_RAX] = m->effect_value;
    /* SYSCALL already placed the return IP and flags in saved RCX/R11.
     * Preserve those actual saved registers; the native comparator checks them. */
    require(equal_bytes(metadata, uc, 40), 149);
    require(equal_bytes(metadata + 40 + 19 * 8, (unsigned char *)uc + 40 + 19 * 8, 304 - 40 - 19 * 8), 150);
    require(equal_bytes(m->completion.fp + 464, fp + 464, 48), 151);
    require(equal_bytes(m->completion.fp + 520, fp + 520, 56) && get32(fp + m->completion.size) == MAGIC2, 152);
    m->completion_metadata_preserved = 1; ++m->completion_accepted; m->phase = 4;
}

void ordinary_callback(void) {
    uintptr_t local = (uintptr_t)&local;
    require(m->phase == 2 && m->entry_accepted == 1 && restorer_requests == 1, 160);
    require(contains(m->callback, local, sizeof(local)), 161);
    m->callback_sp = local; m->callback_pkru = rdpkru(); m->callback_flags = flags();
    require(m->callback_pkru == 0 && (m->callback_flags & DF) == 0, 162);
    ++m->callback_observed; /* Control reached after the first real restorer request. */
    struct effect_result result = guest_effect((long)m->f[F_NR], &m->f[F_ARGS], (uint32_t)m->f[F_GUEST_PKRU]);
    ++m->real_effects; m->effect_value = result.result; m->post_pkru = result.pkru;
    require(result.pkru == m->f[F_GUEST_PKRU] && rdpkru() == 0, 163);
    set_current_metadata();
    clobber_guest_fp((int)m->f[F_AVX], (int)m->f[F_AVX512]);
    /* No work is pending when this normal C frame returns to completion_request. */
    m->callback_finished = 1; m->phase = 3;
}

static struct region allocate_region(size_t size, int key) {
    size_t page = 4096;
    void *p = mmap(NULL, size + 2 * page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    require(p != MAP_FAILED, 170);
    uintptr_t lo = (uintptr_t)p + page;
    require(raw(SYS_pkey_mprotect, (long)lo, (long)size, PROT_READ | PROT_WRITE, key, 0, 0) == 0, 171);
    for (size_t i = 0; i < size; i += page) *(volatile unsigned char *)(lo + i) = 0;
    return (struct region){lo, lo + size};
}
static void setup_hardware(void) {
    unsigned a, b, c, d;
    require(__get_cpuid_max(0, NULL) >= 0xd, 77);
    __cpuid_count(1, 0, a, b, c, d);
    require((c & ((1U << 26) | (1U << 27))) == ((1U << 26) | (1U << 27)), 77);
    __cpuid_count(7, 0, a, b, c, d);
    require((c & (1U << 4)) != 0, 77);
    int avx512f = (b & (1U << 16)) != 0;
    int avx512bw = (b & (1U << 30)) != 0; /* KMOVQ/KXORQ */
    uint32_t xlo, xhi;
    __asm__ volatile("xgetbv" : "=a"(xlo), "=d"(xhi) : "c"(0));
    uint64_t xcr0 = xlo | ((uint64_t)xhi << 32);
    require((xcr0 & (PKRU_BIT | 3)) == (PKRU_BIT | 3), 77);
    __cpuid_count(0xd, 0, a, b, c, d);
    require(b <= XSAVE_CAPACITY && b >= 576, 77);
    m->cpuid_xsave_size = b; m->f[F_XCR0] = xcr0;
    m->f[F_AVX] = (xcr0 & 7) == 7;
    m->f[F_AVX512] = avx512f && avx512bw && (xcr0 & 0xe7) == 0xe7;
    for (unsigned i = 2; i < 64; ++i) if (xcr0 & (UINT64_C(1) << i)) {
        __cpuid_count(0xd, i, a, b, c, d);
        require((c & 1) == 0 && a && b >= 576 && b <= XSAVE_CAPACITY && a <= XSAVE_CAPACITY - b, 77);
        m->components[i] = (struct component){a, b};
    }
    require(m->components[9].size == 8, 77);
    m->seeded_features = 3 | (m->f[F_AVX] ? 4 : 0) | (m->f[F_AVX512] ? 0xe0 : 0) | PKRU_BIT;
}
static void unregister_own_rseq(void) {
    /* Same environmental isolation as the existing protected-stack fixture:
     * no kernel rseq write into denied key-zero TLS while measuring PKRU. */
    const ptrdiff_t *offset = dlsym(RTLD_DEFAULT, "__rseq_offset");
    const unsigned *size = dlsym(RTLD_DEFAULT, "__rseq_size");
    require(offset != NULL && size != NULL, 210);
    m->rseq_initial_size = *size;
    if (!*size) return;
    require(*size <= 32, 211);
    uintptr_t fs = 0;
    require(raw(SYS_arch_prctl, 0x1003, (long)&fs, 0, 0, 0, 0) == 0, 212);
    ptrdiff_t off = *offset;
    uintptr_t area;
    if (off < 0) {
        uint64_t magnitude = (uint64_t)(-(off + 1)) + 1;
        require(fs >= magnitude, 213); area = fs - magnitude;
    } else {
        require((uint64_t)off <= UINTPTR_MAX - fs, 214); area = fs + (uint64_t)off;
    }
    require(raw(SYS_rseq, (long)area, 32, 1, 0x53053053, 0, 0) == 0, 215);
    m->rseq_area = area; m->rseq_unregistered = 1;
}
static void restore_own_rseq(void) {
    if (!m->rseq_unregistered) return;
    require(raw(SYS_rseq, (long)m->rseq_area, 32, 0, 0x53053053, 0, 0) == 0, 216);
    m->rseq_registered_after = 1;
}
static void install_filter(void) {
    uintptr_t trusted = (uintptr_t)trusted_return, effect = (uintptr_t)effect_return;
    struct sock_filter code[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_rt_sigreturn, 8, 0),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, instruction_pointer) + 4),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, (uint32_t)(trusted >> 32), 0, 2),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, instruction_pointer)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, (uint32_t)trusted, 4, 0),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, instruction_pointer) + 4),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, (uint32_t)(effect >> 32), 0, 3),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, instruction_pointer)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, (uint32_t)effect, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP | COOKIE),
    };
    struct sock_fprog prog = {(unsigned short)(sizeof(code) / sizeof(code[0])), code};
    require(raw(SYS_prctl, PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0, 0) == 0, 180);
    require(raw(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, (long)&prog, 0, 0, 0) == 0, 181);
}
static void install_signal(void) {
    struct kernel_action { void (*handler)(int, siginfo_t *, void *); unsigned long flags; void (*restorer)(void); uint64_t mask; } action;
    action.handler = signal_entry; action.flags = SA_SIGINFO | SA_ONSTACK | SA_RESTORER_LOCAL;
    action.restorer = signal_restorer; action.mask = 0;
    require(raw(SYS_rt_sigaction, SIGSYS, (long)&action, 0, 8, 0, 0) == 0, 182);
    stack_t stack = {.ss_sp = (void *)m->alt[0].lo, .ss_size = STACK_SIZE, .ss_flags = 0};
    require(raw(SYS_sigaltstack, (long)&stack, 0, 0, 0, 0, 0) == 0, 183);
    uint64_t mask = UINT64_C(1) << (SIGUSR1 - 1);
    require(raw(SYS_rt_sigprocmask, SIG_SETMASK, (long)&mask, 0, 8, 0, 0) == 0, 184);
}
static int original_stack_is_none(void) {
    /* Actual kernel mapping permissions, not merely the mprotect return scalar. */
    char maps[65536];
    long fd = raw(SYS_openat, AT_FDCWD, (long)"/proc/self/maps", O_RDONLY | O_CLOEXEC, 0, 0, 0);
    require(fd >= 0, 190);
    long used = raw(SYS_read, fd, (long)maps, sizeof(maps) - 1, 0, 0, 0);
    require(used > 0 && used < (long)sizeof(maps) - 1, 191);
    char extra; require(raw(SYS_read, fd, (long)&extra, 1, 0, 0, 0) == 0, 192);
    require(raw(SYS_close, fd, 0, 0, 0, 0, 0) == 0, 193);
    maps[used] = 0;
    const char *p = maps;
    while (*p) {
        uintptr_t lo = 0, hi = 0;
        while ((*p >= '0' && *p <= '9') || (*p >= 'a' && *p <= 'f')) { lo = lo * 16 + (unsigned)(*p <= '9' ? *p - '0' : *p - 'a' + 10); ++p; }
        require(*p++ == '-', 194);
        while ((*p >= '0' && *p <= '9') || (*p >= 'a' && *p <= 'f')) { hi = hi * 16 + (unsigned)(*p <= '9' ? *p - '0' : *p - 'a' + 10); ++p; }
        require(*p++ == ' ', 195);
        if (lo <= m->guest.lo && hi >= m->guest.hi) return p[0] == '-' && p[1] == '-' && p[2] == '-';
        while (*p && *p != '\n') ++p;
        if (*p) ++p;
    }
    return 0;
}
static void array(const uint64_t *values, size_t n) {
    text("["); for (size_t i = 0; i < n; ++i) { if (i) text(","); number(values[i]); } text("]");
}
static void region_report(const char *name, struct region r) {
    text("\""); text(name); text("\":["); number(r.lo); text(","); number(r.hi); text("]");
}
static void frame_report(const char *name, const struct frame_record *f) {
    text("\""); text(name); text("\":{"); field("address", f->address); text(","); field("fp_address", f->fp_address);
    text(","); field("handler_sp", f->handler_sp); text(","); field("uc_flags", f->uc_flags);
    text(","); field("mask", f->mask); text(","); field("alt_sp", (uintptr_t)f->stack.ss_sp);
    text(","); field("alt_size", f->stack.ss_size); text(","); field("alt_flags", (unsigned)f->stack.ss_flags);
    text(","); field("size", f->size); text(","); field("extended", f->extended);
    text(","); field("features", f->features); text(","); field("present", f->present);
    text(",\"gregs\":"); array(f->gregs, 19); text(",\"fp_hex\":"); hex_bytes(f->fp, f->extended); text("}");
}
static void report(void) {
    text("{\"schema\":1,"); field("native", m->native_mode); text(","); field("case_kind", m->case_kind);
#define REPORT(name) do { text(","); field(#name, m->name); } while (0)
    REPORT(pid); REPORT(owner_tid); REPORT(generation); REPORT(phase); REPORT(handler_entries);
    text(","); field("restorer_requests", restorer_requests);
    REPORT(entry_accepted); REPORT(completion_accepted); REPORT(callback_observed); REPORT(callback_finished);
    REPORT(callback_sp); REPORT(callback_pkru); REPORT(callback_flags); REPORT(post_pkru);
    REPORT(real_effects); REPORT(completion_metadata_preserved); REPORT(completion_original_flags);
    REPORT(final_mask); REPORT(final_alt_sp); REPORT(final_alt_size); REPORT(final_alt_flags);
    REPORT(stack_mapping_none); REPORT(observer_key); REPORT(cpuid_xsave_size); REPORT(seeded_features); REPORT(omit_xstate);
    REPORT(rseq_initial_size); REPORT(rseq_area); REPORT(rseq_unregistered); REPORT(rseq_registered_after);
    text(","); field("effect_value", (uint64_t)m->effect_value);
    text(",\"fields\":"); array(m->f, 64);
    text(",\"regions\":{"); region_report("control", m->control); text(","); region_report("guest", m->guest);
    text(","); region_report("observer", m->observer); text(","); region_report("callback", m->callback);
    text(","); region_report("alt0", m->alt[0]); text(","); region_report("alt1", m->alt[1]); text("}");
    text(",\"ips\":{"); field("guest_syscall", (uintptr_t)guest_syscall); text(","); field("guest_resume", (uintptr_t)guest_resume);
    text(","); field("callback_entry", (uintptr_t)callback_entry); text(","); field("completion_syscall", (uintptr_t)completion_syscall);
    text(","); field("completion_return", (uintptr_t)completion_return); text(","); field("restorer_syscall", (uintptr_t)restorer_syscall); text("}");
    text(",\"components\":[");
    for (unsigned i = 0; i < 64; ++i) { if (i) text(","); text("["); number(m->components[i].offset); text(","); number(m->components[i].size); text("]"); }
    text("],\"observed_xsave_hex\":"); hex_bytes(m->observed, m->cpuid_xsave_size);
    text(","); frame_report("entry", &m->entry); text(","); frame_report("completion", &m->completion); text("}\n"); flush();
}
int main(int argc, char **argv) {
    if (argc != 3) return 64;
    int omit_xstate = !strcmp(argv[1], "intercepted-omit-xstate");
    int native = !strcmp(argv[1], "native"), intercepted = !strcmp(argv[1], "intercepted") || omit_xstate;
    if (!native && !intercepted) return 64;
    unsigned kind = !strcmp(argv[2], "getpid-open") ? 0 : !strcmp(argv[2], "getpid-denied") ? 1 : !strcmp(argv[2], "revoke-stack") ? 2 : !strcmp(argv[2], "wrong-phase") ? 3 : !strcmp(argv[2], "spoof-signal") ? 4 : 99;
    if (kind == 99 || ((native || omit_xstate) && kind >= 3)) return 64;
    /* All allocation, CPUID, prefaulting and signal installation precede filtering. */
    long key = raw(SYS_pkey_alloc, 0, 0, 0, 0, 0, 0); require(key > 0 && key < 16, 77);
    size_t machine_size = (sizeof(struct machine) + 4095) & ~(size_t)4095;
    struct region control = allocate_region(machine_size, (int)key);
    m = (struct machine *)control.lo; m->control = control; m->observer_key = (uint64_t)key;
    setup_hardware(); m->native_mode = (uint64_t)native; m->case_kind = kind; m->omit_xstate = (uint64_t)omit_xstate;
    m->guest = allocate_region(STACK_SIZE, 0); m->observer = allocate_region(STACK_SIZE, (int)key);
    m->callback = allocate_region(STACK_SIZE, 0); m->alt[0] = allocate_region(STACK_SIZE, 0); m->alt[1] = allocate_region(STACK_SIZE, 0);
    m->pid = (uint64_t)raw(SYS_getpid, 0, 0, 0, 0, 0, 0); m->owner_tid = (uint64_t)raw(SYS_gettid, 0, 0, 0, 0, 0, 0);
    m->generation = 1; m->f[F_HOST_PKRU] = rdpkru();
    m->f[F_GUEST_PKRU] = kind == 1 ? 3 : 0;
    m->f[F_GUEST_SP] = (m->guest.hi - 64) & ~(uintptr_t)15;
    m->f[F_OBSERVER_SP] = (m->observer.hi - 64) & ~(uintptr_t)15;
    m->f[F_NR] = kind == 2 ? SYS_mprotect : SYS_getpid;
    for (unsigned i = 0; i < 6; ++i) m->f[F_ARGS + i] = UINT64_C(0x1010101010101010) + i;
    if (kind == 2) { m->f[F_ARGS] = m->guest.lo; m->f[F_ARGS + 1] = STACK_SIZE; m->f[F_ARGS + 2] = PROT_NONE; }
    m->f[F_MXCSR] = 0x3f80; m->f[F_X87CW] = 0x77f;
    for (unsigned i = 0; i < 64; ++i) ((unsigned char *)m)[SEED_OFFSET + i] = (unsigned char)(0x51 + i);
    unregister_own_rseq();
    install_signal();
    if (native) set_current_metadata();
    if (intercepted) install_filter();
    if (kind == 3) completion_request();
    if (kind == 4) { raw(SYS_tgkill, (long)m->pid, (long)m->owner_tid, SIGSYS, 0, 0, 0); fail(196); }
    m->phase = 1; enter_guest(m);
    require(m->f[F_GUEST_REACHED] == 1 && m->f[F_OBSERVED_PKRU] == m->f[F_GUEST_PKRU], 197);
    require((m->f[F_REGS + REG_EFL] & ARITH_FLAGS) == ARITH_FLAGS, 198);
    require(m->f[F_REGS + REG_RSP] == m->f[F_GUEST_SP], 199);
    uint64_t expected = kind == 2 ? 0 : m->pid;
    require(m->f[F_REGS + REG_RAX] == expected, 200);
    if (intercepted) require(m->handler_entries == 2 && restorer_requests == 2 && m->entry_accepted == 1 && m->completion_accepted == 1 && m->callback_observed == 1 && m->real_effects == 1 && m->phase == 4, 201);
    else require(m->handler_entries == 0 && restorer_requests == 0 && m->callback_observed == 0, 202);
    m->final_mask = current_mask();
    stack_t actual; require(raw(SYS_sigaltstack, 0, (long)&actual, 0, 0, 0, 0) == 0, 203);
    m->final_alt_sp = (uintptr_t)actual.ss_sp; m->final_alt_size = actual.ss_size; m->final_alt_flags = (unsigned)actual.ss_flags;
    require(m->final_mask == (UINT64_C(1) << (SIGUSR2 - 1)) && m->final_alt_sp == m->alt[1].lo && m->final_alt_size == STACK_SIZE && m->final_alt_flags == 0, 204);
    m->stack_mapping_none = (uint64_t)original_stack_is_none();
    require(m->stack_mapping_none == (uint64_t)(kind == 2), 205);
    restore_own_rseq();
    report(); raw(SYS_exit_group, 0, 0, 0, 0, 0, 0); __builtin_unreachable();
}
