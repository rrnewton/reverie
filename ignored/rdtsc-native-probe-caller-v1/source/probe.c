#define _GNU_SOURCE
#include <cpuid.h>
#include <errno.h>
#include <inttypes.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

/* One architectural case per process. No PR_SET_TSC, perf, ptrace or VM. */
struct snapshot {
    uint64_t rsp_before, rax, rdx, rcx, flags, rsp_after;
};
extern void execute_timestamp(void *, struct snapshot *, uint64_t);
__asm__(
    ".text\n"
    ".globl execute_timestamp\n"
    ".type execute_timestamp,@function\n"
    "execute_timestamp:\n"
    "pushfq\n push %r12\n push %r13\n push %r14\n"
    "mov %rsi,%r12\n mov %rdi,%r13\n mov %rdx,%r14\n"
    "mov %rsp,0(%r12)\n mov $-1,%rax\n mov $-1,%rdx\n"
    "movabs $0xfedcba9876543210,%rcx\n"
    "push %r14\n popfq\n call *%r13\n"
    "pushfq\n pop %r14\n"
    "mov %rax,8(%r12)\n mov %rdx,16(%r12)\n mov %rcx,24(%r12)\n"
    "mov %r14,32(%r12)\n mov %rsp,40(%r12)\n"
    "cld\n pop %r14\n pop %r13\n pop %r12\n popfq\n ret\n"
    ".size execute_timestamp,.-execute_timestamp\n"
);

static sigjmp_buf escape;
static volatile sig_atomic_t caught, caught_code, trap_number, rf_stage;
static volatile uint64_t saved_rip, saved_flags, saved_rax, saved_rdx, saved_rcx;
static volatile uintptr_t fault_address;
static uintptr_t instruction_start, after_instruction;
static int rf_mode;

static void handler(int signal_number, siginfo_t *info, void *opaque) {
    ucontext_t *context = opaque;
    greg_t *registers = context->uc_mcontext.gregs;
    if (rf_mode && signal_number == SIGTRAP && rf_stage == 0 &&
        (uintptr_t)registers[REG_RIP] == instruction_start &&
        registers[REG_TRAPNO] == 3) {
        /* Request RF and TF on return directly onto RDTSC/P via sigreturn. */
        rf_stage = 1;
        registers[REG_EFL] |= (1UL << 16) | (1UL << 8);
        return;
    }
    caught = signal_number;
    caught_code = info->si_code;
    trap_number = registers[REG_TRAPNO];
    saved_rip = registers[REG_RIP];
    saved_flags = registers[REG_EFL];
    saved_rax = registers[REG_RAX];
    saved_rdx = registers[REG_RDX];
    saved_rcx = registers[REG_RCX];
    fault_address = (uintptr_t)info->si_addr;
    siglongjmp(escape, 1);
}

static void require(int condition, const char *message) {
    if (!condition) {
        fprintf(stderr, "FAILED: %s\n", message);
        exit(1);
    }
}

static int has_rdtscp(void) {
    unsigned a, b, c, d;
    return __get_cpuid(0x80000001, &a, &b, &c, &d) && (d & (1U << 27));
}

static size_t append_instruction(unsigned char *at, int rdtscp) {
    at[0] = 0x0f;
    at[1] = rdtscp ? 0x01 : 0x31;
    if (rdtscp) at[2] = 0xf9;
    return rdtscp ? 3 : 2;
}

int main(int argc, char **argv) {
    require(argc == 4, "usage: probe MODE rdtsc|rdtscp PREFIX_INDEX");
    const char *mode = argv[1];
    const int rdtscp = strcmp(argv[2], "rdtscp") == 0;
    require(rdtscp || strcmp(argv[2], "rdtsc") == 0, "instruction name");
    char *end;
    errno = 0;
    long prefix_index = strtol(argv[3], &end, 10);
    require(errno == 0 && *end == 0 && prefix_index >= 0 && prefix_index <= 15,
            "prefix index");
    if (rdtscp && !has_rdtscp()) {
        fputs("UNEXECUTED: native hardware does not advertise RDTSCP\n", stderr);
        return 77;
    }
    const int positive = strcmp(mode, "positive") == 0;
    const int locked = strcmp(mode, "lock") == 0;
    const int overlength = strcmp(mode, "overlength") == 0;
    const int step = strcmp(mode, "step") == 0;
    const int edge = strcmp(mode, "edge") == 0;
    rf_mode = strcmp(mode, "rf") == 0;
    require(positive || locked || overlength || step || edge || rf_mode, "mode");
    require(positive || prefix_index == 0, "prefix argument only for positive cases");

    struct sigaction action = {.sa_sigaction = handler, .sa_flags = SA_SIGINFO};
    require(sigemptyset(&action.sa_mask) == 0, "sigemptyset");
    require(sigaction(SIGILL, &action, NULL) == 0, "SIGILL handler");
    require(sigaction(SIGSEGV, &action, NULL) == 0, "SIGSEGV handler");
    require(sigaction(SIGTRAP, &action, NULL) == 0, "SIGTRAP handler");

    long page_size = sysconf(_SC_PAGESIZE);
    require(page_size >= 4096, "page size");
    size_t length = (size_t)page_size * 3;
    unsigned char *mapping = mmap(NULL, length, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    require(mapping != MAP_FAILED, "mmap");
    unsigned char *code = mapping + page_size;
    unsigned char *at = code;
    const unsigned char enable_tf[] = {0x9c, 0x48, 0x81, 0x0c, 0x24,
                                       0x00, 0x01, 0x00, 0x00, 0x9d};
    if (edge) code = at = mapping + page_size * 2 - sizeof(enable_tf) - 2;
    if (step || edge) {
        memcpy(at, enable_tf, sizeof(enable_tf));
        at += sizeof(enable_tf);
    }
    if (rf_mode) *at++ = 0xcc; /* The first #BP arms RF and TF in sigreturn. */
    instruction_start = (uintptr_t)at;
    if (positive) {
        const unsigned char prefixes[] = {0x66, 0x67, 0xf2, 0xf3, 0x26, 0x2e,
                                          0x36, 0x3e, 0x64, 0x65, 0x40, 0x4f};
        if (prefix_index >= 1 && prefix_index <= 12)
            *at++ = prefixes[prefix_index - 1];
        else if (prefix_index == 13) {
            *at++ = 0x4f; *at++ = 0x66; *at++ = 0xf3;
        } else if (prefix_index == 14) {
            *at++ = 0xf2; *at++ = 0x67; *at++ = 0x4f;
        } else if (prefix_index == 15) {
            size_t count = 15 - (rdtscp ? 3 : 2);
            memset(at, 0x66, count); at += count;
        }
    }
    if (locked) *at++ = 0xf0;
    if (overlength) {
        size_t count = 16 - (rdtscp ? 3 : 2);
        memset(at, 0x66, count); at += count;
    }
    at += append_instruction(at, rdtscp);
    after_instruction = (uintptr_t)at;
    *at++ = 0xc3;
    require(mprotect(mapping, page_size, PROT_NONE) == 0, "first guard page");
    require(mprotect(mapping + page_size, page_size, PROT_READ | PROT_EXEC) == 0,
            "executable page");
    require(mprotect(mapping + page_size * 2, page_size, PROT_READ) == 0,
            "nonexecutable following page");

    static struct snapshot result;
    const uint64_t ordinary_flags = 0xcd7; /* CF/PF/AF/ZF/SF/DF/OF plus bit 1. */
    const uint64_t ordinary_mask = 0xcd5;
    if (sigsetjmp(escape, 1) == 0)
        execute_timestamp(code, &result, ordinary_flags);

    printf("mode=%s instruction=%s prefix=%ld signal=%d code=%d trap=%d "
           "rip_delta=%" PRId64 " fault_delta=%" PRId64 " rf_stage=%d\n",
           mode, argv[2], prefix_index, caught, caught_code, trap_number,
           caught ? (int64_t)(saved_rip - instruction_start) : 0,
           caught ? (int64_t)(fault_address - instruction_start) : 0, rf_stage);
    printf("registers rax=%016" PRIx64 " rdx=%016" PRIx64
           " rcx=%016" PRIx64 " flags=%016" PRIx64
           " rsp_before=%016" PRIx64 " rsp_after=%016" PRIx64 "\n",
           caught ? saved_rax : result.rax, caught ? saved_rdx : result.rdx,
           caught ? saved_rcx : result.rcx, caught ? saved_flags : result.flags,
           result.rsp_before, result.rsp_after);
    if (positive) {
        require(caught == 0, "legal timestamp instruction completed without signal");
        require(result.rax <= UINT32_MAX && result.rdx <= UINT32_MAX,
                "EAX and EDX writes zero-extend");
        require(rdtscp ? result.rcx <= UINT32_MAX : result.rcx == 0xfedcba9876543210,
                "RDTSCP ECX zero-extension or RDTSC RCX preservation");
        require((result.flags & ordinary_mask) == ordinary_mask,
                "ordinary condition flags and DF preserved");
        require(result.rsp_before == result.rsp_after, "stack pointer preserved");
    } else if (locked) {
        require(caught == SIGILL && trap_number == 6, "LOCK gives #UD / SIGILL");
        require(saved_rip == instruction_start, "LOCK fault names instruction start");
    } else if (overlength) {
        require(caught == SIGSEGV && trap_number == 13, "sixteenth byte gives #GP / SIGSEGV");
        require(saved_rip == instruction_start, "length fault names instruction start");
    } else if (edge && rdtscp) {
        require(caught == SIGSEGV && trap_number == 14, "NX third byte gives instruction #PF");
        require(saved_rip == instruction_start, "fetch fault names instruction start");
        require(fault_address == (uintptr_t)(mapping + page_size * 2),
                "fetch fault names NX page");
    } else {
        require(caught == SIGTRAP && trap_number == 1, "TF gives #DB after timestamp");
        require(saved_rip == after_instruction, "single-step names next instruction");
        require(saved_rax <= UINT32_MAX && saved_rdx <= UINT32_MAX,
                "timestamp retired before the debug trap");
        require(rdtscp ? saved_rcx <= UINT32_MAX : saved_rcx == 0xfedcba9876543210,
                "debug trap sees architectural ECX");
        require((saved_flags & ordinary_mask) == ordinary_mask, "debug trap preserves ordinary flags");
        require((saved_flags & (1UL << 16)) == 0, "retirement clears RF");
        if (rf_mode) require(rf_stage == 1, "RF was explicitly armed via sigreturn");
    }
    require(munmap(mapping, length) == 0, "munmap");
    puts("PASS: native architectural properties");
    return 0;
}
