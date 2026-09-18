#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

struct host_config { uint64_t version, straddler_staleness_ticks; };
struct host_frame {
    uint64_t version, begin_rip, ready_rip, install_helper, install_helper_rip;
    uint64_t install_helper_page_start, install_helper_page_len, helper_stack_top;
    uint64_t helper_return, helper_return_rip, syscall_trap_rip;
    uint64_t syscall_trap_return_rip, install_request, install_result;
    uint64_t start_program_break, initial_program_break;
};
struct install_request {
    uint64_t version, site_start, mapping_end, source_len;
    unsigned char source[64];
};
struct program_counter_mapping {
    uint64_t generated_start, generated_end, logical_address;
};
struct install_result {
    uint64_t version, site_start, site_len;
    uint64_t ptrace_entry_stop_rip, ptrace_completion_stop_rip, relocated_tail;
    uint64_t trampoline_start, trampoline_len, trampoline_code_len;
    uint64_t arena_writable_start, arena_writable_len;
    uint64_t arena_executable_start, arena_executable_len;
    uint64_t instruction_len, straddle_prefix, program_counter_count;
    struct program_counter_mapping program_counters[16];
    uint64_t complete;
};
_Static_assert(sizeof(struct program_counter_mapping) == 24,
               "program-counter mapping ABI size");
_Static_assert(offsetof(struct install_result, program_counters) == 128,
               "install-result mapping offset");
_Static_assert(offsetof(struct install_result, complete) == 512,
               "install-result completion offset");
_Static_assert(sizeof(struct install_result) == 520,
               "install-result ABI size");
static int (*initialize)(const struct host_config *);
static struct host_config config = {1, 0};
static struct host_frame saved_frame;
static volatile sig_atomic_t begins, readies, reentry_result, direct_helper_call;
static struct sigaction before_actions[NSIG];
static int action_query_result[NSIG], action_query_errno[NSIG];

/* No injected guest executes this site. A real successful installation proves
 * the initializer prepared both the site table and a usable trampoline arena. */
extern char test_syscall_site[];
__asm__(".text\n.p2align 6\n.global test_syscall_site\n"
        "test_syscall_site:\n syscall\n nop\n nop\n nop\n nop\n nop\n nop\n ret\n");

static void trap(int sig, siginfo_t *info, void *opaque) {
    (void)info;
    ucontext_t *context = opaque;
    uint64_t marker = context->uc_mcontext.gregs[REG_RAX];
    uint64_t rip = context->uc_mcontext.gregs[REG_RIP];
    if (sig != SIGTRAP) _exit(80);
    if (direct_helper_call && rip == saved_frame.install_helper_rip) {
        if (direct_helper_call == 1) _exit(86);
        if (direct_helper_call == 2) return;
        _exit(88);
    }
    const struct host_frame *frame =
        (const void *)(uintptr_t)context->uc_mcontext.gregs[REG_RDI];
    if (!frame || frame->version != 8) _exit(80);
    if (marker == UINT64_C(0x7265766c69000001)) {
        if (begins || readies || rip != frame->begin_rip) _exit(81);
        saved_frame = *frame;
        begins = 1;
        /* The rejected reentry takes only the argument/atomic guard path. */
        reentry_result = initialize(&config);
        if (reentry_result != -EALREADY) _exit(82);
    } else if (marker == UINT64_C(0x7265766c69000002)) {
        if (begins != 1 || readies || rip != frame->ready_rip) _exit(83);
        const uint64_t *saved = (const void *)&saved_frame;
        const uint64_t *current = (const void *)frame;
        for (size_t i = 0; i < sizeof(saved_frame) / sizeof(uint64_t); ++i)
            if (saved[i] != current[i]) _exit(84);
        readies = 1;
    } else {
        _exit(85);
    }
}

static void require(int condition, const char *message) {
    if (!condition) {
        fprintf(stderr, "%s\n", message);
        exit(1);
    }
}

static void snapshot_dispositions(void) {
    for (int sig = 1; sig < NSIG; ++sig) {
        errno = 0;
        action_query_result[sig] = sigaction(sig, NULL, &before_actions[sig]);
        action_query_errno[sig] = errno;
        require(action_query_result[sig] == 0 || action_query_errno[sig] == EINVAL,
                "initial disposition query");
    }
}

static void require_same_dispositions(void) {
    for (int sig = 1; sig < NSIG; ++sig) {
        struct sigaction after = {0};
        errno = 0;
        int result = sigaction(sig, NULL, &after);
        int saved_errno = errno;
        require(result == action_query_result[sig], "disposition query result changed");
        if (result != 0) {
            require(saved_errno == action_query_errno[sig], "disposition query error changed");
            continue;
        }
        const struct sigaction *before = &before_actions[sig];
        require(after.sa_sigaction == before->sa_sigaction &&
                after.sa_flags == before->sa_flags &&
                after.sa_restorer == before->sa_restorer,
                "explicit host changed signal disposition");
        for (int member = 1; member < NSIG; ++member)
            require(sigismember(&after.sa_mask, member) ==
                    sigismember(&before->sa_mask, member),
                    "explicit host changed handler mask");
    }
}

static void refuse_file_opens(void) {
    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_open, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_openat, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_openat2, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {sizeof(filter) / sizeof(filter[0]), filter};
    require(prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0, "no_new_privs");
    require(prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program) == 0, "seccomp");
}

int main(int argc, char **argv) {
    require(argc == 2, "mode");
    initialize = dlsym(RTLD_DEFAULT, "reverie_liteinst_initialize_host");
    require(initialize != NULL, "missing explicit host initializer");
    struct sigaction action = {.sa_sigaction = trap, .sa_flags = SA_SIGINFO};
    require(sigemptyset(&action.sa_mask) == 0, "sigemptyset");
    require(sigaction(SIGTRAP, &action, NULL) == 0, "sigaction");

    if (!strcmp(argv[1], "builtin-active")) {
        void (*legacy_initialize)(void) = dlsym(RTLD_DEFAULT, "reverie_liteinst_initialize");
        require(legacy_initialize != NULL, "missing legacy initializer");
        require(setenv("REVERIE_LITEINST_TOOL", "spoof-getpid", 1) == 0, "select built-in");
        legacy_initialize();
        require(syscall(SYS_getpid) == 424242, "shared built-in was not active");
        require(initialize(&config) == -EALREADY, "host accepted published dispatcher");
        require(begins == 0 && readies == 0, "host emitted a cross-mode handshake");
        puts("published-dispatcher-refused");
        return 0;
    }

    /* The default preload constructor was inert before main. None of these
     * selectors may be read or changed by the explicit host initialization. */
    require(setenv("REVERIE_LITEINST_HOST_RUNTIME", "not-selected", 1) == 0, "set host");
    require(setenv("REVERIE_LITEINST_TOOL", "invalid-tool", 1) == 0, "set tool");
    require(setenv("REVERIE_LITEINST_STRADDLER_STALENESS_TICKS", "invalid-ticks", 1) == 0,
            "set straddler");
    require(initialize(NULL) == -EINVAL, "null configuration accepted");
    struct host_config bad = {2, 0};
    require(initialize(&bad) == -EINVAL, "unknown configuration accepted");
    require(begins == 0 && readies == 0, "invalid configuration consumed initialization");
    config.straddler_staleness_ticks = !strcmp(argv[1], "configured") ? 17000 : 0;
    int failure = !strcmp(argv[1], "preparation-failure");
    snapshot_dispositions();
    if (failure) refuse_file_opens();
    int result = initialize(&config);
    require_same_dispositions();
    if (failure) {
        require(result == -EPERM && begins == 0 && readies == 0 && reentry_result == 0,
                "reversible preflight failure crossed Begin");
        require(initialize(&config) == -EPERM,
                "reversible preflight retry changed its exact failure");
        require(begins == 0 && readies == 0 && reentry_result == 0,
                "preflight retry emitted a handshake");
    } else {
        require(begins == 1 && reentry_result == -EALREADY,
                "missing Begin or reentry guard");
        require(result == 0 && readies == 1, "actual initialization did not finish");
        int64_t (*install)(uint64_t) = (void *)(uintptr_t)saved_frame.install_helper;
        struct install_request *request = (void *)(uintptr_t)saved_frame.install_request;
        memset(request, 0, sizeof(*request));
        request->version = 1;
        request->site_start = (uintptr_t)test_syscall_site;
        request->mapping_end = request->site_start + 8;
        request->source_len = 8;
        memcpy(request->source, test_syscall_site, 8);
        unsigned char original[8];
        memcpy(original, test_syscall_site, sizeof(original));
        pid_t child = fork();
        require(child >= 0, "fork direct helper control");
        if (child == 0) {
            direct_helper_call = 1;
            (void)install((uintptr_t)test_syscall_site);
            _exit(87);
        }
        int status = 0;
        require(waitpid(child, &status, 0) == child, "wait direct helper control");
        require(WIFEXITED(status) && WEXITSTATUS(status) == 86,
                "unguarded helper entry did not stop at INT3");
        const struct install_result *installed = (void *)(uintptr_t)saved_frame.install_result;
        require(installed->complete == 0, "unguarded helper published a result");
        require(!memcmp(original, test_syscall_site, sizeof(original)),
                "unguarded helper changed the site");
        long page_size = sysconf(_SC_PAGESIZE);
        require(page_size > 0, "page size");
        void *site_page = (void *)((uintptr_t)test_syscall_site & ~((uintptr_t)page_size - 1));
        require(mprotect(site_page, (size_t)page_size,
                         PROT_READ | PROT_WRITE | PROT_EXEC) == 0,
                "open positive helper source page");
        direct_helper_call = 2;
        int64_t tail = install((uintptr_t)test_syscall_site);
        direct_helper_call = 0;
        require(mprotect(site_page, (size_t)page_size, PROT_READ | PROT_EXEC) == 0,
                "restore positive helper source page");
        require(tail > 0, "actual patch helper failed");
        require(installed->version == 4 && installed->complete == 1 &&
                installed->site_start == (uintptr_t)test_syscall_site &&
                installed->instruction_len == 2 && installed->site_len == 8 &&
                installed->ptrace_entry_stop_rip == installed->trampoline_start + 1 &&
                installed->ptrace_completion_stop_rip == installed->relocated_tail &&
                installed->relocated_tail == (uint64_t)tail &&
                installed->trampoline_code_len > 0 &&
                installed->arena_writable_start && installed->arena_executable_start &&
                installed->arena_writable_len && installed->arena_executable_len &&
                installed->program_counter_count > 0 &&
                installed->program_counter_count <= 16,
                "missing initialized site/arena/result ABI");
        int tail_mapped = 0;
        for (uint64_t i = 0; i < installed->program_counter_count; ++i) {
            const struct program_counter_mapping *mapping = &installed->program_counters[i];
            require(mapping->generated_start < mapping->generated_end,
                    "empty program-counter mapping");
            if (mapping->generated_start <= installed->relocated_tail &&
                installed->relocated_tail < mapping->generated_end &&
                mapping->logical_address == (uintptr_t)test_syscall_site + 2)
                tail_mapped = 1;
        }
        require(tail_mapped, "relocated tail lacks logical program-counter mapping");
        require((unsigned char)test_syscall_site[0] != 0x0f, "helper did not patch site");
        require_same_dispositions();
        require(initialize(&config) == -EALREADY, "host initialization ran twice");
        require(begins == 1 && readies == 1, "repeat changed handshake counts");
    }
    require_same_dispositions();
    require(!strcmp(getenv("REVERIE_LITEINST_HOST_RUNTIME"), "not-selected"), "host env changed");
    require(!strcmp(getenv("REVERIE_LITEINST_TOOL"), "invalid-tool"), "tool env changed");
    require(!strcmp(getenv("REVERIE_LITEINST_STRADDLER_STALENESS_TICKS"), "invalid-ticks"),
            "straddler env changed");
    puts(failure ? "preparation-failure-retained" : "explicit-host-initialized");
    return 0;
}
