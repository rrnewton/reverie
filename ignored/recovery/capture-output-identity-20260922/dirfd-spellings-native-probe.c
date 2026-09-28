#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static void result(const char *name, long value) {
  printf("%s ret=%ld errno=%d(%s)\n", name, value,
         value == -1 ? errno : 0, value == -1 ? strerror(errno) : "Success");
}

int main(void) {
  int pair[2];
  if (pipe2(pair, O_CLOEXEC) != 0) return 100;
  int fd = pair[1];
  char proc[64], thread[64], dev[64], numeric[96], target[96];
  snprintf(proc, sizeof(proc), "/proc/self/fd/%d", fd);
  snprintf(thread, sizeof(thread), "/proc/thread-self/fd/%d", fd);
  snprintf(dev, sizeof(dev), "/dev/fd/%d", fd);
  snprintf(numeric, sizeof(numeric), "/proc/%ld/fd/%d", (long)getpid(), fd);
  snprintf(target, sizeof(target), "native-spelling-link-%ld", (long)getpid());
  const char *paths[] = {proc, thread, dev, numeric};
  const char *labels[] = {"proc-self", "proc-thread-self", "dev-fd", "proc-pid"};
  struct stat st;
  struct statx stx;
  struct timespec times[2] = {
      {.tv_sec = 1640995199, .tv_nsec = 0},
      {.tv_sec = 1640995199, .tv_nsec = 0},
  };
  char output[128];
  for (size_t i = 0; i < sizeof(paths) / sizeof(paths[0]); ++i) {
    char name[96];
    errno = 0;
    long rc = syscall(SYS_fchmodat2, AT_FDCWD, paths[i], 0600, 0);
    snprintf(name, sizeof(name), "%s-fchmodat2", labels[i]);
    result(name, rc);
    errno = 0;
    rc = utimensat(AT_FDCWD, paths[i], times, 0);
    snprintf(name, sizeof(name), "%s-utimensat", labels[i]);
    result(name, rc);
    errno = 0;
    rc = fchownat(AT_FDCWD, paths[i], (uid_t)-1, (gid_t)-1, 0);
    snprintf(name, sizeof(name), "%s-fchownat", labels[i]);
    result(name, rc);
    errno = 0;
    rc = fstatat(AT_FDCWD, paths[i], &st, 0);
    snprintf(name, sizeof(name), "%s-newfstatat", labels[i]);
    result(name, rc);
    errno = 0;
    rc = syscall(SYS_statx, AT_FDCWD, paths[i], 0, STATX_BASIC_STATS, &stx);
    snprintf(name, sizeof(name), "%s-statx", labels[i]);
    result(name, rc);
    errno = 0;
    rc = syscall(SYS_faccessat2, AT_FDCWD, paths[i], F_OK, 0);
    snprintf(name, sizeof(name), "%s-faccessat2", labels[i]);
    result(name, rc);
    errno = 0;
    rc = readlink(paths[i], output, sizeof(output));
    snprintf(name, sizeof(name), "%s-readlink", labels[i]);
    result(name, rc);
    errno = 0;
    rc = linkat(AT_FDCWD, paths[i], AT_FDCWD, target, AT_SYMLINK_FOLLOW);
    snprintf(name, sizeof(name), "%s-linkat-follow", labels[i]);
    result(name, rc);
    if (rc == 0) unlink(target);
  }
  close(pair[0]);
  close(pair[1]);
  return 0;
}
