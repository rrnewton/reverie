#define _GNU_SOURCE
#include <linux/hw_breakpoint.h>
#include <linux/perf_event.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

struct perf_info {
    int signal_number, error, code, padding;
    void *address;
    unsigned long data;
    unsigned int type, flags;
};
_Static_assert(offsetof(siginfo_t, si_addr) == offsetof(struct perf_info, address), "kernel siginfo ABI");

int clock_fixture_open_breakpoint(uint64_t address) {
    struct perf_event_attr attr = {0};
    attr.type = PERF_TYPE_BREAKPOINT;
    attr.size = sizeof(attr);
    attr.sample_period = 1;
    attr.bp_type = HW_BREAKPOINT_X;
    attr.bp_addr = address;
    attr.bp_len = sizeof(long);
    attr.disabled = 1;
    attr.pinned = 1;
    attr.exclude_kernel = 1;
    attr.exclude_hv = 1;
    attr.remove_on_exec = 1;
    attr.sigtrap = 1;
    attr.sig_data = 0x636c6f636b;
    return syscall(SYS_perf_event_open, &attr, 0, -1, -1, PERF_FLAG_FD_CLOEXEC);
}

int clock_fixture_owned_breakpoint(const siginfo_t *info, uint64_t address) {
    struct perf_info event;
    memcpy(&event, info, sizeof(event));
    return event.signal_number == SIGTRAP && event.code == 6 &&
        event.type == PERF_TYPE_BREAKPOINT && event.flags == 0 &&
        event.data == 0x636c6f636b && event.address == (void *)address;
}
