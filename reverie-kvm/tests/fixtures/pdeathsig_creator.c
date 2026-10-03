/* Actual creator THREAD death; its process leader survives and joins it.
 * Enrollment permits regular-file handshakes, not pipe I/O. No sleeps or
 * ready-before-pause claim: controlled parked modes require the Tool's
 * actual original-syscall callback witness before creator SYS_exit.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int record_fd;
static int mode;
static pid_t parent_pid;
static uid_t parent_uid;
static pid_t child_pid;
static volatile sig_atomic_t calls;
static volatile sig_atomic_t valid_info;

static void fail(int code) { _exit(code); }
static void store(off_t offset, char value) {
  if (pwrite(record_fd, &value, 1, offset) != 1) fail(30);
}
static void await(off_t offset) {
  char value;
  do {
    if (pread(record_fd, &value, 1, offset) != 1) fail(31);
    if (!value) syscall(SYS_sched_yield);
  } while (!value);
}
static void caught(int signal, siginfo_t *info, void *context) {
  (void)context;
  ++calls;
  valid_info = signal == SIGUSR1 && info->si_signo == SIGUSR1 &&
      info->si_errno == 0 && info->si_code == SI_USER &&
      info->si_pid == parent_pid && info->si_uid == parent_uid;
}
static void recipient(void) {
  struct sigaction action = {0};
  action.sa_sigaction = caught;
  action.sa_flags = SA_SIGINFO;
  sigemptyset(&action.sa_mask);
  if (sigaction(SIGUSR1, &action, 0)) fail(32);
  sigset_t one;
  sigemptyset(&one);
  sigaddset(&one, SIGUSR1);
  if (mode <= 1 && sigprocmask(SIG_BLOCK, &one, 0)) fail(33);
  if (prctl(PR_SET_PDEATHSIG, SIGUSR1, 0, 0, 0)) fail(34);
  int setting = -1;
  if (prctl(PR_GET_PDEATHSIG, &setting, 0, 0, 0) || setting != SIGUSR1) fail(35);
  if (mode == 1 && prctl(PR_SET_PDEATHSIG, 0, 0, 0, 0)) fail(36);
  store(0, 1);

  if (mode <= 1) {
    /* The surviving leader writes this only AFTER pthread_join(creator).
     * A dropped publication therefore fails sigpending, not a timeout. */
    await(1);
    sigset_t pending;
    if (sigpending(&pending) || calls != 0 ||
        sigismember(&pending, SIGUSR1) != (mode == 0)) fail(37);
    if (sigprocmask(SIG_UNBLOCK, &one, 0)) fail(38);
    if (calls != (mode == 0) || valid_info != (mode == 0)) fail(39);
  } else {
    /* Only run these modes with the observing Tool, which keeps the creator
     * alive until this exact pause/nanosleep callback is Pending. */
    errno = 0;
    long result;
    if (mode == 2) {
      result = syscall(SYS_pause);
    } else {
      const struct timespec requested = {60, 0};
      result = syscall(SYS_nanosleep, &requested, 0);
    }
    if (result != -1 || errno != EINTR || calls != 1 || valid_info != 1) fail(40);
  }
  if (getppid() != parent_pid) fail(41);
  store(2, 1);
  _exit(0);
}
static void *creator(void *unused) {
  (void)unused;
  child_pid = fork();
  if (child_pid < 0) fail(20);
  if (child_pid == 0) recipient();
  await(0);
  /* pthread return reaches real SYS_exit of this nonleader, not exit_group. */
  return 0;
}
int main(int argc, char **argv) {
  if (argc != 2) return 10;
  mode = atoi(argv[1]);
  if (mode < 0 || mode > 3) return 11;
  parent_pid = getpid(); parent_uid = getuid();
  record_fd = open("creator.record", O_CREAT | O_TRUNC | O_RDWR, 0600);
  if (record_fd < 0 || ftruncate(record_fd, 3)) return 12;
  pthread_t worker;
  if (pthread_create(&worker, 0, creator, 0)) return 13;
  if (pthread_join(worker, 0)) return 14;
  store(1, 1);
  int status = 0;
  if (waitpid(child_pid, &status, 0) != child_pid ||
      !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
    fprintf(stderr, "recipient status=%d\n", status);
    return 15;
  }
  char completed = 0;
  if (pread(record_fd, &completed, 1, 2) != 1 || completed != 1) return 16;
  if (close(record_fd)) return 17;
  printf("pdeathsig creator-thread mode=%d parent-alive=1 child-completed=1\n", mode);
  return 0;
}
