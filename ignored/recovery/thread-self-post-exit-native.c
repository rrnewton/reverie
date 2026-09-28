#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int status_fd = -1;
static int stat_fd = -1;
static int started_status_fd = -1;
static int started_status_dup = -1;
static int started_stat_fd = -1;
static int started_stat_dup = -1;
static unsigned char status_prefix[17];
static unsigned char stat_prefix[17];

static void *opener(void *unused) {
  (void)unused;
  if (pthread_setname_np(pthread_self(), "gone-worker") != 0) {
    return (void *)1;
  }
  status_fd = open("/proc/thread-self/status", O_RDONLY | O_CLOEXEC);
  stat_fd = open("/proc/thread-self/stat", O_RDONLY | O_CLOEXEC);
  started_status_fd = open("/proc/thread-self/status", O_RDONLY | O_CLOEXEC);
  started_stat_fd = open("/proc/thread-self/stat", O_RDONLY | O_CLOEXEC);
  started_status_dup = dup(started_status_fd);
  started_stat_dup = dup(started_stat_fd);
  if (status_fd < 0 || stat_fd < 0 || started_status_fd < 0 ||
      started_stat_fd < 0 || started_status_dup < 0 || started_stat_dup < 0) {
    return (void *)2;
  }
  if (read(started_status_fd, status_prefix, sizeof(status_prefix)) !=
          (ssize_t)sizeof(status_prefix) ||
      read(started_stat_fd, stat_prefix, sizeof(stat_prefix)) !=
          (ssize_t)sizeof(stat_prefix)) {
    return (void *)3;
  }
  return NULL;
}

static void observe_continuation(const char *label, int fd, int original_fd,
                                 const unsigned char *prefix,
                                 size_t prefix_length) {
  unsigned char buffer[4096];
  memcpy(buffer, prefix, prefix_length);
  errno = 0;
  ssize_t count = read(fd, buffer + prefix_length,
                       sizeof(buffer) - prefix_length);
  int error = count < 0 ? errno : 0;
  size_t total = count < 0 ? prefix_length : prefix_length + (size_t)count;
  int mentions_name = memmem(buffer, total, "gone-worker", 11) != NULL;
  off_t shared_offset = lseek(original_fd, 0, SEEK_CUR);
  printf("%s continuation=%zd errno=%d total=%zu mentions_opener_name=%d "
         "shared_offset=%lld\n",
         label, count, error, total, mentions_name, (long long)shared_offset);
}

static void observe(const char *label, int fd) {
  unsigned char buffer[4096];
  errno = 0;
  ssize_t count = read(fd, buffer, sizeof(buffer));
  int error = count < 0 ? errno : 0;
  int mentions_name =
      count > 0 && memmem(buffer, (size_t)count, "gone-worker", 11) != NULL;
  printf("%s read=%zd errno=%d mentions_opener_name=%d\n", label, count,
         error, mentions_name);
}

int main(void) {
  pthread_t thread;
  if (pthread_create(&thread, NULL, opener, NULL) != 0) {
    return 2;
  }
  void *result = NULL;
  if (pthread_join(thread, &result) != 0 || result != NULL) {
    return 3;
  }
  observe("status-after-opener-exit", status_fd);
  observe("stat-after-opener-exit", stat_fd);
  observe_continuation("status-started-before-exit", started_status_dup,
                       started_status_fd, status_prefix,
                       sizeof(status_prefix));
  observe_continuation("stat-started-before-exit", started_stat_dup,
                       started_stat_fd, stat_prefix, sizeof(stat_prefix));
  close(status_fd);
  close(stat_fd);
  close(started_status_fd);
  close(started_status_dup);
  close(started_stat_fd);
  close(started_stat_dup);
  return 0;
}
