#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static int expect_lseek(int fd, off_t offset, int whence, off_t expected,
                        int expected_errno, const char *label) {
    errno = 0;
    off_t actual = syscall(SYS_lseek, fd, offset, whence);
    int actual_errno = errno;
    if (actual == expected && actual_errno == expected_errno) return 0;
    fprintf(stderr,
            "%s: expected %lld errno %d, got %lld errno %d\n",
            label, (long long)expected, expected_errno,
            (long long)actual, actual_errno);
    return 1;
}

int main(int argc, char **argv) {
    int opath = argc == 2 && strcmp(argv[1], "--opath") == 0;
    int fd = open("/dev/urandom", opath ? O_PATH : O_RDONLY);
    if (fd < 0) {
        perror("open /dev/urandom");
        return 1;
    }

    int failed = 0;
    if (opath) {
        failed |= expect_lseek(fd, 0, SEEK_SET, -1, EBADF, "O_PATH SEEK_SET");
        failed |= expect_lseek(fd, -4, SEEK_CUR, -1, EBADF, "O_PATH SEEK_CUR");
        failed |= expect_lseek(fd, 0, SEEK_END, -1, EBADF, "O_PATH SEEK_END");
        failed |= expect_lseek(fd, 0, SEEK_DATA, -1, EBADF, "O_PATH SEEK_DATA");
        failed |= expect_lseek(fd, 0, SEEK_HOLE, -1, EBADF, "O_PATH SEEK_HOLE");
        failed |= expect_lseek(fd, 0, 99, -1, EBADF, "O_PATH invalid whence");
        if (close(fd) != 0) {
            perror("close O_PATH /dev/urandom");
            return 2;
        }
        if (failed) return 3;
        puts("random-device-lseek-opath-ok");
        return 0;
    }

    unsigned char bytes[8];
    if (read(fd, bytes, sizeof(bytes)) != (ssize_t)sizeof(bytes)) {
        perror("read /dev/urandom before lseek");
        return 4;
    }
    failed |= expect_lseek(fd, -4, SEEK_CUR, 0, 0, "SEEK_CUR");
    failed |= expect_lseek(fd, 123, SEEK_SET, 0, 0, "SEEK_SET");
    failed |= expect_lseek(fd, 0, SEEK_END, 0, 0, "SEEK_END");
    failed |= expect_lseek(fd, 0, SEEK_DATA, 0, 0, "SEEK_DATA");
    failed |= expect_lseek(fd, 0, SEEK_HOLE, 0, 0, "SEEK_HOLE");
    failed |= expect_lseek(fd, 0, 99, -1, EINVAL, "invalid whence");
    if (read(fd, bytes, sizeof(bytes)) != (ssize_t)sizeof(bytes)) {
        perror("read /dev/urandom after lseek");
        return 5;
    }
    if (close(fd) != 0) {
        perror("close /dev/urandom");
        return 6;
    }
    if (failed) return 7;
    puts("random-device-lseek-ok");
    return 0;
}
