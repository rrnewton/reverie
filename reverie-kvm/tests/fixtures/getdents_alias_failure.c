/* Process-isolated host fault injection; no guest or production fault hooks. */
#define _GNU_SOURCE
#include <errno.h>
#include <stdatomic.h>
#include <stdint.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

static _Atomic int stage;
static _Atomic uintptr_t reservation;
static _Atomic unsigned long counters[5];

void reverie_alias_failure_arm(int value) {
    for (int i = 0; i < 5; i++) atomic_store(&counters[i], 0);
    atomic_store(&reservation, 0);
    atomic_store(&stage, value);
}

unsigned long reverie_alias_failure_count(int index) {
    return atomic_load(&counters[index]);
}

void *mmap(void *address, size_t length, int prot, int flags, int fd, off_t offset) {
    int active = atomic_load(&stage);
    int reserve = active && length == 12288 && prot == PROT_NONE &&
        flags == (MAP_PRIVATE | MAP_ANONYMOUS) && fd == -1;
    if (reserve) {
        atomic_fetch_add(&counters[0], 1);
        if (active == 1) {
            atomic_store(&stage, 0);
            atomic_fetch_add(&counters[2], 1);
            errno = ENOMEM;
            return MAP_FAILED;
        }
    }
    uintptr_t base = atomic_load(&reservation);
    if (active == 2 && base && length == 4096 &&
        ((uintptr_t)address == base || (uintptr_t)address == base + 8192) &&
        flags == (MAP_SHARED | MAP_FIXED) && prot == (PROT_READ | PROT_WRITE)) {
        unsigned long ordinal = atomic_fetch_add(&counters[1], 1) + 1;
        if (ordinal == 2) {
            atomic_store(&stage, 0);
            atomic_fetch_add(&counters[2], 1);
            errno = ENOMEM;
            return MAP_FAILED;
        }
    }
    void *result = (void *)syscall(SYS_mmap, address, length, prot, flags, fd, offset);
    if (reserve && result != MAP_FAILED) atomic_store(&reservation, (uintptr_t)result);
    return result;
}

int munmap(void *address, size_t length) {
    int cleanup = atomic_load(&counters[2]) == 1 &&
        (uintptr_t)address == atomic_load(&reservation) && length == 12288;
    if (cleanup) {
        /* The first shared extent is still live and the guest is stopped. */
        const unsigned char *bytes = address;
        int unchanged = 1;
        for (int i = 0; i < 4096; i++) unchanged &= bytes[i] == 0xa5;
        atomic_store(&counters[4], unchanged);
    }
    int result = syscall(SYS_munmap, address, length);
    if (cleanup && result == 0) {
        atomic_fetch_add(&counters[3], 1);
        atomic_store(&reservation, 0);
    }
    return result;
}
