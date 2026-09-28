#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

static void show(const char *name, long rc) {
  printf("%-40s rc=%ld errno=%d (%s)\n", name, rc, rc < 0 ? errno : 0,
         rc < 0 ? strerror(errno) : "ok");
}
#define RUN(name, expression) do { errno = 0; show((name), (expression)); } while (0)

static long pr2(int fd, const struct iovec *iov, unsigned long count,
                int64_t off, unsigned long flags) {
  return syscall(SYS_preadv2, fd, iov, count, (unsigned long)off, 0UL, flags);
}
static long pw2(int fd, const struct iovec *iov, unsigned long count,
                int64_t off, unsigned long flags) {
  return syscall(SYS_pwritev2, fd, iov, count, (unsigned long)off, 0UL, flags);
}

int main(void) {
  char path[] = "/tmp/vectored-order-XXXXXX";
  int fd = mkstemp(path);
  unlink(path);
  write(fd, "abc", 3);
  char good_byte = 'z';
  struct iovec good = {.iov_base = &good_byte, .iov_len = 1};
  struct iovec bad_data = {.iov_base = (void *)1, .iov_len = 1};
  struct iovec zero_bad = {.iov_base = (void *)1, .iov_len = 0};
  const unsigned long bad_flags = 0x80000000UL;

  RUN("preadv2 bad-data bad-flags", pr2(fd, &bad_data, 1, 0, bad_flags));
  RUN("pwritev2 bad-data bad-flags", pw2(fd, &bad_data, 1, 0, bad_flags));
  RUN("preadv2 good-data bad-flags", pr2(fd, &good, 1, 0, bad_flags));
  RUN("pwritev2 good-data bad-flags", pw2(fd, &good, 1, 0, bad_flags));
  RUN("preadv2 zero-bad bad-flags", pr2(fd, &zero_bad, 1, 0, bad_flags));
  RUN("pwritev2 zero-bad bad-flags", pw2(fd, &zero_bad, 1, 0, bad_flags));
  RUN("preadv2 bad-data neg2", pr2(fd, &bad_data, 1, -2, 0));
  RUN("pwritev2 bad-data neg2", pw2(fd, &bad_data, 1, -2, 0));
  RUN("preadv2 count-too-large badfd", pr2(-1, &good, 1025, 0, 0));
  RUN("preadv2 count-too-large goodfd", pr2(fd, &good, 1025, 0, 0));
  close(fd);
  return 0;
}
