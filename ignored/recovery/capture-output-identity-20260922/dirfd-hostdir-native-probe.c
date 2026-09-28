#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static void report(const char *label, long rc) {
  printf("%s ret=%ld errno=%d(%s)\n", label, rc, rc == -1 ? errno : 0,
         rc == -1 ? strerror(errno) : "Success");
}

static void metadata(const char *label, int fd) {
  struct stat st;
  if (fstat(fd, &st) != 0) {
    perror("fstat");
    return;
  }
  printf("%s mode=%#o atime=%ld.%09ld mtime=%ld.%09ld\n", label,
         st.st_mode & 07777, (long)st.st_atim.tv_sec, st.st_atim.tv_nsec,
         (long)st.st_mtim.tv_sec, st.st_mtim.tv_nsec);
}

int main(void) {
  char path[96];
  snprintf(path, sizeof(path), "native-hostdir-victim-%ld", (long)getpid());
  if (mkdir(path, 0750) != 0) return 100;
  int fd = open(path, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
  if (fd < 0) return 101;
  metadata("initial", fd);
  errno = 0;
  long rc = syscall(SYS_fchmodat2, fd, "", 0711, AT_EMPTY_PATH);
  report("fchmodat2-empty", rc);
  metadata("after-fchmodat2", fd);
  struct timespec first[2] = {
      {.tv_sec = 1640995198, .tv_nsec = 123},
      {.tv_sec = 1640995199, .tv_nsec = 456},
  };
  errno = 0;
  rc = utimensat(fd, "", first, AT_EMPTY_PATH);
  report("utimensat-empty", rc);
  metadata("after-utimensat-empty", fd);
  struct timespec second[2] = {
      {.tv_sec = 1640995196, .tv_nsec = 789},
      {.tv_sec = 1640995197, .tv_nsec = 987},
  };
  errno = 0;
  rc = utimensat(fd, NULL, second, 0);
  report("utimensat-null", rc);
  metadata("after-utimensat-null", fd);
  printf("victim=%s\n", path);
  close(fd);
  return 0;
}
