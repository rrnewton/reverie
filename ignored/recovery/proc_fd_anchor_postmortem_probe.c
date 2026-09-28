#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

static _Atomic int self_anchor = -1;
static _Atomic int thread_anchor = -1;

static void *worker(void *unused) {
  (void)unused;
  atomic_store(&self_anchor, open("/proc/self/fd", O_PATH | O_DIRECTORY));
  atomic_store(&thread_anchor,
               open("/proc/thread-self/fd", O_PATH | O_DIRECTORY));
  return NULL;
}

static void inspect(const char *label, int fd) {
  struct stat st;
  struct statx stx;
  char proc_path[64];
  char target[256];
  int n = snprintf(proc_path, sizeof(proc_path), "/proc/self/fd/%d", fd);
  if (n < 0 || (size_t)n >= sizeof(proc_path)) _exit(90);

  errno = 0;
  int fstat_rc = fstat(fd, &st);
  int fstat_errno = errno;
  mode_t fstat_mode = fstat_rc == 0 ? st.st_mode : 0;

  errno = 0;
  int empty_statx_rc = syscall(SYS_statx, fd, "", AT_EMPTY_PATH,
                               STATX_BASIC_STATS, &stx);
  int empty_statx_errno = errno;
  unsigned empty_statx_mode = empty_statx_rc == 0 ? stx.stx_mode : 0;

  errno = 0;
  int path_stat_rc = stat(proc_path, &st);
  int path_stat_errno = errno;

  errno = 0;
  int path_lstat_rc = lstat(proc_path, &st);
  int path_lstat_errno = errno;

  errno = 0;
  ssize_t readlink_rc = readlink(proc_path, target, sizeof(target));
  int readlink_errno = errno;

  errno = 0;
  int reopened = open(proc_path, O_PATH | O_DIRECTORY);
  int reopen_errno = errno;

  errno = 0;
  ssize_t child_rc = readlinkat(fd, "0", target, sizeof(target));
  int child_errno = errno;

  printf("%s fd=%d fstat=%d/%d/%#o empty_statx=%d/%d/%#o "
         "path_stat=%d/%d path_lstat=%d/%d readlink=%zd/%d "
         "reopen=%d/%d child=%zd/%d\n",
         label, fd, fstat_rc, fstat_errno, fstat_mode, empty_statx_rc,
         empty_statx_errno, empty_statx_mode, path_stat_rc, path_stat_errno,
         path_lstat_rc, path_lstat_errno, readlink_rc, readlink_errno,
         reopened, reopen_errno, child_rc, child_errno);
  if (reopened >= 0) close(reopened);
}

int main(void) {
  pthread_t thread;
  if (pthread_create(&thread, NULL, worker, NULL) != 0) return 10;
  if (pthread_join(thread, NULL) != 0) return 11;
  for (int delay = 0; delay < 4; ++delay) {
    if (delay == 1) usleep(10000);
    if (delay == 2) usleep(100000);
    if (delay == 3) sleep(1);
    printf("delay=%d ", delay);
    inspect("self", atomic_load(&self_anchor));
    printf("delay=%d ", delay);
    inspect("thread", atomic_load(&thread_anchor));
  }
  return 0;
}
