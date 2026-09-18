#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
  if (argc != 2) {
    return 2;
  }
  for (int i = 1; i <= 5000; ++i) {
    errno = 0;
    int fd = open(argv[1], O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
      printf("open=%d rc=-1 errno=%d (%s)\n", i, errno, strerror(errno));
      return 1;
    }
    if (close(fd) != 0) {
      printf("close=%d rc=-1 errno=%d (%s)\n", i, errno, strerror(errno));
      return 1;
    }
  }
  puts("5000 opens completed");
  return 0;
}
