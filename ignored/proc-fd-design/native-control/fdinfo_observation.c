#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* One native observation control; no exec, thread or recursive process creation. */
static pid_t child_pid = -1;
static const char *active_case = "setup";
static unsigned passed;

static void fail(const char *expression, int line) {
    int saved = errno;
    dprintf(STDERR_FILENO, "fdinfo-control: case=%s line=%d check=%s errno=%d\n",
            active_case, line, expression, saved);
    if (child_pid > 0) {
        kill(child_pid, SIGKILL);
        for (unsigned i = 0; i < 100; ++i) {
            pid_t result = waitpid(child_pid, NULL, WNOHANG);
            if (result == child_pid || (result < 0 && errno == ECHILD)) break;
            struct timespec delay = {0, 1000000};
            nanosleep(&delay, NULL);
        }
    }
    _exit(1);
}
#define CHECK(x) do { if (!(x)) fail(#x, __LINE__); } while (0)

struct snapshot {
    char bytes[4096];
    size_t length;
    long long position;
    unsigned long long flags, mount_id, inode;
};

static void emit_raw(const char *kind, const struct snapshot *s) {
    printf("{\"case\":\"%s\",\"kind\":\"%s\",\"raw_hex\":\"", active_case, kind);
    for (size_t i = 0; i < s->length; ++i) printf("%02x", (unsigned char)s->bytes[i]);
    printf("\"}\n");
}

static void parse(struct snapshot *s) {
    CHECK(s->length < sizeof(s->bytes));
    s->bytes[s->length] = 0;
    char extra;
    CHECK(sscanf(s->bytes, "pos:\t%lld\nflags:\t%llo\nmnt_id:\t%llu\nino:\t%llu\n%c",
                 &s->position, &s->flags, &s->mount_id, &s->inode, &extra) == 4);
}

static void finish_read(int fd, struct snapshot *s) {
    for (;;) {
        CHECK(s->length < sizeof(s->bytes) - 1);
        ssize_t got = read(fd, s->bytes + s->length, sizeof(s->bytes) - 1 - s->length);
        CHECK(got >= 0);
        if (got == 0) break;
        s->length += (size_t)got;
    }
    emit_raw("read", s);
    parse(s);
}

static struct snapshot read_all(int fd) {
    struct snapshot result = {0};
    finish_read(fd, &result);
    return result;
}

static int open_info(int target) {
    char path[64];
    CHECK(snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", target) > 0);
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    CHECK(fd >= 0);
    return fd;
}

static void matches_target(const struct snapshot *s, int target) {
    struct stat st;
    struct statx sx;
    CHECK(fstat(target, &st) == 0);
    CHECK(statx(target, "", AT_EMPTY_PATH, STATX_MNT_ID, &sx) == 0);
    CHECK((sx.stx_mask & STATX_MNT_ID) != 0);
    int flags = fcntl(target, F_GETFL);
    int descriptor_flags = fcntl(target, F_GETFD);
    CHECK(flags >= 0 && descriptor_flags >= 0);
    if (descriptor_flags & FD_CLOEXEC) flags |= O_CLOEXEC;
    CHECK(s->position == lseek(target, 0, SEEK_CUR));
    CHECK(s->flags == (unsigned)flags);
    CHECK(s->mount_id == sx.stx_mnt_id);
    CHECK(s->inode == (unsigned long long)st.st_ino);
}

static void pass(void) {
    printf("{\"case\":\"%s\",\"status\":\"pass\"}\n", active_case);
    ++passed;
}

static void handshake(int fd, bool sending, char token) {
    struct pollfd p = {.fd = fd, .events = sending ? POLLOUT : POLLIN};
    CHECK(poll(&p, 1, 2000) == 1);
    CHECK(p.revents & p.events);
    char observed = 0;
    CHECK((sending ? write(fd, &token, 1) : read(fd, &observed, 1)) == 1);
    if (!sending) CHECK(observed == token);
}

int main(void) {
    CHECK(setvbuf(stdout, NULL, _IONBF, 0) == 0);
    alarm(10); /* Default termination is not swallowed; the service owns cleanup. */
    int a = open("file-a", O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC, 0600);
    int b = open("file-b", O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC, 0600);
    int c = open("file-c", O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC, 0600);
    CHECK(a >= 0 && b >= 0 && c >= 0);
    int target = fcntl(a, F_DUPFD, 20);
    CHECK(target >= 20);

    active_case = "change-after-open";
    int info = open_info(target);
    CHECK(lseek(target, 17, SEEK_SET) == 17);
    CHECK(fcntl(target, F_SETFL, O_APPEND | O_NONBLOCK) == 0);
    CHECK(fcntl(target, F_SETFD, FD_CLOEXEC) == 0);
    struct snapshot s = read_all(info);
    matches_target(&s, target);
    CHECK(close(info) == 0);
    pass();

    active_case = "close-before-first-read";
    info = open_info(target);
    CHECK(close(target) == 0);
    char byte;
    errno = 0;
    ssize_t result = read(info, &byte, 1);
    int saved = errno;
    printf("{\"case\":\"%s\",\"result\":%zd,\"errno\":%d}\n", active_case, result, saved);
    CHECK(result == -1 && saved == ENOENT);
    CHECK(close(info) == 0);
    CHECK(dup2(a, target) == target);
    pass();

    active_case = "replace-before-first-read";
    info = open_info(target);
    CHECK(dup2(b, target) == target);
    CHECK(lseek(target, 23, SEEK_SET) == 23);
    s = read_all(info);
    matches_target(&s, target);
    CHECK(close(info) == 0);
    pass();

    active_case = "partial-read-and-rewind";
    info = open_info(target);
    int separate = open_info(target);
    struct snapshot expected = read_all(separate);
    CHECK(close(separate) == 0);
    struct snapshot partial = {0};
    CHECK(read(info, partial.bytes, 7) == 7);
    partial.length = 7;
    CHECK(lseek(target, 29, SEEK_SET) == 29);
    CHECK(fcntl(target, F_SETFL, O_NONBLOCK) == 0);
    CHECK(fcntl(target, F_SETFD, FD_CLOEXEC) == 0);
    finish_read(info, &partial);
    CHECK(partial.length == expected.length);
    CHECK(memcmp(partial.bytes, expected.bytes, expected.length) == 0);
    CHECK(lseek(info, 0, SEEK_SET) == 0);
    s = read_all(info);
    matches_target(&s, target);
    CHECK(close(info) == 0);
    pass();

    active_case = "nonzero-seek-observation";
    info = open_info(target);
    CHECK(lseek(target, 31, SEEK_SET) == 31);
    separate = open_info(target);
    expected = read_all(separate);
    CHECK(close(separate) == 0);
    CHECK(lseek(info, 3, SEEK_SET) == 3);
    CHECK(lseek(target, 37, SEEK_SET) == 37);
    struct snapshot suffix = {0};
    for (;;) {
        CHECK(suffix.length < sizeof(suffix.bytes) - 1);
        result = read(info, suffix.bytes + suffix.length, sizeof(suffix.bytes) - 1 - suffix.length);
        CHECK(result >= 0);
        if (result == 0) break;
        suffix.length += (size_t)result;
    }
    emit_raw("suffix", &suffix);
    CHECK(suffix.length == expected.length - 3);
    CHECK(memcmp(suffix.bytes, expected.bytes + 3, suffix.length) == 0);
    errno = 0;
    off_t seek_result = lseek(info, 0, SEEK_END);
    saved = errno;
    printf("{\"case\":\"%s\",\"seek_end_result\":%lld,\"errno\":%d}\n", active_case, (long long)seek_result, saved);
    CHECK(seek_result == -1 && saved == EINVAL);
    CHECK(close(info) == 0);
    pass();

    active_case = "positioned-read-regeneration";
    info = open_info(target);
    for (off_t position = 41; position <= 43; position += 2) {
        CHECK(lseek(target, position, SEEK_SET) == position);
        s = (struct snapshot){0};
        result = pread(info, s.bytes, sizeof(s.bytes) - 1, 0);
        CHECK(result > 0);
        s.length = (size_t)result;
        emit_raw("pread", &s);
        parse(&s);
        matches_target(&s, target);
    }
    CHECK(lseek(info, 0, SEEK_CUR) == 0);
    CHECK(close(info) == 0);
    pass();

    active_case = "duplicate-description-buffer";
    info = open_info(target);
    int alias = dup(info);
    CHECK(alias >= 0);
    separate = open_info(target);
    expected = read_all(separate);
    CHECK(close(separate) == 0);
    partial = (struct snapshot){0};
    CHECK(read(info, partial.bytes, 7) == 7);
    partial.length = 7;
    CHECK(lseek(target, 47, SEEK_SET) == 47);
    finish_read(alias, &partial);
    CHECK(partial.length == expected.length);
    CHECK(memcmp(partial.bytes, expected.bytes, expected.length) == 0);
    CHECK(lseek(alias, 0, SEEK_SET) == 0);
    s = read_all(info);
    matches_target(&s, target);
    CHECK(close(alias) == 0 && close(info) == 0);
    pass();

    active_case = "vectored-first-read";
    info = open_info(target);
    s = (struct snapshot){0};
    struct iovec vectors[2] = {{s.bytes, 7}, {s.bytes + 7, sizeof(s.bytes) - 8}};
    result = readv(info, vectors, 2);
    CHECK(result > 7);
    s.length = (size_t)result;
    emit_raw("readv", &s);
    parse(&s);
    matches_target(&s, target);
    CHECK(close(info) == 0);
    pass();

    active_case = "fork-retains-original-task";
    info = open_info(target);
    int to_child[2], to_parent[2];
    CHECK(pipe2(to_child, O_CLOEXEC) == 0 && pipe2(to_parent, O_CLOEXEC) == 0);
    child_pid = fork();
    CHECK(child_pid >= 0);
    if (child_pid == 0) {
        struct rlimit cpu = {1, 1}, address_space = {128 * 1024 * 1024, 128 * 1024 * 1024};
        CHECK(setrlimit(RLIMIT_CPU, &cpu) == 0);
        CHECK(setrlimit(RLIMIT_AS, &address_space) == 0);
        alarm(3);
        CHECK(close(to_child[1]) == 0 && close(to_parent[0]) == 0);
        CHECK(dup2(c, target) == target); /* The child's fd differs deliberately. */
        handshake(to_child[0], false, '1');
        s = read_all(info);
        matches_target(&s, a); /* Parent replaced its target with a, not c. */
        handshake(to_parent[1], true, '1');
        handshake(to_child[0], false, '2');
        CHECK(lseek(info, 0, SEEK_SET) == 0);
        errno = 0;
        result = read(info, &byte, 1);
        saved = errno;
        printf("{\"case\":\"%s\",\"closed_parent_result\":%zd,\"errno\":%d}\n", active_case, result, saved);
        CHECK(result == -1 && saved == ENOENT);
        handshake(to_parent[1], true, '2');
        _exit(0);
    }
    CHECK(close(to_child[0]) == 0 && close(to_parent[1]) == 0);
    CHECK(dup2(a, target) == target);
    CHECK(fcntl(target, F_SETFD, FD_CLOEXEC) == 0); /* a was opened CLOEXEC. */
    handshake(to_child[1], true, '1');
    handshake(to_parent[0], false, '1');
    CHECK(close(target) == 0);
    handshake(to_child[1], true, '2');
    handshake(to_parent[0], false, '2');
    int status;
    CHECK(waitpid(child_pid, &status, 0) == child_pid);
    child_pid = -1;
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    CHECK(close(info) == 0 && close(to_child[1]) == 0 && close(to_parent[0]) == 0);
    pass();

    active_case = "cleanup";
    CHECK(close(a) == 0 && close(b) == 0 && close(c) == 0);
    CHECK(unlink("file-a") == 0 && unlink("file-b") == 0 && unlink("file-c") == 0);
    CHECK(passed == 9);
    printf("{\"summary\":\"fdinfo-observation-ok\",\"passed\":%u,\"fork_children\":1}\n", passed);
    return 0;
}
