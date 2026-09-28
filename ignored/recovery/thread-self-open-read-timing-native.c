#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

struct shared_state {
  pthread_barrier_t ready;
  pthread_barrier_t release;
  int delayed_status;
  int delayed_stat;
  int started_status;
  int started_status_dup;
  int started_stat;
  int started_stat_dup;
  unsigned char status_prefix[32];
  unsigned char stat_prefix[32];
  int error;
};

static ssize_t read_to_end(int fd, unsigned char *buffer, size_t capacity) {
  size_t used = 0;
  while (used < capacity) {
    ssize_t count = read(fd, buffer + used, capacity - used);
    if (count > 0) {
      used += (size_t)count;
    } else if (count == 0) {
      return (ssize_t)used;
    } else if (errno != EINTR) {
      return -1;
    }
  }
  return (ssize_t)used;
}

static int contains(const unsigned char *buffer, size_t length,
                    const char *needle) {
  return memmem(buffer, length, needle, strlen(needle)) != NULL;
}

static void *worker(void *opaque) {
  struct shared_state *shared = opaque;
  if (pthread_setname_np(pthread_self(), "ts-open") != 0) {
    shared->error = 10;
    goto ready;
  }
  shared->delayed_status = open("/proc/thread-self/status", O_RDONLY | O_CLOEXEC);
  shared->delayed_stat = open("/proc/thread-self/stat", O_RDONLY | O_CLOEXEC);
  shared->started_status = open("/proc/thread-self/status", O_RDONLY | O_CLOEXEC);
  shared->started_stat = open("/proc/thread-self/stat", O_RDONLY | O_CLOEXEC);
  shared->started_status_dup = dup(shared->started_status);
  shared->started_stat_dup = dup(shared->started_stat);
  if (shared->delayed_status < 0 || shared->delayed_stat < 0 ||
      shared->started_status < 0 || shared->started_stat < 0 ||
      shared->started_status_dup < 0 || shared->started_stat_dup < 0) {
    shared->error = 11;
    goto ready;
  }
  if (read(shared->started_status, shared->status_prefix,
           sizeof(shared->status_prefix)) !=
          (ssize_t)sizeof(shared->status_prefix) ||
      read(shared->started_stat, shared->stat_prefix,
           sizeof(shared->stat_prefix)) != (ssize_t)sizeof(shared->stat_prefix)) {
    shared->error = 12;
    goto ready;
  }
  if (pthread_setname_np(pthread_self(), "ts-read") != 0) {
    shared->error = 13;
  }

ready:
  pthread_barrier_wait(&shared->ready);
  pthread_barrier_wait(&shared->release);
  return (void *)(long)shared->error;
}

static int read_and_report(const char *label, int fd, const char *expected,
                           const char *unexpected) {
  unsigned char buffer[4096];
  ssize_t count = read_to_end(fd, buffer, sizeof(buffer));
  int has_expected = count >= 0 && contains(buffer, (size_t)count, expected);
  int has_unexpected = count >= 0 && contains(buffer, (size_t)count, unexpected);
  printf("%s read=%zd expected_name=%d stale_name=%d\n", label, count,
         has_expected, has_unexpected);
  return count >= 0 && has_expected && !has_unexpected ? 0 : -1;
}

static int continue_and_report(const char *label, int fd,
                               const unsigned char *prefix,
                               size_t prefix_length) {
  unsigned char buffer[4096];
  memcpy(buffer, prefix, prefix_length);
  ssize_t suffix = read_to_end(fd, buffer + prefix_length,
                               sizeof(buffer) - prefix_length);
  size_t total = suffix < 0 ? prefix_length : prefix_length + (size_t)suffix;
  int has_open = contains(buffer, total, "ts-open");
  int has_read = contains(buffer, total, "ts-read");
  printf("%s continuation=%zd open_name=%d read_name=%d\n", label, suffix,
         has_open, has_read);
  return suffix >= 0 && has_open && !has_read ? 0 : -1;
}

int main(void) {
  struct shared_state shared;
  memset(&shared, 0, sizeof(shared));
  shared.delayed_status = -1;
  shared.delayed_stat = -1;
  shared.started_status = -1;
  shared.started_status_dup = -1;
  shared.started_stat = -1;
  shared.started_stat_dup = -1;
  if (pthread_barrier_init(&shared.ready, NULL, 2) != 0 ||
      pthread_barrier_init(&shared.release, NULL, 2) != 0) {
    return 2;
  }
  pthread_t thread;
  if (pthread_create(&thread, NULL, worker, &shared) != 0) {
    return 3;
  }
  pthread_barrier_wait(&shared.ready);

  int failure = shared.error;
  if (!failure &&
      read_and_report("status-open-before/read-after-rename",
                      shared.delayed_status, "ts-read", "ts-open") != 0)
    failure = 20;
  if (!failure &&
      read_and_report("stat-open-before/read-after-rename", shared.delayed_stat,
                      "ts-read", "ts-open") != 0)
    failure = 21;
  if (!failure &&
      continue_and_report("status-first-read-before-rename",
                          shared.started_status_dup, shared.status_prefix,
                          sizeof(shared.status_prefix)) != 0)
    failure = 22;
  if (!failure &&
      continue_and_report("stat-first-read-before-rename",
                          shared.started_stat_dup, shared.stat_prefix,
                          sizeof(shared.stat_prefix)) != 0)
    failure = 23;

  if (!failure && lseek(shared.started_status, 0, SEEK_SET) != 0) failure = 24;
  if (!failure &&
      read_and_report("status-rewind-after-rename", shared.started_status,
                      "ts-read", "ts-open") != 0)
    failure = 25;
  if (!failure && lseek(shared.started_stat, 0, SEEK_SET) != 0) failure = 26;
  if (!failure &&
      read_and_report("stat-rewind-after-rename", shared.started_stat,
                      "ts-read", "ts-open") != 0)
    failure = 27;

  pthread_barrier_wait(&shared.release);
  void *result = NULL;
  if (pthread_join(thread, &result) != 0 || (long)result != shared.error) {
    failure = failure == 0 ? 28 : failure;
  }
  if (failure) {
    fprintf(stderr, "failure=%d worker_error=%d\n", failure, shared.error);
  }
  return failure;
}
