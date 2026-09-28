#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static void probe_file(const char *path) {
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) {
    printf("open %s errno=%d\n", path, errno);
    return;
  }
  char byte = 'x';
  for (size_t count = 0; count <= 1; ++count) {
    errno = 0;
    ssize_t result = pwrite(fd, &byte, count, 0);
    printf("pwrite %s count=%zu result=%zd errno=%d\n", path, count,
           result, errno);
  }
  close(fd);
}

static void probe_uuid(void) {
  int fd = open("/proc/sys/kernel/random/uuid", O_RDONLY | O_CLOEXEC);
  char first[64] = {0};
  char rest[64] = {0};
  char rewound[64] = {0};
  ssize_t a = read(fd, first, 5);
  ssize_t b = read(fd, rest, sizeof(rest));
  off_t seek = lseek(fd, 0, SEEK_SET);
  ssize_t c = read(fd, rewound, sizeof(rewound));
  printf("uuid partial a=%zd b=%zd seek=%lld c=%zd first=%.*s rest=%.*s rewound=%.*s",
         a, b, (long long)seek, c, (int)a, first, (int)b, rest, (int)c,
         rewound);
  close(fd);
}

static int pty_count(void) {
  FILE *file = fopen("/proc/sys/kernel/pty/nr", "r");
  int count = -1;
  if (file != NULL) {
    (void)fscanf(file, "%d", &count);
    fclose(file);
  }
  return count;
}

int main(void) {
  const char *paths[] = {
      "/proc/sys/kernel/overflowuid",
      "/proc/sys/kernel/overflowgid",
      "/proc/sys/kernel/random/uuid",
      "/proc/sys/kernel/pty/nr",
  };
  for (size_t i = 0; i < sizeof(paths) / sizeof(paths[0]); ++i) {
    probe_file(paths[i]);
  }
  probe_uuid();
  int before = pty_count();
  int master = open("/dev/ptmx", O_RDWR | O_NOCTTY | O_CLOEXEC);
  int after_open = pty_count();
  int alias = dup(master);
  int after_dup = pty_count();
  int second = open("/dev/ptmx", O_RDWR | O_NOCTTY | O_CLOEXEC);
  int after_second = pty_count();
  close(master);
  int after_close_one_alias = pty_count();
  close(alias);
  int after_close_description = pty_count();
  close(second);
  int after_close_all = pty_count();
  printf("pty native=%d,%d,%d,%d,%d,%d,%d\n", before, after_open,
         after_dup, after_second, after_close_one_alias,
         after_close_description, after_close_all);
  return 0;
}
