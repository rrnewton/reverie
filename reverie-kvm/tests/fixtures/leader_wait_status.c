#define _GNU_SOURCE
#include <errno.h>
#include <linux/futex.h>
#include <pthread.h>
#include <stdatomic.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>
static _Atomic int leader_tid;
static int ready[2], release_worker[2], concurrent;
static void *worker(void *unused) {
    (void)unused;
    for (;;) {
        int tid = atomic_load(&leader_tid);
        if (!tid) break;
        long result = syscall(SYS_futex, &leader_tid, FUTEX_WAIT, tid, 0, 0, 0);
        if (result && errno != EAGAIN && errno != EINTR) syscall(SYS_exit_group, 91);
    }
    char byte = 'r';
    if (concurrent) {
    if (write(ready[1], &byte, 1) != 1) syscall(SYS_exit_group, 92);
    if (read(release_worker[0], &byte, 1) != 1 || byte != 'g') syscall(SYS_exit_group, 93);
    }
    syscall(SYS_exit, 73);
    __builtin_unreachable();
}
int main(int argc, char **argv) {
    if (argc != 2 && argc != 3) return 90;
    int mode = argc == 3 ? argv[2][0] - '0' : 0;
    if (mode < 0 || mode > 5) return 105;
    unsigned long options = WUNTRACED;
    if (mode == 2 || mode == 3) options |= WNOHANG;
    if (mode == 3) options |= 0xdeadbeefUL << 32;
    concurrent = argv[1][0] == '1';
    if (pipe(ready) || pipe(release_worker)) return 94;
    pid_t child = fork();
    if (child < 0) return 95;
    if (!child) {
        atomic_store(&leader_tid, syscall(SYS_set_tid_address, &leader_tid));
        pthread_t thread;
        if (pthread_create(&thread, 0, worker, 0)) syscall(SYS_exit_group, 96);
        syscall(SYS_exit, 37);
        __builtin_unreachable();
    }
    int status = 0;
    siginfo_t info = {0};
    if (concurrent) {
        char byte;
        if (read(ready[0], &byte, 1) != 1 || byte != 'r') return 97;
        if (mode) {
            errno = 0;
            if (syscall(SYS_wait4, child, &status, options | WNOHANG | 0x40, 0) != -1 ||
                errno != EINVAL) return 106;
            status = 0x5a5a5a5a;
            if (syscall(SYS_wait4, child, &status, options | WNOHANG, 0) != 0 ||
                status != 0x5a5a5a5a) return 107;
        }
        if (waitpid(child, &status, WNOHANG) != 0) return 98;
        if (waitid(P_PID, child, &info, WEXITED | WNOWAIT | WNOHANG) || info.si_pid != 0) return 99;
        byte = 'g';
        if (write(release_worker[1], &byte, 1) != 1) return 100;
    }
    for (int i = 0; i < 2; i++) {
        if (waitid(P_PID, child, &info, WEXITED | WNOWAIT) || info.si_pid != child ||
            info.si_code != CLD_EXITED || info.si_status != 73) return 101;
    }
    if (!mode) {
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 73) return 102;
    } else {
        struct rusage usage;
        unsigned char untouched[sizeof usage];
        memset(&usage, 0xa5, sizeof usage);
        memset(untouched, 0xa5, sizeof untouched);
        errno = 0;
        if (syscall(SYS_wait4, child, &status, options | 0x40, &usage) != -1 ||
            errno != EINVAL) return 108;
        errno = 0;
        long result = syscall(SYS_wait4, child, mode == 4 ? (void *)-1 : &status,
                              options, mode == 5 ? (void *)-1 : &usage);
        if (mode == 4 || mode == 5) {
            if (result != -1 || errno != EFAULT) return 109;
            if (mode == 4 && memcmp(&usage, untouched, sizeof usage)) return 110;
        } else if (result != child) {
            return 111;
        }
        if (mode != 4 && (!WIFEXITED(status) || WEXITSTATUS(status) != 73)) return 112;
    }
    errno = 0;
    if (waitpid(child, &status, WNOHANG) != -1 || errno != ECHILD) return 103;
    static const char marker[] = "wait observed last worker status exactly once\n";
    if (write(1, marker, sizeof(marker)-1) != sizeof(marker)-1) return 104;
    return 0;
}
