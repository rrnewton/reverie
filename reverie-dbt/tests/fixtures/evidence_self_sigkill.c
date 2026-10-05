/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// The modes "root", "fork" and "vfork" each end one process image with a
// SIGKILL that it sends to itself. DynamoRIO runs no exit callback for SIGKILL,
// so the client must end that image's protected evidence stream before the
// kill is delivered. In "root-returned", a seccomp filter makes the root's
// self-directed SIGKILL return an error, which leaves the root running after
// its stream has ended. In "zombie-child", the root sends SIGKILL to a child
// that has exited but is not yet reaped; that kill succeeds without being
// self-directed, and the root must run on normally.

#define _GNU_SOURCE
#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static void kill_current_thread(void) {
  (void)syscall(SYS_tgkill, syscall(SYS_getpid), syscall(SYS_gettid), SIGKILL);
}

static int killed_by_sigkill(pid_t child) {
  int status = 0;
  pid_t waited;
  do {
    waited = waitpid(child, &status, 0);
  } while (waited < 0 && errno == EINTR);
  return waited == child && WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL;
}

// Makes kill, tkill and tgkill fail with EPERM whenever they send SIGKILL.
static int refuse_sigkill_syscalls(void) {
  struct sock_filter filter[] = {
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 0, 8),
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_kill, 2, 0),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_tkill, 1, 0),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_tgkill, 2, 4),
      BPF_STMT(
          BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1])),
      BPF_STMT(BPF_JMP | BPF_JA, 1),
      BPF_STMT(
          BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[2])),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SIGKILL, 1, 0),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
  };
  struct sock_fprog program = {
      .len = (unsigned short)(sizeof(filter) / sizeof(filter[0])),
      .filter = filter,
  };
  if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
    return -1;
  return prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program, 0, 0);
}

static int kill_zombie_child(void) {
  siginfo_t info;
  int status = 0;
  pid_t child = fork();
  if (child < 0) {
    perror("zombie child");
    return 7;
  }
  if (child == 0)
    _exit(0);
  // WNOWAIT leaves the exited child unreaped, so its pid cannot be reused
  // while the root signals it.
  memset(&info, 0, sizeof(info));
  if (waitid(P_PID, (id_t)child, &info, WEXITED | WNOWAIT) != 0)
    return 13;
  if (kill(child, SIGKILL) != 0)
    return 14;
  if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
      WEXITSTATUS(status) != 0)
    return 15;
  return 0;
}

int main(int argc, char** argv) {
  if (argc != 2)
    return 2;
  const char* mode = argv[1];
  if (strcmp(mode, "root") == 0) {
    raise(SIGKILL);
    return 3;
  }
  if (strcmp(mode, "root-returned") == 0) {
    if (refuse_sigkill_syscalls() != 0) {
      perror("seccomp filter");
      return 16;
    }
    raise(SIGKILL);
    return 12;
  }
  if (strcmp(mode, "zombie-child") == 0) {
    int result = kill_zombie_child();
    if (result == 0)
      printf("self-sigkill-%s-ok\n", mode);
    return result;
  }

  pid_t child;
  if (strcmp(mode, "fork") == 0) {
    child = fork();
    if (child == 0) {
      (void)kill(getpid(), SIGKILL);
      _exit(4);
    }
  } else if (strcmp(mode, "vfork") == 0) {
    child = vfork();
    if (child == 0) {
      kill_current_thread();
      _exit(5);
    }
  } else {
    return 6;
  }
  if (child < 0) {
    perror("self-SIGKILL child");
    return 7;
  }
  if (!killed_by_sigkill(child))
    return 8;
  printf("self-sigkill-%s-ok\n", mode);
  return 0;
}
