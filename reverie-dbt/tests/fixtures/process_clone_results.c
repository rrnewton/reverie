/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <linux/sched.h>
#include <signal.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int wait_ok(pid_t child) {
  int status = 0;
  return waitpid(child, &status, 0) == child && WIFEXITED(status) &&
         WEXITSTATUS(status) == 0;
}

static int finish_child(pid_t child) {
  if (child < 0)
    return 0;
  if (child == 0)
    _exit(0);
  return wait_ok(child);
}

int main(void) {
  errno = 0;
  long invalid = syscall(SYS_clone, CLONE_SIGHAND | SIGCHLD, 0, 0, 0, 0);
  if (invalid != -1 || errno != EINVAL)
    return 1;

  if (!finish_child((pid_t)syscall(SYS_fork)))
    return 2;
  if (!finish_child((pid_t)syscall(SYS_clone, SIGCHLD, 0, 0, 0, 0)))
    return 3;

#ifdef SYS_clone3
  struct clone_args args = {.exit_signal = SIGCHLD};
  if (!finish_child((pid_t)syscall(SYS_clone3, &args, sizeof(args))))
    return 4;
#endif

  if (!finish_child((pid_t)syscall(SYS_vfork)))
    return 5;

  puts("process-clone-results-ok");
  return 0;
}
