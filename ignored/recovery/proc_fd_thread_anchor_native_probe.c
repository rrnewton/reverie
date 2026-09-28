#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

struct shared_state {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    int ready;
    int exit_requested;
    pid_t worker_tid;
    int target_fd;
    int target_errno;
    int self_fd_dir;
    int self_errno;
    int thread_self_fd_dir;
    int thread_self_errno;
};

static struct shared_state shared = {
    .mutex = PTHREAD_MUTEX_INITIALIZER,
    .cond = PTHREAD_COND_INITIALIZER,
    .target_fd = -1,
    .self_fd_dir = -1,
    .thread_self_fd_dir = -1,
};

static void *worker(void *unused)
{
    (void)unused;

    shared.worker_tid = (pid_t)syscall(SYS_gettid);

    errno = 0;
    shared.target_fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
    shared.target_errno = shared.target_fd < 0 ? errno : 0;

    errno = 0;
    shared.self_fd_dir =
        open("/proc/self/fd", O_PATH | O_DIRECTORY | O_CLOEXEC);
    shared.self_errno = shared.self_fd_dir < 0 ? errno : 0;

    errno = 0;
    shared.thread_self_fd_dir =
        open("/proc/thread-self/fd", O_PATH | O_DIRECTORY | O_CLOEXEC);
    shared.thread_self_errno = shared.thread_self_fd_dir < 0 ? errno : 0;

    pthread_mutex_lock(&shared.mutex);
    shared.ready = 1;
    pthread_cond_broadcast(&shared.cond);
    while (!shared.exit_requested)
        pthread_cond_wait(&shared.cond, &shared.mutex);
    pthread_mutex_unlock(&shared.mutex);

    return NULL;
}

static void report_open(const char *anchor, int fd, int error)
{
    printf("open anchor=%s fd=%d errno=%d (%s)\n", anchor, fd, error,
           error == 0 ? "Success" : strerror(error));
}

static void report_readlink(const char *phase, const char *anchor, int dirfd,
                            int target_fd)
{
    char component[32];
    char destination[4096];
    int component_len = snprintf(component, sizeof(component), "%d", target_fd);

    if (dirfd < 0 || target_fd < 0 || component_len < 0 ||
        (size_t)component_len >= sizeof(component)) {
        printf("readlink phase=%s anchor=%s skipped dirfd=%d target_fd=%d\n",
               phase, anchor, dirfd, target_fd);
        return;
    }

    errno = 0;
    ssize_t result =
        readlinkat(dirfd, component, destination, sizeof(destination) - 1);
    int error = result < 0 ? errno : 0;
    if (result >= 0) {
        destination[result] = '\0';
        printf("readlink phase=%s anchor=%s dirfd=%d target_fd=%d rc=%zd "
               "errno=0 (Success) destination=%s\n",
               phase, anchor, dirfd, target_fd, result, destination);
    } else {
        printf("readlink phase=%s anchor=%s dirfd=%d target_fd=%d rc=-1 "
               "errno=%d (%s)\n",
               phase, anchor, dirfd, target_fd, error, strerror(error));
    }
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

    printf("identity tgid=%ld leader_tid=%ld worker_tid=%ld\n", (long)getpid(),
           (long)syscall(SYS_gettid), (long)shared.worker_tid);
    printf("target fd=%d errno=%d (%s)\n", shared.target_fd,
           shared.target_errno,
           shared.target_errno == 0 ? "Success" : strerror(shared.target_errno));
    report_open("/proc/self/fd", shared.self_fd_dir, shared.self_errno);
    report_open("/proc/thread-self/fd", shared.thread_self_fd_dir,
                shared.thread_self_errno);

    report_readlink("worker-alive", "/proc/self/fd", shared.self_fd_dir,
                    shared.target_fd);
    report_readlink("worker-alive", "/proc/thread-self/fd",
                    shared.thread_self_fd_dir, shared.target_fd);

    pthread_mutex_lock(&shared.mutex);
    shared.exit_requested = 1;
    pthread_cond_broadcast(&shared.cond);
    pthread_mutex_unlock(&shared.mutex);

    result = pthread_join(thread, NULL);
    if (result != 0) {
        fprintf(stderr, "pthread_join: %s\n", strerror(result));
        return 1;
    }
    printf("worker_join rc=0\n");

    report_readlink("worker-exited", "/proc/self/fd", shared.self_fd_dir,
                    shared.target_fd);
    report_readlink("worker-exited", "/proc/thread-self/fd",
                    shared.thread_self_fd_dir, shared.target_fd);

    if (shared.thread_self_fd_dir >= 0)
        close(shared.thread_self_fd_dir);
    if (shared.self_fd_dir >= 0)
        close(shared.self_fd_dir);
    if (shared.target_fd >= 0)
        close(shared.target_fd);
    return 0;
}
