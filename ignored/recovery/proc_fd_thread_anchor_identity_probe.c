#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

struct shared_state {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    int ready;
    int exit_requested;
    pid_t worker_tid;
    int self_fd_dir;
    int self_errno;
    int thread_self_fd_dir;
    int thread_self_errno;
    struct stat worker_self_stat;
    int worker_self_stat_errno;
    struct stat worker_thread_self_stat;
    int worker_thread_self_stat_errno;
};

static struct shared_state shared = {
    .mutex = PTHREAD_MUTEX_INITIALIZER,
    .cond = PTHREAD_COND_INITIALIZER,
    .self_fd_dir = -1,
    .thread_self_fd_dir = -1,
};

static void *worker(void *unused)
{
    (void)unused;
    shared.worker_tid = (pid_t)syscall(SYS_gettid);

    errno = 0;
    shared.self_fd_dir =
        open("/proc/self/fd", O_PATH | O_DIRECTORY | O_CLOEXEC);
    shared.self_errno = shared.self_fd_dir < 0 ? errno : 0;

    errno = 0;
    shared.thread_self_fd_dir =
        open("/proc/thread-self/fd", O_PATH | O_DIRECTORY | O_CLOEXEC);
    shared.thread_self_errno = shared.thread_self_fd_dir < 0 ? errno : 0;

    errno = 0;
    shared.worker_self_stat_errno =
        stat("/proc/self/fd", &shared.worker_self_stat) < 0 ? errno : 0;
    errno = 0;
    shared.worker_thread_self_stat_errno =
        stat("/proc/thread-self/fd", &shared.worker_thread_self_stat) < 0
            ? errno
            : 0;

    pthread_mutex_lock(&shared.mutex);
    shared.ready = 1;
    pthread_cond_broadcast(&shared.cond);
    while (!shared.exit_requested)
        pthread_cond_wait(&shared.cond, &shared.mutex);
    pthread_mutex_unlock(&shared.mutex);
    return NULL;
}

static int report_fstat(const char *label, int fd, struct stat *result)
{
    errno = 0;
    int rc = fstat(fd, result);
    int error = rc < 0 ? errno : 0;
    if (rc == 0) {
        printf("fstat label=%s fd=%d rc=0 errno=0 dev=%llu ino=%llu mode=%#o\n",
               label, fd, (unsigned long long)result->st_dev,
               (unsigned long long)result->st_ino,
               (unsigned int)result->st_mode);
    } else {
        printf("fstat label=%s fd=%d rc=-1 errno=%d (%s)\n", label, fd,
               error, strerror(error));
    }
    return rc;
}

static int report_stat(const char *label, const char *path, struct stat *result)
{
    errno = 0;
    int rc = stat(path, result);
    int error = rc < 0 ? errno : 0;
    if (rc == 0) {
        printf("stat label=%s path=%s rc=0 errno=0 dev=%llu ino=%llu mode=%#o\n",
               label, path, (unsigned long long)result->st_dev,
               (unsigned long long)result->st_ino,
               (unsigned int)result->st_mode);
    } else {
        printf("stat label=%s path=%s rc=-1 errno=%d (%s)\n", label, path,
               error, strerror(error));
    }
    return rc;
}

static void report_readlink(const char *label, int fd)
{
    char path[64];
    char destination[4096];
    int length = snprintf(path, sizeof(path), "/proc/self/fd/%d", fd);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        printf("readlink label=%s skipped fd=%d\n", label, fd);
        return;
    }

    errno = 0;
    ssize_t rc = readlink(path, destination, sizeof(destination) - 1);
    int error = rc < 0 ? errno : 0;
    if (rc >= 0) {
        destination[rc] = '\0';
        printf("readlink label=%s path=%s rc=%zd errno=0 destination=%s\n",
               label, path, rc, destination);
    } else {
        printf("readlink label=%s path=%s rc=-1 errno=%d (%s)\n", label,
               path, error, strerror(error));
    }
}

static void report_equal(const char *label, const struct stat *left,
                         const struct stat *right)
{
    printf("compare label=%s dev_equal=%d ino_equal=%d same_identity=%d\n",
           label, left->st_dev == right->st_dev, left->st_ino == right->st_ino,
           left->st_dev == right->st_dev && left->st_ino == right->st_ino);
}

int main(void)
{
    pthread_t thread;
    int result = pthread_create(&thread, NULL, worker, NULL);
    if (result != 0) {
        fprintf(stderr, "pthread_create: %s\n", strerror(result));
        return 1;
    }

    pthread_mutex_lock(&shared.mutex);
    while (!shared.ready)
        pthread_cond_wait(&shared.cond, &shared.mutex);
    pthread_mutex_unlock(&shared.mutex);

    pid_t tgid = getpid();
    pid_t leader_tid = (pid_t)syscall(SYS_gettid);
    char pid_fd_path[128];
    char worker_fd_path[128];
    snprintf(pid_fd_path, sizeof(pid_fd_path), "/proc/%ld/fd", (long)tgid);
    snprintf(worker_fd_path, sizeof(worker_fd_path),
             "/proc/%ld/task/%ld/fd", (long)tgid, (long)shared.worker_tid);

    printf("identity tgid=%ld leader_tid=%ld worker_tid=%ld\n", (long)tgid,
           (long)leader_tid, (long)shared.worker_tid);
    printf("open anchor=/proc/self/fd fd=%d errno=%d (%s)\n",
           shared.self_fd_dir, shared.self_errno,
           shared.self_errno == 0 ? "Success" : strerror(shared.self_errno));
    printf("open anchor=/proc/thread-self/fd fd=%d errno=%d (%s)\n",
           shared.thread_self_fd_dir, shared.thread_self_errno,
           shared.thread_self_errno == 0 ? "Success"
                                         : strerror(shared.thread_self_errno));

    report_readlink("worker-opened-self", shared.self_fd_dir);
    report_readlink("worker-opened-thread-self", shared.thread_self_fd_dir);

    struct stat self_anchor = {0};
    struct stat thread_anchor = {0};
    struct stat pid_fd = {0};
    struct stat worker_fd = {0};
    struct stat leader_thread_self = {0};
    int self_anchor_ok =
        shared.self_fd_dir >= 0 &&
        report_fstat("worker-opened-self", shared.self_fd_dir, &self_anchor) == 0;
    int thread_anchor_ok =
        shared.thread_self_fd_dir >= 0 &&
        report_fstat("worker-opened-thread-self", shared.thread_self_fd_dir,
                     &thread_anchor) == 0;
    int pid_fd_ok = report_stat("canonical-process-fd", pid_fd_path, &pid_fd) == 0;
    int worker_fd_ok =
        report_stat("canonical-worker-fd", worker_fd_path, &worker_fd) == 0;
    int leader_thread_self_ok =
        report_stat("leader-thread-self", "/proc/thread-self/fd",
                    &leader_thread_self) == 0;

    printf("worker_stat label=/proc/self/fd errno=%d", shared.worker_self_stat_errno);
    if (shared.worker_self_stat_errno == 0)
        printf(" dev=%llu ino=%llu\n",
               (unsigned long long)shared.worker_self_stat.st_dev,
               (unsigned long long)shared.worker_self_stat.st_ino);
    else
        printf(" (%s)\n", strerror(shared.worker_self_stat_errno));
    printf("worker_stat label=/proc/thread-self/fd errno=%d",
           shared.worker_thread_self_stat_errno);
    if (shared.worker_thread_self_stat_errno == 0)
        printf(" dev=%llu ino=%llu\n",
               (unsigned long long)shared.worker_thread_self_stat.st_dev,
               (unsigned long long)shared.worker_thread_self_stat.st_ino);
    else
        printf(" (%s)\n", strerror(shared.worker_thread_self_stat_errno));

    if (self_anchor_ok && pid_fd_ok)
        report_equal("self-anchor-vs-canonical-process", &self_anchor, &pid_fd);
    if (self_anchor_ok && shared.worker_self_stat_errno == 0)
        report_equal("self-anchor-vs-worker-direct-self", &self_anchor,
                     &shared.worker_self_stat);
    if (thread_anchor_ok && worker_fd_ok)
        report_equal("thread-anchor-vs-canonical-worker", &thread_anchor,
                     &worker_fd);
    if (thread_anchor_ok && shared.worker_thread_self_stat_errno == 0)
        report_equal("thread-anchor-vs-worker-direct-thread-self", &thread_anchor,
                     &shared.worker_thread_self_stat);
    if (thread_anchor_ok && leader_thread_self_ok)
        report_equal("thread-anchor-vs-leader-thread-self", &thread_anchor,
                     &leader_thread_self);
    if (self_anchor_ok && thread_anchor_ok)
        report_equal("self-anchor-vs-thread-anchor", &self_anchor, &thread_anchor);

    pthread_mutex_lock(&shared.mutex);
    shared.exit_requested = 1;
    pthread_cond_broadcast(&shared.cond);
    pthread_mutex_unlock(&shared.mutex);
    result = pthread_join(thread, NULL);
    if (result != 0) {
        fprintf(stderr, "pthread_join: %s\n", strerror(result));
        return 1;
    }

    if (shared.thread_self_fd_dir >= 0)
        close(shared.thread_self_fd_dir);
    if (shared.self_fd_dir >= 0)
        close(shared.self_fd_dir);
    return 0;
}
