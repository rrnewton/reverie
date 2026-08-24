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
#include <sched.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static unsigned char clone_vm_stack[64 * 1024];

static int clone_vm_child(void *unused) {
  (void)unused;
  (void)getuid();
  return 0;
}

static int wait_ok(pid_t child) {
  int status = 0;
  return waitpid(child, &status, 0) == child && WIFEXITED(status) &&
         WEXITSTATUS(status) == 0;
}

static int finish_child(pid_t child) {
  if (child < 0)
    return 0;
  if (child == 0) {
    (void)syscall(SYS_getuid);
    _exit(0);
  }
  return wait_ok(child);
}

static int emit_call_marker(const char *phase, int id, const char *operation) {
  char marker[160];
  int length = snprintf(marker, sizeof(marker),
                        "reverie-dbt-test: process-clone-call phase=%s id=%d "
                        "op=%s\n",
                        phase, id, operation);
  if (length < 0 || (size_t)length >= sizeof(marker))
    return 0;
  return syscall(SYS_write, STDERR_FILENO, marker, (size_t)length) == length;
}

static int begin_call(int id, const char *operation) {
  return emit_call_marker("begin", id, operation);
}

static int end_call(int id, const char *operation) {
  return emit_call_marker("end", id, operation);
}

int main(void) {
  if (!begin_call(1, "invalid-clone-1"))
    return 11;
  errno = 0;
  long invalid = syscall(SYS_clone, CLONE_SIGHAND | SIGCHLD, 0, 0, 0, 0);
  if (invalid != -1 || errno != EINVAL)
    return 1;
  if (!end_call(1, "invalid-clone-1"))
    return 21;

#ifdef SYS_clone3
  if (!begin_call(2, "malformed-clone3"))
    return 12;
  errno = 0;
  long malformed_clone3 = syscall(SYS_clone3, NULL, 0);
  if (malformed_clone3 != -1)
    return 8;
  if (!end_call(2, "malformed-clone3"))
    return 22;
#else
  return 9;
#endif

  if (!begin_call(3, "invalid-clone-2"))
    return 13;
  errno = 0;
  long invalid_again = syscall(SYS_clone, CLONE_SIGHAND | SIGCHLD, 0, 0, 0, 0);
  if (invalid_again != -1 || errno != EINVAL)
    return 7;
  if (!end_call(3, "invalid-clone-2"))
    return 23;

  if (!begin_call(4, "fork"))
    return 14;
  if (!finish_child((pid_t)syscall(SYS_fork)))
    return 2;
  if (!end_call(4, "fork"))
    return 24;

  if (!begin_call(5, "clone"))
    return 15;
  if (!finish_child((pid_t)syscall(SYS_clone, SIGCHLD, 0, 0, 0, 0)))
    return 3;
  if (!end_call(5, "clone"))
    return 25;

  if (!begin_call(6, "clone-vm"))
    return 16;
  pid_t clone_vm_child_pid =
      clone(clone_vm_child, clone_vm_stack + sizeof(clone_vm_stack),
            CLONE_VM | SIGCHLD, NULL);
  if (clone_vm_child_pid < 0 || !wait_ok(clone_vm_child_pid))
    return 6;
  if (!end_call(6, "clone-vm"))
    return 26;

#ifdef SYS_clone3
  if (!begin_call(7, "clone3"))
    return 17;
  struct clone_args args = {.exit_signal = SIGCHLD};
  if (!finish_child((pid_t)syscall(SYS_clone3, &args, sizeof(args))))
    return 4;
  if (!end_call(7, "clone3"))
    return 27;
#endif

  if (!begin_call(8, "vfork"))
    return 18;
  if (!finish_child((pid_t)syscall(SYS_vfork)))
    return 5;
  if (!end_call(8, "vfork"))
    return 28;

  puts("process-clone-results-ok");
  return 0;
}
