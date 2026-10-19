/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/* A process forks while another of its threads waits in select or pselect6 on
 * an empty pipe (https://github.com/rrnewton/hermit/issues/4483).
 * Under the inject-waits test Tool the wait runs
 * inside DynamoRIO, so the fork's synch cannot suspend that thread, and every
 * synch round interrupts the wait with DynamoRIO's suspend signal.
 *
 * Usage: fork_with_waiting_thread MODE
 *
 * Waiter modes (the waiter thread waits; the first thread forks):
 *   select     select, no timeout, data arrives after the fork
 *   pselect    pselect6 with an empty temporary mask
 *   blocked    SIGUSR2 blocked and pending before the call
 *   arrive     SIGUSR1 blocked; a sender tgkills it to the waiter mid-wait
 *   blockall   the waiter blocks every signal
 *   itimer     blockall, run with a client itimer armed
 *   signal     SIGUSR1 unblocked in the waiter; a sender tgkills it mid-wait
 *   sigchld    SIGCHLD at its default action; a sender tgkills it mid-wait
 *   timeout    select with a 2 s timeout; data arrives only after it expires
 *   ptimeout   the same with pselect6
 *   sticky     timeout, under the STICKY_TIMEOUTS personality
 *   psticky    ptimeout, under the STICKY_TIMEOUTS personality
 *   longtv     select with a LONG_MAX-second timeout under STICKY_TIMEOUTS;
 *              data arrives after the fork
 * Fork shapes (a waiter as in select):
 *   three      the first thread joins worker A, A forks, the waiter is B
 *   nested     the fork child forks again from a new thread while its first
 *              thread waits in read (a native select there would see a
 *              separate, pre-existing EINTR from that fork's synch)
 *   handler    the first thread forks from a SIGUSR1 handler
 *   clone      clone without CLONE_VM instead of fork
 *   clone3     clone3 without CLONE_VM instead of fork
 * DynamoRIO cache reset:
 *   reset      the waiter waits, then a third thread is created
 * Exit:
 *   exitpark   the first thread calls exit while the waiter still waits
 */
#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <linux/sched.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/personality.h>
#include <sys/select.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define SENDER_DELAY_SECONDS 3

static const char* mode;
static int data_pipe[2];
static int release_pipe[2];
static int tid_pipe[2];
static int ready_pipe[2];
static pid_t sender = -1;
static volatile sig_atomic_t handled;
static volatile sig_atomic_t handler_fork_child = -1;

static int is(const char* name) {
  return strcmp(mode, name) == 0;
}

static void on_usr1(int signal) {
  (void)signal;
  handled++;
}

static void die(const char* what) {
  perror(what);
  exit(90);
}

static double monotonic_seconds(void) {
  struct timespec now;
  clock_gettime(CLOCK_MONOTONIC, &now);
  return (double)now.tv_sec + (double)now.tv_nsec / 1e9;
}

/* Forked before any second thread exists, so its own fork needs no synch. It
 * waits for the waiter's tid, then signals that thread mid-wait. */
static void start_sender(int signal) {
  if (pipe(tid_pipe) != 0)
    die("pipe");
  pid_t parent = getpid();
  sender = fork();
  if (sender < 0)
    die("fork sender");
  if (sender == 0) {
    pid_t tid;
    if (read(tid_pipe[0], &tid, sizeof(tid)) != (ssize_t)sizeof(tid))
      _exit(91);
    sleep(SENDER_DELAY_SECONDS);
    _exit(syscall(SYS_tgkill, parent, tid, signal) == 0 ? 0 : 92);
  }
}

static int sender_signal(void) {
  if (is("arrive") || is("signal"))
    return SIGUSR1;
  if (is("sigchld"))
    return SIGCHLD;
  return 0;
}

static int is_timed(void) {
  return strstr(mode, "timeout") != NULL || strstr(mode, "sticky") != NULL;
}

static long wait_on_pipe(struct timeval* tv, struct timespec* ts) {
  fd_set fds;
  FD_ZERO(&fds);
  FD_SET(data_pipe[0], &fds);
  if (is("pselect") || is("ptimeout") || is("psticky")) {
    sigset_t empty;
    sigemptyset(&empty);
    struct {
      const sigset_t* set;
      size_t size;
    } mask = {&empty, 8};
    return syscall(SYS_pselect6, data_pipe[0] + 1, &fds, NULL, NULL, ts, &mask);
  }
  return syscall(SYS_select, data_pipe[0] + 1, &fds, NULL, NULL, tv);
}

static void* waiter(void* argument) {
  (void)argument;
  sigset_t set;
  if (is("signal")) {
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    pthread_sigmask(SIG_UNBLOCK, &set, NULL);
  } else if (is("blocked")) {
    sigemptyset(&set);
    sigaddset(&set, SIGUSR2);
    pthread_sigmask(SIG_BLOCK, &set, NULL);
    syscall(SYS_tgkill, getpid(), syscall(SYS_gettid), SIGUSR2);
  } else if (is("blockall") || is("itimer")) {
    sigfillset(&set);
    pthread_sigmask(SIG_SETMASK, &set, NULL);
  }
  if (is("sticky") || is("psticky") || is("longtv")) {
    int persona = personality(0xffffffff);
    if (persona == -1 || personality(persona | STICKY_TIMEOUTS) == -1)
      die("personality");
  }
  struct timeval tv = {2, 0};
  struct timespec ts = {2, 0};
  int timed = is_timed();
  if (write(ready_pipe[1], "r", 1) != 1)
    die("write ready");
  if (sender >= 0) {
    pid_t tid = (pid_t)syscall(SYS_gettid);
    if (write(tid_pipe[1], &tid, sizeof(tid)) != (ssize_t)sizeof(tid))
      die("write tid");
  }
  double start = monotonic_seconds();
  struct timeval longest = {LONG_MAX, 0};
  long result = is("longtv")
      ? wait_on_pipe(&longest, NULL)
      : wait_on_pipe(timed ? &tv : NULL, timed ? &ts : NULL);
  int error = errno;
  if (is("exitpark")) {
    /* Natively exit_group ends this thread inside the wait. If DBT returns
     * from it first, stay silent until the exit reaches this thread. */
    for (;;)
      pause();
  }
  double elapsed = monotonic_seconds() - start;
  sigset_t pending;
  sigpending(&pending);
  printf(
      "waiter mode=%s r=%ld%s%s handled=%d usr1_pending=%d usr2_pending=%d",
      mode,
      result,
      result < 0 ? " errno=" : "",
      result < 0 ? strerror(error) : "",
      (int)handled,
      sigismember(&pending, SIGUSR1),
      sigismember(&pending, SIGUSR2));
  if (timed) {
    int pselect = mode[0] == 'p';
    printf(
        " timeout=%ld.%06ld within_5s=%d",
        pselect ? (long)ts.tv_sec : (long)tv.tv_sec,
        pselect ? ts.tv_nsec / 1000 : (long)tv.tv_usec,
        elapsed < 5.0);
  }
  printf("\n");
  return NULL;
}

static void child_body(void) {
  char byte;
  /* Without the write end, the child sees EOF and exits if the parent dies,
   * instead of holding the test's output pipes open forever. */
  close(release_pipe[1]);
  if (read(release_pipe[0], &byte, 1) != 1)
    _exit(93);
  _exit(3);
}

static pid_t fork_child(void) {
  pid_t child;
  if (is("clone")) {
    child = (pid_t)syscall(SYS_clone, SIGCHLD, 0, NULL, NULL, 0);
  } else if (is("clone3")) {
    struct clone_args args;
    memset(&args, 0, sizeof(args));
    args.exit_signal = SIGCHLD;
    child = (pid_t)syscall(SYS_clone3, &args, sizeof(args));
  } else {
    child = fork();
  }
  if (child == 0)
    child_body();
  return child;
}

static void on_usr1_fork(int signal) {
  (void)signal;
  handler_fork_child = fork_child();
}

/* In the fork child, where the Tool does not run, a new thread forks while
 * the first thread waits. */
static void* nested_forker(void* argument) {
  (void)argument;
  usleep(200000);
  pid_t grandchild = fork();
  if (grandchild == 0)
    _exit(4);
  int status = 0;
  waitpid(grandchild, &status, 0);
  if (write(data_pipe[1], "x", 1) != 1)
    _exit(94);
  return (void*)(intptr_t)(WIFEXITED(status) ? WEXITSTATUS(status) : -1);
}

static void nested_child(void) {
  pthread_t thread;
  if (pipe(data_pipe) != 0 || pthread_create(&thread, NULL, nested_forker, NULL) != 0)
    _exit(95);
  char byte;
  long result = read(data_pipe[0], &byte, 1);
  void* grandchild_code;
  pthread_join(thread, &grandchild_code);
  _exit(result == 1 && (intptr_t)grandchild_code == 4 ? 3 : 96);
}

static void* worker_a(void* argument) {
  (void)argument;
  usleep(200000);
  return (void*)(intptr_t)fork_child();
}

static void* idle_thread(void* argument) {
  (void)argument;
  return NULL;
}

int main(int argc, char** argv) {
  setvbuf(stdout, NULL, _IONBF, 0);
  mode = argc > 1 ? argv[1] : "select";
  signal(SIGUSR1, is("handler") ? on_usr1_fork : on_usr1);
  sigset_t set;
  sigemptyset(&set);
  sigaddset(&set, SIGUSR1);
  if (!is("handler"))
    pthread_sigmask(SIG_BLOCK, &set, NULL);
  if (pipe(data_pipe) != 0 || pipe(release_pipe) != 0 || pipe(ready_pipe) != 0)
    die("pipe");
  if (sender_signal() != 0)
    start_sender(sender_signal());

  pthread_t wait_thread;
  if (pthread_create(&wait_thread, NULL, waiter, NULL) != 0)
    die("pthread_create");
  char ready;
  if (read(ready_pipe[0], &ready, 1) != 1)
    die("read ready");
  /* Let the waiter enter its wait. */
  usleep(200000);

  pid_t child = 0;
  if (is("exitpark")) {
    printf("exiting with the waiter parked\n");
    exit(0);
  }
  if (is("reset")) {
    /* With -reset_at_nth_thread 3 this creation resets the code cache, whose
     * synch gives up on the waiting thread and takes the abort path. */
    pthread_t third;
    if (pthread_create(&third, NULL, idle_thread, NULL) != 0)
      die("pthread_create third");
    pthread_join(third, NULL);
  } else if (is("three")) {
    pthread_t a;
    void* result;
    if (pthread_create(&a, NULL, worker_a, NULL) != 0)
      die("pthread_create A");
    pthread_join(a, &result);
    child = (pid_t)(intptr_t)result;
  } else if (is("handler")) {
    raise(SIGUSR1);
    child = handler_fork_child;
  } else if (is("nested")) {
    child = fork();
    if (child == 0)
      nested_child();
  } else {
    child = fork_child();
  }
  if (child < 0)
    die("fork");

  if (sender >= 0) {
    int status = 0;
    waitpid(sender, &status, 0);
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
      fprintf(stderr, "sender failed: %#x\n", status);
      return 97;
    }
  }
  /* A timed wait must expire first; the others get their data now. */
  if (is_timed())
    pthread_join(wait_thread, NULL);
  if (write(data_pipe[1], "x", 1) != 1)
    die("write data");
  if (!is_timed())
    pthread_join(wait_thread, NULL);
  if (child == 0)
    return 0;
  if (write(release_pipe[1], "x", 1) != 1)
    die("write release");
  int status = 0;
  waitpid(child, &status, 0);
  printf("child code=%d\n", WIFEXITED(status) ? WEXITSTATUS(status) : -1);
  return 0;
}
