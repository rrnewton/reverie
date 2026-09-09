#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

static long raw_preadv(int fd, const struct iovec *iov, unsigned long n,
                       int64_t off) {
  return syscall(SYS_preadv, fd, iov, n, (unsigned long)off,
                 0UL);
}

static long raw_pwritev(int fd, const struct iovec *iov, unsigned long n,
                        int64_t off) {
  return syscall(SYS_pwritev, fd, iov, n, (unsigned long)off,
                 0UL);
}

static long raw_preadv2(int fd, const struct iovec *iov, unsigned long n,
                        int64_t off, unsigned long flags) {
  return syscall(SYS_preadv2, fd, iov, n, (unsigned long)off,
                 0UL, flags);
}

static long raw_pwritev2(int fd, const struct iovec *iov, unsigned long n,
                         int64_t off, unsigned long flags) {
  return syscall(SYS_pwritev2, fd, iov, n, (unsigned long)off,
                 0UL, flags);
}

static void result(const char *name, long rc) {
  int saved = errno;
  printf("%-36s rc=%ld errno=%d (%s)\n", name, rc,
         rc < 0 ? saved : 0, rc < 0 ? strerror(saved) : "ok");
}

int main(void) {
  char path[] = "/tmp/vectored-host-XXXXXX";
  int fd = mkstemp(path);
  if (fd < 0) return 2;
  unlink(path);
  if (write(fd, "abcdefgh", 8) != 8) return 3;
  char buf[16] = {0};
  struct iovec one = {.iov_base = buf, .iov_len = 1};
  struct iovec bad = {.iov_base = (void *)1, .iov_len = 1};

#define RUN(name, expr) do { errno = 0; long rc_ = (expr); result((name), rc_); } while (0)
  RUN("preadv badfd badptr", raw_preadv(-1, (void *)1, 1, 0));
  RUN("preadv goodfd badptr", raw_preadv(fd, (void *)1, 1, 0));
  RUN("preadv badfd zero", raw_preadv(-1, NULL, 0, 0));
  RUN("preadv goodfd zero negative", raw_preadv(fd, NULL, 0, -2));
  RUN("preadv goodfd badvec EOF", raw_preadv(fd, &bad, 1, 999));
  RUN("preadv2 badfd invalid-flags", raw_preadv2(-1, &one, 1, 0, 0x80000000UL));
  RUN("preadv2 goodfd badptr invalid", raw_preadv2(fd, (void *)1, 1, 0, 0x80000000UL));
  RUN("preadv2 goodfd badptr", raw_preadv2(fd, (void *)1, 1, 0, 0));
  RUN("preadv2 goodfd neg2", raw_preadv2(fd, &one, 1, -2, 0));
  RUN("preadv2 goodfd neg1", raw_preadv2(fd, &one, 1, -1, 0));
  RUN("preadv2 zero invalid-flags", raw_preadv2(fd, NULL, 0, 0, 0x80000000UL));
  RUN("pwritev2 badfd invalid-flags", raw_pwritev2(-1, &one, 1, 0, 0x80000000UL));
  RUN("pwritev2 goodfd badptr invalid", raw_pwritev2(fd, (void *)1, 1, 0, 0x80000000UL));
  RUN("pwritev2 goodfd badptr", raw_pwritev2(fd, (void *)1, 1, 0, 0));
  RUN("pwritev2 goodfd neg2", raw_pwritev2(fd, &one, 1, -2, 0));
  RUN("pwritev2 zero invalid-flags", raw_pwritev2(fd, NULL, 0, 0, 0x80000000UL));

  long pagesize = sysconf(_SC_PAGESIZE);
  char *pages = mmap(NULL, (size_t)pagesize * 2, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (pages == MAP_FAILED) return 4;
  memset(pages, 'x', (size_t)pagesize);
  if (mprotect(pages + pagesize, (size_t)pagesize, PROT_NONE) != 0) return 5;
  struct iovec partial[2] = {
      {.iov_base = pages, .iov_len = 2},
      {.iov_base = pages + pagesize, .iov_len = 2},
  };
  RUN("preadv valid then fault", raw_preadv(fd, partial, 2, 0));
  RUN("pwritev valid then fault", raw_pwritev(fd, partial, 2, 0));

  int pipefd[2];
  if (pipe(pipefd) != 0) return 6;
  RUN("preadv pipe explicit offset", raw_preadv(pipefd[0], &one, 1, 0));
  RUN("preadv2 pipe offset -1", raw_preadv2(pipefd[0], NULL, 0, -1, 0));
  RUN("pwritev pipe explicit offset", raw_pwritev(pipefd[1], &one, 1, 0));
  RUN("pwritev2 pipe offset -1", raw_pwritev2(pipefd[1], &one, 1, -1, 0));

  if (ftruncate(fd, 0) != 0 || write(fd, "base", 4) != 4) return 7;
  if (lseek(fd, 1, SEEK_SET) != 1) return 8;
  char z = 'Z';
  struct iovec zvec = {.iov_base = &z, .iov_len = 1};
  RUN("pwritev2 append off=0", raw_pwritev2(fd, &zvec, 1, 0, RWF_APPEND));
  printf("offset after append off=0: %ld\n", (long)lseek(fd, 0, SEEK_CUR));
  RUN("pwritev2 append off=-1", raw_pwritev2(fd, &zvec, 1, -1, RWF_APPEND));
  printf("offset after append off=-1: %ld\n", (long)lseek(fd, 0, SEEK_CUR));

  close(pipefd[0]);
  close(pipefd[1]);
  close(fd);
  munmap(pages, (size_t)pagesize * 2);
  return 0;
}
