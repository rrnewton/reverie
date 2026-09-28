#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

struct proc_identity {
  long pid;
  long tgid;
  long ppid;
  char state;
  char name[64];
};

struct pwrite_result {
  ssize_t result;
  int error;
};

struct shared_state {
  pthread_barrier_t ready;
  pthread_barrier_t release;
  long process_id;
  long process_parent_id;
  long main_tid;
  long worker_tid;
  int main_status_fd;
  int main_stat_fd;
  int worker_status_fd;
  int worker_status_dup;
  int worker_status_independent;
  int worker_stat_fd;
  unsigned char prefix[32];
  size_t prefix_len;
  struct proc_identity main_status_read_by_worker;
  struct proc_identity main_stat_read_by_worker;
  struct pwrite_result status_one;
  struct pwrite_result status_zero;
  struct pwrite_result stat_one;
  struct pwrite_result stat_zero;
  int worker_error;
};

static ssize_t read_to_end(int fd, unsigned char *buffer, size_t capacity) {
  size_t used = 0;
  while (used < capacity) {
    ssize_t count = read(fd, buffer + used, capacity - used);
    if (count > 0) {
      used += (size_t)count;
      continue;
    }
    if (count == 0) {
      return (ssize_t)used;
    }
    if (errno == EINTR) {
      continue;
    }
    return -1;
  }
  errno = EOVERFLOW;
  return -1;
}

static int parse_status(unsigned char *buffer, size_t length,
                        struct proc_identity *identity) {
  if (length >= 8192) {
    return -1;
  }
  buffer[length] = '\0';
  identity->pid = -1;
  identity->tgid = -1;
  identity->ppid = -1;
  identity->state = '\0';
  identity->name[0] = '\0';

  char *save = NULL;
  for (char *line = strtok_r((char *)buffer, "\n", &save); line != NULL;
       line = strtok_r(NULL, "\n", &save)) {
    if (strncmp(line, "Name:\t", 6) == 0) {
      snprintf(identity->name, sizeof(identity->name), "%s", line + 6);
    } else if (strncmp(line, "Tgid:", 5) == 0) {
      identity->tgid = strtol(line + 5, NULL, 10);
    } else if (strncmp(line, "Pid:", 4) == 0) {
      identity->pid = strtol(line + 4, NULL, 10);
    } else if (strncmp(line, "PPid:", 5) == 0) {
      identity->ppid = strtol(line + 5, NULL, 10);
    }
  }
  return identity->pid >= 0 && identity->tgid >= 0 && identity->ppid >= 0 &&
                 identity->name[0] != '\0'
             ? 0
             : -1;
}

static int parse_stat(unsigned char *buffer, size_t length,
                      struct proc_identity *identity) {
  if (length >= 8192) {
    return -1;
  }
  buffer[length] = '\0';
  identity->pid = -1;
  identity->tgid = -1;
  identity->ppid = -1;
  identity->state = '\0';
  identity->name[0] = '\0';
  return sscanf((char *)buffer, "%ld (%63[^)]) %c %ld", &identity->pid,
                identity->name, &identity->state, &identity->ppid) == 4
             ? 0
             : -1;
}

static int read_status_identity(int fd, struct proc_identity *identity) {
  unsigned char buffer[8193];
  ssize_t count = read_to_end(fd, buffer, 8192);
  return count >= 0 ? parse_status(buffer, (size_t)count, identity) : -1;
}

static int read_stat_identity(int fd, struct proc_identity *identity) {
  unsigned char buffer[8193];
  ssize_t count = read_to_end(fd, buffer, 8192);
  return count >= 0 ? parse_stat(buffer, (size_t)count, identity) : -1;
}

static struct pwrite_result try_pwrite(int fd, size_t count) {
  const unsigned char byte = 'x';
  errno = 0;
  ssize_t result = syscall(SYS_pwrite64, fd, &byte, count, 0);
  struct pwrite_result observed = {.result = result,
                                   .error = result < 0 ? errno : 0};
  return observed;
}

static void *worker(void *opaque) {
  struct shared_state *shared = opaque;
  shared->worker_tid = syscall(SYS_gettid);
  if (pthread_setname_np(pthread_self(), "ts-worker") != 0) {
    shared->worker_error = 10;
    goto ready;
  }

  if (read_status_identity(shared->main_status_fd,
                           &shared->main_status_read_by_worker) != 0) {
    shared->worker_error = 11;
    goto ready;
  }
  if (read_stat_identity(shared->main_stat_fd,
                         &shared->main_stat_read_by_worker) != 0) {
    shared->worker_error = 12;
    goto ready;
  }

  shared->worker_status_fd =
      open("/proc/thread-self/status", O_RDONLY | O_CLOEXEC);
  shared->worker_stat_fd = open("/proc/thread-self/stat", O_RDONLY | O_CLOEXEC);
  shared->worker_status_independent =
      open("/proc/thread-self/status", O_RDONLY | O_CLOEXEC);
  if (shared->worker_status_fd < 0 || shared->worker_stat_fd < 0 ||
      shared->worker_status_independent < 0) {
    shared->worker_error = 13;
    goto ready;
  }
  shared->worker_status_dup = dup(shared->worker_status_fd);
  if (shared->worker_status_dup < 0) {
    shared->worker_error = 14;
    goto ready;
  }

  shared->status_one = try_pwrite(shared->worker_status_fd, 1);
  shared->status_zero = try_pwrite(shared->worker_status_fd, 0);
  shared->stat_one = try_pwrite(shared->worker_stat_fd, 1);
  shared->stat_zero = try_pwrite(shared->worker_stat_fd, 0);

  ssize_t prefix = read(shared->worker_status_fd, shared->prefix, 17);
  if (prefix != 17) {
    shared->worker_error = 15;
    goto ready;
  }
  shared->prefix_len = (size_t)prefix;

ready:
  pthread_barrier_wait(&shared->ready);
  pthread_barrier_wait(&shared->release);
  return (void *)(intptr_t)shared->worker_error;
}

static int identity_matches(const struct proc_identity *identity, long pid,
                            long tgid, long ppid, const char *name) {
  return identity->pid == pid && identity->tgid == tgid &&
         identity->ppid == ppid && strcmp(identity->name, name) == 0;
}

int main(void) {
  struct shared_state shared;
  memset(&shared, 0, sizeof(shared));
  shared.main_status_fd = -1;
  shared.main_stat_fd = -1;
  shared.worker_status_fd = -1;
  shared.worker_status_dup = -1;
  shared.worker_status_independent = -1;
  shared.worker_stat_fd = -1;
  shared.process_id = getpid();
  shared.process_parent_id = getppid();
  shared.main_tid = syscall(SYS_gettid);

  if (pthread_setname_np(pthread_self(), "ts-main") != 0) {
    return 2;
  }
  shared.main_status_fd =
      open("/proc/thread-self/status", O_RDONLY | O_CLOEXEC);
  shared.main_stat_fd = open("/proc/thread-self/stat", O_RDONLY | O_CLOEXEC);
  if (shared.main_status_fd < 0 || shared.main_stat_fd < 0) {
    perror("main open /proc/thread-self");
    return 3;
  }
  if (pthread_barrier_init(&shared.ready, NULL, 2) != 0 ||
      pthread_barrier_init(&shared.release, NULL, 2) != 0) {
    return 4;
  }

  pthread_t thread;
  if (pthread_create(&thread, NULL, worker, &shared) != 0) {
    return 5;
  }
  pthread_barrier_wait(&shared.ready);

  int failure = shared.worker_error;
  struct proc_identity worker_status_from_shared_cursor;
  struct proc_identity worker_status_from_independent;
  struct proc_identity worker_stat;
  memset(&worker_status_from_shared_cursor, 0,
         sizeof(worker_status_from_shared_cursor));
  memset(&worker_status_from_independent, 0,
         sizeof(worker_status_from_independent));
  memset(&worker_stat, 0, sizeof(worker_stat));

  off_t shared_before = -1;
  off_t shared_after = -1;
  off_t independent_before = -1;
  ssize_t combined_length = -1;
  if (failure == 0) {
    shared_before = lseek(shared.worker_status_dup, 0, SEEK_CUR);
    independent_before = lseek(shared.worker_status_independent, 0, SEEK_CUR);

    unsigned char combined[8193];
    memcpy(combined, shared.prefix, shared.prefix_len);
    ssize_t suffix = read_to_end(shared.worker_status_dup,
                                 combined + shared.prefix_len,
                                 8192 - shared.prefix_len);
    if (suffix < 0) {
      failure = 20;
    } else {
      combined_length = (ssize_t)shared.prefix_len + suffix;
      if (parse_status(combined, (size_t)combined_length,
                       &worker_status_from_shared_cursor) != 0) {
        failure = 21;
      }
    }
    shared_after = lseek(shared.worker_status_fd, 0, SEEK_CUR);

    if (failure == 0 &&
        read_status_identity(shared.worker_status_independent,
                             &worker_status_from_independent) != 0) {
      failure = 22;
    }
    if (failure == 0 &&
        read_stat_identity(shared.worker_stat_fd, &worker_stat) != 0) {
      failure = 23;
    }
  }

  int main_to_worker_status =
      identity_matches(&shared.main_status_read_by_worker, shared.main_tid,
                       shared.process_id, shared.process_parent_id, "ts-main");
  int main_to_worker_stat =
      shared.main_stat_read_by_worker.pid == shared.main_tid &&
      shared.main_stat_read_by_worker.ppid == shared.process_parent_id &&
      strcmp(shared.main_stat_read_by_worker.name, "ts-main") == 0;
  int worker_to_main_status =
      identity_matches(&worker_status_from_shared_cursor, shared.worker_tid,
                       shared.process_id, shared.process_parent_id, "ts-worker");
  int worker_independent_status =
      identity_matches(&worker_status_from_independent, shared.worker_tid,
                       shared.process_id, shared.process_parent_id, "ts-worker");
  int worker_to_main_stat = worker_stat.pid == shared.worker_tid &&
                            worker_stat.ppid == shared.process_parent_id &&
                            strcmp(worker_stat.name, "ts-worker") == 0;
  int distinct_thread_ids = shared.main_tid == shared.process_id &&
                            shared.worker_tid != shared.main_tid;
  int shared_cursor = shared_before == (off_t)shared.prefix_len &&
                      shared_after == combined_length;
  int independent_cursor = independent_before == 0;

  printf("raw pid=%ld main_tid=%ld worker_tid=%ld\n", shared.process_id,
         shared.main_tid, shared.worker_tid);
  printf("main-open/read-worker status_name=%s status_pid=%ld status_tgid=%ld "
         "status_ppid=%ld stat_name=%s stat_pid=%ld stat_ppid=%ld "
         "stat_state=%c\n",
         shared.main_status_read_by_worker.name,
         shared.main_status_read_by_worker.pid,
         shared.main_status_read_by_worker.tgid,
         shared.main_status_read_by_worker.ppid,
         shared.main_stat_read_by_worker.name, shared.main_stat_read_by_worker.pid,
         shared.main_stat_read_by_worker.ppid,
         shared.main_stat_read_by_worker.state);
  printf("worker-open/read-main status_name=%s status_pid=%ld status_tgid=%ld "
         "status_ppid=%ld stat_name=%s stat_pid=%ld stat_ppid=%ld "
         "stat_state=%c\n",
         worker_status_from_shared_cursor.name,
         worker_status_from_shared_cursor.pid,
         worker_status_from_shared_cursor.tgid,
         worker_status_from_shared_cursor.ppid, worker_stat.name,
         worker_stat.pid, worker_stat.ppid, worker_stat.state);
  printf("cursor prefix=%zu shared_before=%lld combined=%zd shared_after=%lld "
         "independent_before=%lld\n",
         shared.prefix_len, (long long)shared_before, combined_length,
         (long long)shared_after, (long long)independent_before);
  printf("pwrite status_one=%zd/%d status_zero=%zd/%d stat_one=%zd/%d "
         "stat_zero=%zd/%d errno_ESPIPE=%d\n",
         shared.status_one.result, shared.status_one.error,
         shared.status_zero.result, shared.status_zero.error,
         shared.stat_one.result, shared.stat_one.error, shared.stat_zero.result,
         shared.stat_zero.error, ESPIPE);
  printf("normalized distinct=%d main_status=%d main_stat=%d worker_status=%d "
         "worker_status_independent=%d worker_stat=%d shared_cursor=%d "
         "independent_cursor=%d pwrite_status_one_espipe=%d "
         "pwrite_status_zero_espipe=%d pwrite_stat_one_espipe=%d "
         "pwrite_stat_zero_espipe=%d\n",
         distinct_thread_ids, main_to_worker_status, main_to_worker_stat,
         worker_to_main_status, worker_independent_status, worker_to_main_stat,
         shared_cursor, independent_cursor,
         shared.status_one.result == -1 && shared.status_one.error == ESPIPE,
         shared.status_zero.result == -1 && shared.status_zero.error == ESPIPE,
         shared.stat_one.result == -1 && shared.stat_one.error == ESPIPE,
         shared.stat_zero.result == -1 && shared.stat_zero.error == ESPIPE);

  if (!distinct_thread_ids || !main_to_worker_status || !main_to_worker_stat ||
      !worker_to_main_status || !worker_independent_status ||
      !worker_to_main_stat || !shared_cursor || !independent_cursor) {
    failure = failure == 0 ? 30 : failure;
  }

  pthread_barrier_wait(&shared.release);
  void *worker_result = NULL;
  if (pthread_join(thread, &worker_result) != 0 ||
      (intptr_t)worker_result != shared.worker_error) {
    failure = failure == 0 ? 31 : failure;
  }

  if (shared.main_status_fd >= 0) close(shared.main_status_fd);
  if (shared.main_stat_fd >= 0) close(shared.main_stat_fd);
  if (shared.worker_status_fd >= 0) close(shared.worker_status_fd);
  if (shared.worker_status_dup >= 0) close(shared.worker_status_dup);
  if (shared.worker_status_independent >= 0)
    close(shared.worker_status_independent);
  if (shared.worker_stat_fd >= 0) close(shared.worker_stat_fd);
  pthread_barrier_destroy(&shared.ready);
  pthread_barrier_destroy(&shared.release);

  if (failure != 0) {
    fprintf(stderr, "probe failure=%d worker_error=%d\n", failure,
            shared.worker_error);
    return failure;
  }
  return 0;
}
