/* Process-isolated host fault injection; no guest or production fault hooks. */
#define _GNU_SOURCE
#include <errno.h>
#include <linux/futex.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

#define PAGE 4096
#define MARKER 0x6b
#define COUNT 20

enum {
    RESERVE, EXTENT, FIRED, CLEANUP, FIRST_UNCHANGED,
    PREP_UNMAP, PREP_RELEASED, CLEANUP_MASK,
    WORKER_CREATED, WORKER_JOINED, FOREIGN_CREATED,
    FOREIGN_BEFORE, FOREIGN_AFTER, FOREIGN_FINISH, FOREIGN_RELEASED,
    REAL_EEXIST, WRONG_CREATED, WRONG_RELEASED, DIFFERENT_OWNER, BAD_CLEANUP
};

static _Atomic int active;
static _Atomic long owner_tid;
static _Atomic uintptr_t reservation;
static _Atomic unsigned long counters[COUNT];
static int chosen;
static _Atomic int finished;
static size_t span;
static unsigned long queried;
static uintptr_t foreign_address, wrong_address;
static _Atomic int foreign_owned, wrong_owned;
static pthread_t worker;
static _Atomic int request;
static uintptr_t worker_target;
static long worker_tid;
static int worker_errno, worker_mapped;

static _Noreturn void refuse(const char *reason) {
    atomic_store(&active, 0);
    dprintf(2, "ALIAS_FIXTURE_SETUP_FAILURE case=%d reason=%s errno=%d\n",
            chosen, reason, errno);
    syscall(SYS_exit_group, 97);
    __builtin_unreachable();
}

static int has_foreign(void) {
    return chosen == 3 || chosen == 4 || chosen == 6 || chosen == 7;
}

/* Kernel copies avoid dereferencing a mapping that buggy cleanup removed. */
static int all_bytes(uintptr_t address, unsigned char expected) {
    unsigned char bytes[PAGE], vector;
    long rc;
    do { rc = syscall(SYS_mincore, (void *)address, PAGE, &vector); }
    while (rc == -1 && errno == EINTR);
    if (rc != 0) return 0;
    struct iovec local = {.iov_base = bytes, .iov_len = PAGE};
    struct iovec remote = {.iov_base = (void *)address, .iov_len = PAGE};
    do { rc = syscall(SYS_process_vm_readv, getpid(), &local, 1, &remote, 1, 0); }
    while (rc == -1 && errno == EINTR);
    if (rc != PAGE) return 0;
    for (size_t i = 0; i < PAGE; i++) if (bytes[i] != expected) return 0;
    return 1;
}

static int overlaps(uintptr_t address, size_t length, uintptr_t start, size_t n) {
    if (!length || !n) return 0;
    return address <= start ? start - address < length : address - start < n;
}

static void *foreign_allocator(void *unused) {
    (void)unused;
    worker_tid = syscall(SYS_gettid);
    while (atomic_load_explicit(&request, memory_order_acquire) == 0) {
        /* The containing child owns the 30s timeout and 2s kill grace. */
        long rc = syscall(SYS_futex, &request, FUTEX_WAIT_PRIVATE, 0, 0, 0, 0);
        if (rc == -1 && errno != EAGAIN && errno != EINTR) {
            worker_errno = errno;
            return 0;
        }
    }
    void *p = (void *)syscall(SYS_mmap, (void *)worker_target, PAGE,
                             PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE,
                             -1, 0);
    if (p == MAP_FAILED) worker_errno = errno;
    else if ((uintptr_t)p != worker_target) {
        syscall(SYS_munmap, p, PAGE); /* Only the positively returned map is owned. */
        worker_errno = EOPNOTSUPP;
    } else {
        memset(p, MARKER, PAGE);
        worker_mapped = 1;
    }
    return 0;
}

void reverie_alias_failure_arm(int value) {
    if (chosen || value < 1 || value > 7 || sysconf(_SC_PAGESIZE) != PAGE)
        refuse("case-or-page-size");
    chosen = value;
    span = (value == 7 ? 5 : 3) * PAGE;
    if (has_foreign()) {
        /* Start before arming, so pthread's own allocations are never injected. */
        int rc = pthread_create(&worker, 0, foreign_allocator, 0);
        if (rc) { errno = rc; refuse("worker-create"); }
        atomic_store(&counters[WORKER_CREATED], 1);
    }
    atomic_store(&active, value);
}

static void install_foreign(uintptr_t target) {
    worker_target = target;
    atomic_store_explicit(&request, 1, memory_order_release);
    if (syscall(SYS_futex, &request, FUTEX_WAKE_PRIVATE, 1, 0, 0, 0) < 0)
        refuse("worker-wake");
    int rc = pthread_join(worker, 0);
    if (rc) { errno = rc; refuse("worker-join"); }
    atomic_store(&counters[WORKER_JOINED], 1);
    if (worker_errno || !worker_mapped || worker_tid == atomic_load(&owner_tid)) {
        errno = worker_errno;
        refuse("foreign-thread-map-not-owned-exactly");
    }
    foreign_address = target;
    foreign_owned = 1;
    atomic_store(&counters[FOREIGN_CREATED], 1);
    atomic_store(&counters[DIFFERENT_OWNER], 1);
    if (!all_bytes(foreign_address, MARKER)) refuse("foreign-marker-not-readable");
}

void *mmap(void *address, size_t length, int prot, int flags, int fd, off_t offset) {
    int armed = atomic_load(&active);
    long tid = syscall(SYS_gettid);
    long owner = atomic_load(&owner_tid);
    int reserve = armed && !owner && !address && length == span && prot == PROT_NONE &&
                  flags == (MAP_PRIVATE | MAP_ANONYMOUS) && fd == -1 && offset == 0;
    if (reserve) {
        long unclaimed = 0;
        if (!atomic_compare_exchange_strong(&owner_tid, &unclaimed, tid))
            reserve = 0;
        else {
            /* Execution may occur on a backend thread, not the arming thread. */
            owner = tid;
            atomic_fetch_add(&counters[RESERVE], 1);
            if (armed == 1) {
                atomic_store(&active, 0);
                atomic_fetch_add(&counters[FIRED], 1);
                errno = ENOMEM;
                return MAP_FAILED;
            }
        }
    }
    uintptr_t base = atomic_load(&reservation);
    int install = armed && tid == owner && base && length == PAGE &&
                  ((uintptr_t)address == base || (uintptr_t)address == base + 2 * PAGE) &&
                  prot == (PROT_READ | PROT_WRITE) && fd >= 0;
    if (install) {
        if (flags != (MAP_SHARED | MAP_FIXED_NOREPLACE)) refuse("extent-flags");
        unsigned long ordinal = atomic_fetch_add(&counters[EXTENT], 1) + 1;
        if (ordinal == 1) {
            if ((uintptr_t)address != base ||
                atomic_load(&counters[PREP_RELEASED]) != 1)
                refuse("first-extent-shape");
        } else if (ordinal == 2) {
            if ((uintptr_t)address != base + 2 * PAGE || chosen == 5 ||
                atomic_load(&counters[PREP_RELEASED]) != 2)
                refuse("second-extent-shape");
            atomic_store(&active, 0);
            if (has_foreign()) install_foreign((uintptr_t)address);
            if (chosen == 4) {
                /* EEXIST must be produced by the real kernel collision check. */
                void *p = (void *)syscall(SYS_mmap, address, length, prot, flags, fd, offset);
                int error = errno;
                if (p != MAP_FAILED || error != EEXIST) refuse("real-collision-result");
                atomic_store(&counters[REAL_EEXIST], 1);
                atomic_fetch_add(&counters[FIRED], 1);
                errno = error;
                return p;
            }
            if (chosen == 6) {
                /* F occupies the requested address, forcing a distinct result. */
                void *p = (void *)syscall(SYS_mmap, 0, length, prot, MAP_SHARED, fd, offset);
                if (p == MAP_FAILED || overlaps((uintptr_t)p, length, base, span))
                    refuse("wrong-address-setup");
                wrong_address = (uintptr_t)p;
                wrong_owned = 1;
                atomic_store(&counters[WRONG_CREATED], 1);
                atomic_fetch_add(&counters[FIRED], 1);
                errno = EACCES; /* A successful syscall does not supply an errno. */
                return p;
            }
            atomic_fetch_add(&counters[FIRED], 1);
            errno = ENOMEM;
            return MAP_FAILED;
        } else refuse("extra-extent");
    }
    void *result = (void *)syscall(SYS_mmap, address, length, prot, flags, fd, offset);
    int error = errno;
    if (reserve) {
        if (result == MAP_FAILED) refuse("reservation-setup");
        atomic_store(&reservation, (uintptr_t)result);
    }
    if (install && result != address) {
        if (result != MAP_FAILED) syscall(SYS_munmap, result, length);
        errno = error;
        refuse("first-extent-setup");
    }
    errno = error;
    return result;
}

int munmap(void *address, size_t length) {
    uintptr_t base = atomic_load(&reservation);
    long owner = atomic_load(&owner_tid);
    if (!owner || syscall(SYS_gettid) != owner || finished)
        return syscall(SYS_munmap, address, length);
    int in_alias = base && overlaps((uintptr_t)address, length, base, span);
    if (atomic_load(&active) && in_alias) {
        unsigned long ordinal = atomic_fetch_add(&counters[PREP_UNMAP], 1) + 1;
        uintptr_t expected = base + (ordinal == 1 ? 0 : 2 * PAGE);
        if (ordinal > 2 || (uintptr_t)address != expected || length != PAGE)
            refuse("preparation-unmap-shape");
        if (chosen == 5 && ordinal == 2) {
            atomic_store(&active, 0);
            atomic_fetch_add(&counters[FIRED], 1);
            errno = ENOMEM;
            return -1; /* The reservation page remains owned. */
        }
        int result = syscall(SYS_munmap, address, length);
        int error = errno;
        if (result) refuse("preparation-unmap-setup");
        atomic_fetch_add(&counters[PREP_RELEASED], 1);
        errno = error;
        return result;
    }
    int cleanup = atomic_load(&counters[FIRED]) == 1 && in_alias;
    if (cleanup) {
        int shape = chosen == 5
            ? (uintptr_t)address == base && length == 3 * PAGE
            : ((uintptr_t)address == base && length == 2 * PAGE) ||
              (chosen == 7 && (uintptr_t)address == base + 3 * PAGE && length == 2 * PAGE);
        if (!shape) atomic_fetch_add(&counters[BAD_CLEANUP], 1);
        if ((uintptr_t)address == base)
            atomic_store(&counters[FIRST_UNCHANGED], all_bytes(base, 0xa5));
        if (foreign_owned && all_bytes(foreign_address, MARKER))
            atomic_fetch_add(&counters[FOREIGN_BEFORE], 1);
    }
    int touches_foreign = foreign_owned && overlaps((uintptr_t)address, length, foreign_address, PAGE);
    int touches_wrong = wrong_owned && overlaps((uintptr_t)address, length, wrong_address, PAGE);
    int result = syscall(SYS_munmap, address, length);
    int error = errno;
    if (!result && touches_foreign) foreign_owned = 0;
    if (!result && touches_wrong) {
        if ((uintptr_t)address != wrong_address || length != PAGE)
            atomic_fetch_add(&counters[BAD_CLEANUP], 1);
        wrong_owned = 0;
        atomic_fetch_add(&counters[WRONG_RELEASED], 1);
    }
    if (cleanup && !result) {
        atomic_fetch_add(&counters[CLEANUP], 1);
        unsigned long mask = 0;
        for (size_t i = 0; i < span / PAGE; i++)
            if (overlaps((uintptr_t)address, length, base + i * PAGE, PAGE)) mask |= 1UL << i;
        if (atomic_fetch_or(&counters[CLEANUP_MASK], mask) & mask)
            atomic_fetch_add(&counters[BAD_CLEANUP], 1);
        if (foreign_owned && all_bytes(foreign_address, MARKER))
            atomic_fetch_add(&counters[FOREIGN_AFTER], 1);
    }
    errno = error;
    return result;
}

void reverie_alias_failure_finish(void) {
    if (finished || !chosen || atomic_load(&counters[FIRED]) != 1)
        refuse("missing-or-repeated-trigger");
    if (has_foreign()) {
        if (atomic_load(&counters[WORKER_JOINED]) != 1) refuse("worker-not-joined");
        if (foreign_owned && all_bytes(foreign_address, MARKER)) {
            atomic_store(&counters[FOREIGN_FINISH], 1);
            if (syscall(SYS_munmap, (void *)foreign_address, PAGE)) refuse("foreign-owner-release");
            foreign_owned = 0;
            atomic_store(&counters[FOREIGN_RELEASED], 1);
        }
        /* Never unmap by stale address if product cleanup destroyed ownership. */
    }
    finished = 1;
}

unsigned long reverie_alias_failure_count(int index) {
    if (!finished || index < 0 || index >= COUNT || (queried & (1UL << index)))
        refuse("counter-query");
    queried |= 1UL << index;
    return atomic_load(&counters[index]);
}
