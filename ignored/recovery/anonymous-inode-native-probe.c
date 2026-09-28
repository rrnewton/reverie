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

struct observation {
    const char *kind;
    const char *end;
    const char *link_prefix;
    int fd;
    struct stat fstat_value;
    struct stat at_empty_value;
    struct stat at_proc_value;
    struct statx statx_empty_value;
    struct statx statx_proc_value;
    char link_text[128];
    uint64_t link_ino;
    uint64_t fdinfo_ino;
    uint64_t fdinfo_mnt_id;
};

static void fail(const char *what)
{
    int saved_errno = errno;
    fprintf(stderr, "FAIL operation=%s errno=%d message=%s\n", what,
            saved_errno, strerror(saved_errno));
    exit(2);
}

static void fail_check(const char *what)
{
    fprintf(stderr, "FAIL invariant=%s\n", what);
    exit(3);
}

static uint64_t timespec_ns(const struct timespec *value)
{
    return (uint64_t)value->tv_sec * UINT64_C(1000000000) +
           (uint64_t)value->tv_nsec;
}

static uint64_t parse_link_ino(const char *text, const char *prefix)
{
    size_t prefix_len = strlen(prefix);
    size_t text_len = strlen(text);
    char *end = NULL;
    unsigned long long value;

    if (text_len < prefix_len + 3 || strncmp(text, prefix, prefix_len) != 0 ||
        text[prefix_len] != '[' || text[text_len - 1] != ']') {
        fail_check("proc_fd_link_shape");
    }

    errno = 0;
    value = strtoull(text + prefix_len + 1, &end, 10);
    if (errno != 0 || end != text + text_len - 1 || value == 0) {
        fail_check("proc_fd_link_inode_parse");
    }
    return (uint64_t)value;
}

static void read_fdinfo(int fd, uint64_t *ino, uint64_t *mnt_id)
{
    char path[64];
    char line[256];
    FILE *stream;
    unsigned long long value;
    bool saw_ino = false;
    bool saw_mnt_id = false;

    if (snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", fd) >=
        (int)sizeof(path)) {
        fail_check("fdinfo_path_length");
    }
    stream = fopen(path, "re");
    if (stream == NULL) {
        fail("fopen_fdinfo");
    }
    while (fgets(line, sizeof(line), stream) != NULL) {
        if (sscanf(line, "ino:\t%llu", &value) == 1 ||
            sscanf(line, "ino: %llu", &value) == 1) {
            *ino = (uint64_t)value;
            saw_ino = true;
        } else if (sscanf(line, "mnt_id:\t%llu", &value) == 1 ||
                   sscanf(line, "mnt_id: %llu", &value) == 1) {
            *mnt_id = (uint64_t)value;
            saw_mnt_id = true;
        }
    }
    if (ferror(stream)) {
        fail("fgets_fdinfo");
    }
    if (fclose(stream) != 0) {
        fail("fclose_fdinfo");
    }
    if (!saw_ino || !saw_mnt_id) {
        fail_check("fdinfo_ino_or_mnt_id_missing");
    }
}

static bool same_stat_identity(const struct stat *left, const struct stat *right)
{
    return left->st_dev == right->st_dev && left->st_ino == right->st_ino &&
           left->st_mode == right->st_mode;
}

static bool statx_matches_stat(const struct statx *left,
                               const struct stat *right)
{
    uint32_t needed = STATX_TYPE | STATX_MODE | STATX_INO;

    return (left->stx_mask & needed) == needed &&
           left->stx_dev_major == major(right->st_dev) &&
           left->stx_dev_minor == minor(right->st_dev) &&
           left->stx_ino == (uint64_t)right->st_ino &&
           left->stx_mode == (uint16_t)right->st_mode;
}

static void observe(struct observation *result)
{
    char fd_path[64];
    ssize_t link_len;

    if (snprintf(fd_path, sizeof(fd_path), "/proc/self/fd/%d", result->fd) >=
        (int)sizeof(fd_path)) {
        fail_check("proc_fd_path_length");
    }

    if (fstat(result->fd, &result->fstat_value) != 0) {
        fail("fstat");
    }
    if (syscall(SYS_newfstatat, result->fd, "", &result->at_empty_value,
                AT_EMPTY_PATH) != 0) {
        fail("newfstatat_empty_path");
    }
    if (syscall(SYS_newfstatat, AT_FDCWD, fd_path, &result->at_proc_value, 0) !=
        0) {
        fail("newfstatat_proc_path");
    }
    if (syscall(SYS_statx, result->fd, "",
                AT_EMPTY_PATH | AT_STATX_SYNC_AS_STAT, STATX_BASIC_STATS,
                &result->statx_empty_value) != 0) {
        fail("statx_empty_path");
    }
    if (syscall(SYS_statx, AT_FDCWD, fd_path, AT_STATX_SYNC_AS_STAT,
                STATX_BASIC_STATS, &result->statx_proc_value) != 0) {
        fail("statx_proc_path");
    }

    link_len = readlink(fd_path, result->link_text,
                        sizeof(result->link_text) - 1);
    if (link_len < 0) {
        fail("readlink_proc_fd");
    }
    if ((size_t)link_len >= sizeof(result->link_text) - 1) {
        fail_check("proc_fd_link_truncated");
    }
    result->link_text[link_len] = '\0';
    result->link_ino = parse_link_ino(result->link_text, result->link_prefix);
    read_fdinfo(result->fd, &result->fdinfo_ino, &result->fdinfo_mnt_id);

    if (!same_stat_identity(&result->fstat_value,
                            &result->at_empty_value) ||
        !same_stat_identity(&result->fstat_value, &result->at_proc_value)) {
        fail_check("fstat_newfstatat_identity_agreement");
    }
    if (!statx_matches_stat(&result->statx_empty_value,
                            &result->fstat_value) ||
        !statx_matches_stat(&result->statx_proc_value,
                            &result->fstat_value)) {
        fail_check("statx_stat_identity_agreement");
    }
    if (result->link_ino != (uint64_t)result->fstat_value.st_ino ||
        result->fdinfo_ino != (uint64_t)result->fstat_value.st_ino) {
        fail_check("proc_link_fdinfo_stat_inode_agreement");
    }
    if ((result->statx_empty_value.stx_mask & STATX_MNT_ID) == 0 ||
        (result->statx_proc_value.stx_mask & STATX_MNT_ID) == 0 ||
        result->fdinfo_mnt_id != result->statx_empty_value.stx_mnt_id ||
        result->fdinfo_mnt_id != result->statx_proc_value.stx_mnt_id) {
        fail_check("fdinfo_statx_mount_id_agreement");
    }
}

static void print_stat(const char *api, const struct stat *value)
{
    printf(" %s={dev_raw=%ju,dev_major=%u,dev_minor=%u,ino=%ju,mode=%#jo,"
           "nlink=%ju,size=%jd,blksize=%jd,blocks=%jd}",
           api, (uintmax_t)value->st_dev, major(value->st_dev),
           minor(value->st_dev), (uintmax_t)value->st_ino,
           (uintmax_t)value->st_mode, (uintmax_t)value->st_nlink,
           (intmax_t)value->st_size, (intmax_t)value->st_blksize,
           (intmax_t)value->st_blocks);
}

static void print_statx(const char *api, const struct statx *value)
{
    printf(" %s={mask=%#x,dev_major=%u,dev_minor=%u,rdev_major=%u,"
           "rdev_minor=%u,ino=%" PRIu64 ",mode=%#o,nlink=%u,size=%" PRIu64
           ",blksize=%u,blocks=%" PRIu64 ",mnt_id=%" PRIu64 "}",
           api, value->stx_mask, value->stx_dev_major, value->stx_dev_minor,
           value->stx_rdev_major, value->stx_rdev_minor,
           (uint64_t)value->stx_ino, (unsigned int)value->stx_mode,
           value->stx_nlink, (uint64_t)value->stx_size, value->stx_blksize,
           (uint64_t)value->stx_blocks, (uint64_t)value->stx_mnt_id);
}

static void print_observation(const struct observation *value)
{
    printf("OBJECT kind=%s end=%s fd=%d link=%s link_ino=%" PRIu64
           " fdinfo_ino=%" PRIu64 " fdinfo_mnt_id=%" PRIu64,
           value->kind, value->end, value->fd, value->link_text,
           value->link_ino, value->fdinfo_ino, value->fdinfo_mnt_id);
    print_stat("fstat", &value->fstat_value);
    print_stat("newfstatat_empty", &value->at_empty_value);
    print_stat("newfstatat_proc", &value->at_proc_value);
    print_statx("statx_empty", &value->statx_empty_value);
    print_statx("statx_proc", &value->statx_proc_value);
    printf(" agreement=PASS\n");
}

static bool same_object(const struct observation *left,
                        const struct observation *right)
{
    return left->fstat_value.st_dev == right->fstat_value.st_dev &&
           left->fstat_value.st_ino == right->fstat_value.st_ino;
}

int main(void)
{
    struct timespec started;
    struct timespec finished;
    int pipe_fds[2];
    int socket_fds[2];
    struct observation values[4];
    bool pipe_same;
    bool socket_same;
    bool same_device_between_kinds;

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
        .kind = "pipe", .end = "read", .link_prefix = "pipe:",
        .fd = pipe_fds[0],
    };
    values[1] = (struct observation){
        .kind = "pipe", .end = "write", .link_prefix = "pipe:",
        .fd = pipe_fds[1],
    };
    values[2] = (struct observation){
        .kind = "socket", .end = "zero", .link_prefix = "socket:",
        .fd = socket_fds[0],
    };
    values[3] = (struct observation){
        .kind = "socket", .end = "one", .link_prefix = "socket:",
        .fd = socket_fds[1],
    };

    for (size_t index = 0; index < sizeof(values) / sizeof(values[0]); index++) {
        observe(&values[index]);
    }

    pipe_same = same_object(&values[0], &values[1]);
    socket_same = same_object(&values[2], &values[3]);
    same_device_between_kinds =
        values[0].fstat_value.st_dev == values[2].fstat_value.st_dev;
    if (!pipe_same) {
        fail_check("pipe_ends_share_identity");
    }
    if (socket_same) {
        fail_check("socketpair_ends_have_distinct_identity");
    }

    printf("RUN pid=%jd requested_statx_mask=%#x\n", (intmax_t)getpid(),
           STATX_BASIC_STATS);
    for (size_t index = 0; index < sizeof(values) / sizeof(values[0]); index++) {
        print_observation(&values[index]);
    }
    printf("RELATION pipe_ends_same_identity=%d socket_ends_same_identity=%d "
           "pipe_socket_same_device=%d pipe_dev=%u:%u socket_dev=%u:%u\n",
           pipe_same, socket_same, same_device_between_kinds,
           major(values[0].fstat_value.st_dev),
           minor(values[0].fstat_value.st_dev),
           major(values[2].fstat_value.st_dev),
           minor(values[2].fstat_value.st_dev));

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
