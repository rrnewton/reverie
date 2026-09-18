#define _GNU_SOURCE

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <unistd.h>

extern long raw_openat_on_stack(void *new_stack, const char *path);

int main(int argc, char **argv) {
  if (argc != 3) {
    return 2;
  }

  long page_size = sysconf(_SC_PAGESIZE);
  char *region = mmap(NULL,
                      (size_t)page_size * 2,
                      PROT_NONE,
                      MAP_PRIVATE | MAP_ANONYMOUS,
                      -1,
                      0);
  if (region == MAP_FAILED ||
      mprotect(region + page_size, (size_t)page_size, PROT_READ | PROT_WRITE) != 0) {
    perror("stack mapping");
    return 2;
  }

  long offset = strtol(argv[1], NULL, 10);
  void *new_stack = region + page_size + offset;
  long raw_rc = raw_openat_on_stack(new_stack, argv[2]);
  if (raw_rc >= 0) {
    close((int)raw_rc);
    printf("openat raw_rc=%ld errno=0\n", raw_rc);
    return 0;
  }

  printf("openat raw_rc=%ld errno=%ld\n", raw_rc, -raw_rc);
  return 1;
}
