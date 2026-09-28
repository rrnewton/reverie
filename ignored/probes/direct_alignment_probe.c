#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

static void one(const char *name, int fd, struct iovec *iov, int n, int flags) {
  errno = 0;
  long r = syscall(SYS_pwritev2, fd, iov, n, 0, 0, flags);
  printf("%s=%ld errno=%d\n", name, r, errno);
}

int main(void) {
  void *a = NULL, *b = NULL;
  if (posix_memalign(&a, 4096, 8192) || posix_memalign(&b, 4096, 8192)) return 2;
  memset(a, 'a', 8192); memset(b, 'b', 8192);
  int fd = open("/tmp/reverie-direct-probe", O_CREAT|O_TRUNC|O_RDWR|O_DIRECT, 0600);
  if (fd < 0) { perror("open"); return 3; }
  struct iovec aligned[] = {{a,4096},{b,4096}};
  struct iovec base1[] = {{(char*)a+1,4096}};
  struct iovec shape[] = {{a,2048},{(char*)b+1,2048}};
  one("aligned", fd, aligned, 2, 0);
  one("base1", fd, base1, 1, 0);
  one("shape", fd, shape, 2, 0);
  one("base1-hipri", fd, base1, 1, RWF_HIPRI);
  one("shape-hipri", fd, shape, 2, RWF_HIPRI);
  one("aligned-atomic", fd, aligned, 2, RWF_ATOMIC);
  close(fd); unlink("/tmp/reverie-direct-probe"); free(a); free(b);
  return 0;
}
