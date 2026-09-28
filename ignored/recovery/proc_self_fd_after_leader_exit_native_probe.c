#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int target_fd;

static int wait_for_leader_exit(void) {
  char path[64];
  if (snprintf(path, sizeof(path), "/proc/self/fd/%d", target_fd) < 0)
    return -1;

  const struct timespec delay = {.tv_sec = 0, .tv_nsec = 1000000};
  for (int attempt = 0; attempt < 5000; ++attempt) {
    char target[128];
    if (readlink(path, target, sizeof(target)) < 0) {
      return errno == ENOENT ? 0 : -1;
    }
    nanosleep(&delay, NULL);
  }
  errno = ETIMEDOUT;
  return -1;
}

static void *worker(void *unused) {
  (void)unused;
  if (wait_for_leader_exit() != 0) {
    perror("wait_for_leader_exit");
    exit(10);
  }

  struct stat status;
  errno = 0;
  int self_stat = stat("/proc/self/fd", &status);
  int self_stat_errno = self_stat == 0 ? 0 : errno;

  errno = 0;
  int self_anchor = open("/proc/self/fd", O_PATH | O_DIRECTORY | O_CLOEXEC);
  int self_open_errno = self_anchor >= 0 ? 0 : errno;

  char target_path[64];
  if (snprintf(target_path, sizeof(target_path), "/proc/self/fd/%d", target_fd) < 0)
    exit(11);
  char bytes[128];
  errno = 0;
  ssize_t absolute_count = readlink(target_path, bytes, sizeof(bytes));
  int absolute_errno = absolute_count >= 0 ? 0 : errno;

  errno = 0;
  ssize_t self_relative_count =
      self_anchor >= 0
          ? readlinkat(self_anchor, "3", bytes, sizeof(bytes))
          : -1;
  int self_relative_errno = self_relative_count >= 0 ? 0 : errno;

  errno = 0;
  int thread_anchor =
      open("/proc/thread-self/fd", O_PATH | O_DIRECTORY | O_CLOEXEC);
  int thread_open_errno = thread_anchor >= 0 ? 0 : errno;
  errno = 0;
  ssize_t thread_relative_count =
      thread_anchor >= 0
          ? readlinkat(thread_anchor, "3", bytes, sizeof(bytes))
          : -1;
  int thread_relative_errno = thread_relative_count >= 0 ? 0 : errno;

  printf("leader_gone=1 self_stat_rc=%d self_stat_errno=%d self_is_dir=%d "
         "self_open_ok=%d self_open_errno=%d absolute_rc=%zd absolute_errno=%d "
         "self_relative_rc=%zd self_relative_errno=%d thread_open_ok=%d "
         "thread_open_errno=%d thread_relative=%.*s thread_relative_errno=%d\n",
         self_stat, self_stat_errno,
         self_stat == 0 && S_ISDIR(status.st_mode), self_anchor >= 0,
         self_open_errno, absolute_count, absolute_errno, self_relative_count,
         self_relative_errno, thread_anchor >= 0, thread_open_errno,
         thread_relative_count > 0 ? (int)thread_relative_count : 0, bytes,
         thread_relative_errno);

  if (self_anchor >= 0)
    close(self_anchor);
  if (thread_anchor >= 0)
    close(thread_anchor);
  close(target_fd);

  int ok = self_stat == 0 && S_ISDIR(status.st_mode) && self_anchor >= 0 &&
           absolute_count == -1 && absolute_errno == ENOENT &&
           self_relative_count == -1 && self_relative_errno == ENOENT &&
           thread_anchor >= 0 && thread_relative_count == 9 &&
           memcmp(bytes, "/dev/null", 9) == 0;
  exit(ok ? 0 : 12);
}

int main(void) {
  target_fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
  if (target_fd < 0) {
    perror("open /dev/null");
    return 1;
  }
  if (target_fd != 3) {
    fprintf(stderr, "expected target fd 3, got %d\n", target_fd);
    return 2;
  }

  pthread_t thread;
  int error = pthread_create(&thread, NULL, worker, NULL);
  if (error != 0) {
    fprintf(stderr, "pthread_create: %s\n", strerror(error));
    return 3;
  }

  pthread_exit(NULL);
}
