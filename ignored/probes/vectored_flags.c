#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

static long callv(long nr, int fd, const struct iovec *iov, int count,
                  int64_t off, unsigned long flags) {
  return syscall(nr, fd, iov, count, (unsigned long)off, 0UL, flags);
}

int main(void) {
  char path[] = "/tmp/vectored-flags-XXXXXX";
  int fd = mkstemp(path);
  unlink(path);
  write(fd, "abcdef", 6);
  char byte = 'z';
  struct iovec good = {.iov_base = &byte, .iov_len = 1};
  struct iovec bad = {.iov_base = (void *)1, .iov_len = 1};
  const unsigned long flags[] = {0, 1, 2, 4, 8, 16, 32, 64, 128, 255, 256};
  for (size_t i = 0; i < sizeof(flags) / sizeof(flags[0]); ++i) {
    errno = 0;
    long rc = callv(SYS_preadv2, fd, &good, 1, 0, flags[i]);
    printf("pread good flag=%3lu rc=%2ld errno=%d\n", flags[i], rc,
           rc < 0 ? errno : 0);
    errno = 0;
    rc = callv(SYS_preadv2, fd, &bad, 1, 0, flags[i]);
    printf("pread bad  flag=%3lu rc=%2ld errno=%d\n", flags[i], rc,
           rc < 0 ? errno : 0);
    errno = 0;
    rc = callv(SYS_pwritev2, fd, &good, 1, 0, flags[i]);
    printf("pwrite good flag=%3lu rc=%2ld errno=%d\n", flags[i], rc,
           rc < 0 ? errno : 0);
    errno = 0;
    rc = callv(SYS_pwritev2, fd, &bad, 1, 0, flags[i]);
    printf("pwrite bad  flag=%3lu rc=%2ld errno=%d\n", flags[i], rc,
           rc < 0 ? errno : 0);
  }
  close(fd);
  return 0;
}
