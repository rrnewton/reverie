#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <linux/stat.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/sysmacros.h>
#include <time.h>
#include <unistd.h>

#ifndef AT_EMPTY_PATH
#define AT_EMPTY_PATH 0x1000
#endif

#ifndef STATX_MNT_ID_UNIQUE
#define STATX_MNT_ID_UNIQUE 0x00004000U
#endif

#define LEGACY_REQUEST (STATX_BASIC_STATS | STATX_MNT_ID)
#define UNIQUE_ONLY_REQUEST STATX_MNT_ID_UNIQUE
#define UNIQUE_BASIC_REQUEST (STATX_BASIC_STATS | STATX_MNT_ID_UNIQUE)

struct observation {
    const char *kind;
    const char *end;
    int fd;
    struct stat stat_value;
    uint64_t fdinfo_mnt_id;
    struct statx legacy_empty;
    struct statx legacy_proc;
    struct statx unique_only_empty;
    struct statx unique_only_proc;
    struct statx unique_basic_empty;
    struct statx unique_basic_proc;
};

static void fail(const char *operation)
{
    int saved_errno = errno;
    fprintf(stderr, "FAIL operation=%s errno=%d message=%s\n", operation,
            saved_errno, strerror(saved_errno));
    exit(2);
}

static void fail_check(const char *invariant)
{
    fprintf(stderr, "FAIL invariant=%s\n", invariant);
    exit(3);
}

static uint64_t timespec_ns(const struct timespec *value)
{
    return (uint64_t)value->tv_sec * UINT64_C(1000000000) +
           (uint64_t)value->tv_nsec;
}

static uint64_t read_fdinfo_mnt_id(int fd)
{
    char path[64];
    char line[256];
    FILE *stream;
    unsigned long long value;

    if (snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", fd) >=
        (int)sizeof(path)) {
        fail_check("fdinfo_path_length");
    }
    stream = fopen(path, "re");
    if (stream == NULL) {
        fail("fopen_fdinfo");
    }
    while (fgets(line, sizeof(line), stream) != NULL) {
        if (sscanf(line, "mnt_id:\t%llu", &value) == 1 ||
            sscanf(line, "mnt_id: %llu", &value) == 1) {
            if (fclose(stream) != 0) {
                fail("fclose_fdinfo");
            }
            return (uint64_t)value;
        }
    }
    if (ferror(stream)) {
        fail("fgets_fdinfo");
    }
    if (fclose(stream) != 0) {
        fail("fclose_fdinfo_missing_mnt_id");
    }
    fail_check("fdinfo_mnt_id_missing");
    return 0;
}

static void do_statx(int dirfd, const char *path, int flags, unsigned int mask,
                     struct statx *result, const char *operation)
{
    if (syscall(SYS_statx, dirfd, path, flags | AT_STATX_SYNC_AS_STAT, mask,
                result) != 0) {
        fail(operation);
    }
}

static bool statx_identity_matches(const struct statx *value,
                                   const struct stat *expected)
{
    unsigned int needed = STATX_TYPE | STATX_MODE | STATX_INO;

    return (value->stx_mask & needed) == needed &&
           value->stx_dev_major == major(expected->st_dev) &&
           value->stx_dev_minor == minor(expected->st_dev) &&
           value->stx_ino == (uint64_t)expected->st_ino &&
           value->stx_mode == (uint16_t)expected->st_mode;
}

static void observe(struct observation *value)
{
    char proc_path[64];

    if (snprintf(proc_path, sizeof(proc_path), "/proc/self/fd/%d", value->fd) >=
        (int)sizeof(proc_path)) {
        fail_check("proc_path_length");
    }
    if (fstat(value->fd, &value->stat_value) != 0) {
        fail("fstat");
    }
    value->fdinfo_mnt_id = read_fdinfo_mnt_id(value->fd);

    do_statx(value->fd, "", AT_EMPTY_PATH, LEGACY_REQUEST,
             &value->legacy_empty, "statx_legacy_empty");
    do_statx(AT_FDCWD, proc_path, 0, LEGACY_REQUEST, &value->legacy_proc,
             "statx_legacy_proc");
    do_statx(value->fd, "", AT_EMPTY_PATH, UNIQUE_ONLY_REQUEST,
             &value->unique_only_empty, "statx_unique_only_empty");
    do_statx(AT_FDCWD, proc_path, 0, UNIQUE_ONLY_REQUEST,
             &value->unique_only_proc, "statx_unique_only_proc");
    do_statx(value->fd, "", AT_EMPTY_PATH, UNIQUE_BASIC_REQUEST,
             &value->unique_basic_empty, "statx_unique_basic_empty");
    do_statx(AT_FDCWD, proc_path, 0, UNIQUE_BASIC_REQUEST,
             &value->unique_basic_proc, "statx_unique_basic_proc");

    if ((value->legacy_empty.stx_mask & STATX_MNT_ID) == 0 ||
        (value->legacy_proc.stx_mask & STATX_MNT_ID) == 0 ||
        value->legacy_empty.stx_mnt_id != value->fdinfo_mnt_id ||
        value->legacy_proc.stx_mnt_id != value->fdinfo_mnt_id) {
        fail_check("legacy_statx_fdinfo_mnt_id_agreement");
    }
    if ((value->unique_only_empty.stx_mask & STATX_MNT_ID_UNIQUE) == 0 ||
        (value->unique_only_proc.stx_mask & STATX_MNT_ID_UNIQUE) == 0 ||
        (value->unique_basic_empty.stx_mask & STATX_MNT_ID_UNIQUE) == 0 ||
        (value->unique_basic_proc.stx_mask & STATX_MNT_ID_UNIQUE) == 0 ||
        value->unique_only_empty.stx_mnt_id !=
            value->unique_only_proc.stx_mnt_id ||
        value->unique_only_empty.stx_mnt_id !=
            value->unique_basic_empty.stx_mnt_id ||
        value->unique_only_empty.stx_mnt_id !=
            value->unique_basic_proc.stx_mnt_id) {
        fail_check("unique_statx_route_agreement");
    }
    if (!statx_identity_matches(&value->legacy_empty, &value->stat_value) ||
        !statx_identity_matches(&value->legacy_proc, &value->stat_value) ||
        !statx_identity_matches(&value->unique_basic_empty,
                                &value->stat_value) ||
        !statx_identity_matches(&value->unique_basic_proc,
                                &value->stat_value)) {
        fail_check("statx_fstat_identity_agreement");
    }
}

static void print_statx(const char *label, const struct statx *value)
{
    printf(" %s={mask=%#x,mnt_id=%" PRIu64 ",dev=%u:%u,ino=%" PRIu64
           ",mode=%#o}",
           label, value->stx_mask, (uint64_t)value->stx_mnt_id,
           value->stx_dev_major, value->stx_dev_minor,
           (uint64_t)value->stx_ino, (unsigned int)value->stx_mode);
}

static void print_observation(const struct observation *value)
{
    uint64_t unique_id = (uint64_t)value->unique_only_empty.stx_mnt_id;

    printf("OBJECT kind=%s end=%s fd=%d fstat_dev=%u:%u fstat_ino=%ju "
           "fdinfo_mnt_id=%" PRIu64 " unique_differs_from_fdinfo=%d",
           value->kind, value->end, value->fd,
           major(value->stat_value.st_dev), minor(value->stat_value.st_dev),
           (uintmax_t)value->stat_value.st_ino, value->fdinfo_mnt_id,
           unique_id != value->fdinfo_mnt_id);
    print_statx("legacy_empty", &value->legacy_empty);
    print_statx("legacy_proc", &value->legacy_proc);
    print_statx("unique_only_empty", &value->unique_only_empty);
    print_statx("unique_only_proc", &value->unique_only_proc);
    print_statx("unique_basic_empty", &value->unique_basic_empty);
    print_statx("unique_basic_proc", &value->unique_basic_proc);
    printf(" agreement=PASS\n");
}

int main(void)
{
    struct timespec started;
    struct timespec finished;
    int pipe_fds[2];
    int socket_fds[2];
    struct observation values[4];
    uint64_t pipe_unique;
    uint64_t socket_unique;

    if (clock_gettime(CLOCK_MONOTONIC_RAW, &started) != 0) {
        fail("clock_gettime_start");
    }
    if (pipe2(pipe_fds, O_CLOEXEC) != 0) {
        fail("pipe2");
    }
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, socket_fds) != 0) {
        fail("socketpair");
    }

    values[0] = (struct observation){
        .kind = "pipe", .end = "read", .fd = pipe_fds[0],
    };
    values[1] = (struct observation){
        .kind = "pipe", .end = "write", .fd = pipe_fds[1],
    };
    values[2] = (struct observation){
        .kind = "socket", .end = "zero", .fd = socket_fds[0],
    };
    values[3] = (struct observation){
        .kind = "socket", .end = "one", .fd = socket_fds[1],
    };

    for (size_t index = 0; index < sizeof(values) / sizeof(values[0]); index++) {
        observe(&values[index]);
    }

    pipe_unique = (uint64_t)values[0].unique_only_empty.stx_mnt_id;
    socket_unique = (uint64_t)values[2].unique_only_empty.stx_mnt_id;
    if (pipe_unique != (uint64_t)values[1].unique_only_empty.stx_mnt_id ||
        socket_unique != (uint64_t)values[3].unique_only_empty.stx_mnt_id) {
        fail_check("same_filesystem_unique_mount_id_agreement");
    }

    printf("RUN pid=%jd legacy_request=%#x unique_only_request=%#x "
           "unique_basic_request=%#x\n",
           (intmax_t)getpid(), LEGACY_REQUEST, UNIQUE_ONLY_REQUEST,
           UNIQUE_BASIC_REQUEST);
    for (size_t index = 0; index < sizeof(values) / sizeof(values[0]); index++) {
        print_observation(&values[index]);
    }
    printf("RELATION pipe_legacy_mnt_id=%" PRIu64
           " pipe_unique_mnt_id=%" PRIu64
           " socket_legacy_mnt_id=%" PRIu64
           " socket_unique_mnt_id=%" PRIu64
           " pipe_socket_unique_same=%d\n",
           values[0].fdinfo_mnt_id, pipe_unique, values[2].fdinfo_mnt_id,
           socket_unique, pipe_unique == socket_unique);

    if (clock_gettime(CLOCK_MONOTONIC_RAW, &finished) != 0) {
        fail("clock_gettime_finish");
    }
    printf("RESULT PASS elapsed_ns=%" PRIu64 "\n",
           timespec_ns(&finished) - timespec_ns(&started));

    for (size_t index = 0; index < sizeof(values) / sizeof(values[0]); index++) {
        if (close(values[index].fd) != 0) {
            fail("close");
        }
    }
    return 0;
}
