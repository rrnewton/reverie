/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

enum { VIRTUAL_IDENTITY_FD = 197 };

#ifndef P_PIDFD
#define P_PIDFD ((idtype_t)3)
#endif

static int pidfd_targets_virtual_child(void) {
  int gate[2];
  if (pipe(gate) != 0)
    return 1;

  pid_t child = fork();
  if (child < 0)
    return 1;
  if (child == 0) {
    close(gate[1]);
    char byte;
    if (read(gate[0], &byte, 1) != 1)
      _exit(1);
    _exit(42);
  }

  close(gate[0]);
  int fd = (int)syscall(SYS_pidfd_open, child, O_NONBLOCK);
  siginfo_t info = {0};
  errno = 0;
  int wait_result = fd < 0 ? 0 : waitid(P_PIDFD, (id_t)fd, &info, WEXITED);
  int wait_errno = errno;

  char byte = 1;
  int write_result = write(gate[1], &byte, 1);
  close(gate[1]);
  int status = 0;
  pid_t reaped = waitpid(child, &status, 0);
  if (fd >= 0)
    close(fd);

  return fd >= 0 && wait_result == -1 && wait_errno == EAGAIN &&
         write_result == 1 && reaped == child && WIFEXITED(status) &&
         WEXITSTATUS(status) == 42
      ? 0
      : 1;
}

// TODO-HUMAN-REVIEW(PR-154): Review deferred DBT identity and private-fd policy.
int main(void) {
  long pid = syscall(SYS_getpid);
  long ppid = syscall(SYS_getppid);
  long tid = syscall(SYS_gettid);

  if (syscall(SYS_close, VIRTUAL_IDENTITY_FD) != 0)
    return 1;
  errno = 0;
  if (fcntl(VIRTUAL_IDENTITY_FD, F_GETFD) < 0)
    return 2;
  if (pid != 3 || ppid != 1 || tid != 3)
    return 3;
  if (pidfd_targets_virtual_child() != 0)
    return 4;

  printf("pid=%ld ppid=%ld tid=%ld identity_fd=open\n", pid, ppid, tid);
  return 0;
}
