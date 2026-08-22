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
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

enum {
  VIRTUAL_IDENTITY_FD = 197,
  TEST_GUEST_ID_SENTINEL = INT32_MAX - 1,
  TEST_GUEST_ID_RESULT = INT32_MAX - 2,
};

static int test_guest_id_targets_enabled(void) {
  const char *value = getenv("HERMIT_DBT_TEST_GUEST_ID_TARGETS");
  return value != NULL && value[0] != '\0' && strcmp(value, "0") != 0 &&
         strcmp(value, "false") != 0;
}

#ifndef P_PIDFD
#define P_PIDFD ((idtype_t)3)
#endif

static int pidfd_targets_virtual_child(void) {
  _Atomic int *gate = mmap(NULL, sizeof(*gate), PROT_READ | PROT_WRITE,
                           MAP_SHARED | MAP_ANONYMOUS, -1, 0);
  if (gate == MAP_FAILED)
    return 1;
  atomic_init(gate, 0);

  pid_t child = fork();
  if (child < 0) {
    munmap(gate, sizeof(*gate));
    return 1;
  }
  if (child == 0) {
    while (atomic_load_explicit(gate, memory_order_acquire) == 0)
      syscall(SYS_sched_yield);
    _exit(42);
  }

  int fd = (int)syscall(SYS_pidfd_open, child, O_NONBLOCK);
  siginfo_t info = {0};
  errno = 0;
  int wait_result = fd < 0 ? 0 : waitid(P_PIDFD, (id_t)fd, &info, WEXITED);
  int wait_errno = errno;

  atomic_store_explicit(gate, 1, memory_order_release);
  int status = 0;
  pid_t reaped = waitpid(child, &status, 0);
  if (fd >= 0)
    close(fd);
  munmap(gate, sizeof(*gate));

  return fd >= 0 && wait_result == -1 && wait_errno == EAGAIN &&
                 reaped == child && WIFEXITED(status) &&
                 WEXITSTATUS(status) == 42
             ? 0
             : 1;
}

static int pidfd_targets_tool_guest_identity(void) {
  if (!test_guest_id_targets_enabled())
    return 0;
  int fd = (int)syscall(SYS_pidfd_open, TEST_GUEST_ID_SENTINEL, 0);
  if (fd < 0)
    return 1;
  return close(fd) == 0 ? 0 : 1;
}

static int tool_identity_result_remains_guest_visible(void) {
  if (!test_guest_id_targets_enabled())
    return 0;
  if (syscall(SYS_wait4, TEST_GUEST_ID_SENTINEL, NULL, WNOHANG, NULL) != 0)
    return 1;
  long result = syscall(SYS_getpid);
  return result == TEST_GUEST_ID_RESULT ? 0 : 1;
}

static int pidfd_preserves_identity_and_flag_validation_order(void) {
  errno = 0;
  long fd = syscall(SYS_pidfd_open, INT32_MAX, 0);
  if (fd != -1 || errno != ESRCH)
    return 1;

  errno = 0;
  fd = syscall(SYS_pidfd_open, INT32_MAX, UINT32_C(0x40000000));
  return fd == -1 && errno == EINVAL ? 0 : 1;
}

static int queued_signals_target_virtual_self(void) {
  long pid = syscall(SYS_getpid);
  long tid = syscall(SYS_gettid);
  siginfo_t info = {0};
  long target_pid = test_guest_id_targets_enabled()
                        ? TEST_GUEST_ID_SENTINEL
                        : pid;
  long target_tid = test_guest_id_targets_enabled()
                        ? TEST_GUEST_ID_SENTINEL
                        : tid;
  info.si_code = SI_QUEUE;
  info.si_pid = pid;
  info.si_uid = getuid();

  /* Signal 0 checks the translated target without delivering a signal. In
   * test mode the impossible guest sentinel must be rewritten by the Tool and
   * then translated by the native boundary before the kernel can accept it. */
  if (syscall(SYS_rt_sigqueueinfo, target_pid, 0, &info) != 0)
    return 1;
  return syscall(SYS_rt_tgsigqueueinfo, target_pid, target_tid, 0, &info) == 0
             ? 0
             : 1;
}

static int queued_signals_preserve_validation_order(void) {
  siginfo_t info = {0};
  info.si_code = SI_QUEUE;
  info.si_pid = syscall(SYS_getpid);
  info.si_uid = getuid();

  errno = 0;
  if (syscall(SYS_rt_sigqueueinfo, INT32_MAX, 0, &info) != -1 ||
      errno != ESRCH)
    return 1;
  errno = 0;
  if (syscall(SYS_rt_tgsigqueueinfo, INT32_MAX, INT32_MAX, 0, &info) != -1 ||
      errno != ESRCH)
    return 1;
  errno = 0;
  if (syscall(SYS_rt_sigqueueinfo, INT32_MAX, 0, (void *)1) != -1 ||
      errno != EFAULT)
    return 1;
  errno = 0;
  if (syscall(SYS_rt_tgsigqueueinfo, INT32_MAX, INT32_MAX, 0, (void *)1) !=
          -1 ||
      errno != EFAULT)
    return 1;
  return 0;
}

// TODO-HUMAN-REVIEW(PR-154): Review deferred DBT identity and private-fd
// policy.
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
  if (pidfd_targets_tool_guest_identity() != 0)
    return 5;
  if (pidfd_preserves_identity_and_flag_validation_order() != 0)
    return 6;
  if (queued_signals_target_virtual_self() != 0)
    return 7;
  if (queued_signals_preserve_validation_order() != 0)
    return 8;
  if (tool_identity_result_remains_guest_visible() != 0)
    return 9;

  const char *result_check =
      test_guest_id_targets_enabled() ? " result_identity=ok" : "";
  printf("pid=%ld ppid=%ld tid=%ld identity_fd=open pidfd=ok queued_signal=ok%s\n",
         pid, ppid, tid, result_check);
  return 0;
}
