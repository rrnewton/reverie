/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Each mode except "vfork-external" ends one process image with a SIGKILL that
// it sends to itself. DynamoRIO runs no exit callback for SIGKILL, so the
// client must end that image's protected evidence stream before the kill is
// delivered. In "vfork-external", a thread of the parent kills the vfork child
// instead. A vfork or CLONE_VFORK child that dies this way also leaves its
// DynamoRIO thread record behind in the address space it shared with the
// parent, which must not stop the parent's own process exit from running.

#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static _Alignas(16) char clone_stack[64 * 1024];
static _Atomic pid_t vfork_child;

static void kill_current_thread(void) {
  (void)syscall(SYS_tgkill, syscall(SYS_getpid), syscall(SYS_gettid), SIGKILL);
}

static int clone_vfork_child(void* argument) {
  (void)argument;
  kill_current_thread();
  return 9;
}

static void* kill_vfork_child(void* argument) {
  (void)argument;
  pid_t child;
  while ((child = atomic_load(&vfork_child)) == 0)
    sched_yield();
  (void)kill(child, SIGKILL);
  return NULL;
}

static int killed_by_sigkill(pid_t child) {
  int status = 0;
  pid_t waited;
  do {
    waited = waitpid(child, &status, 0);
  } while (waited < 0 && errno == EINTR);
  return waited == child && WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL;
}

int main(int argc, char** argv) {
  if (argc != 2)
    return 2;
  const char* mode = argv[1];
  if (strcmp(mode, "root") == 0) {
    raise(SIGKILL);
    return 3;
  }

  pthread_t killer;
  int external = strcmp(mode, "vfork-external") == 0;
  if (external && pthread_create(&killer, NULL, kill_vfork_child, NULL) != 0)
    return 10;
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
  } else if (strcmp(mode, "clone-vfork") == 0) {
    child = clone(
        clone_vfork_child,
        clone_stack + sizeof(clone_stack),
        CLONE_VM | CLONE_VFORK | SIGCHLD,
        NULL);
  } else if (external) {
    child = vfork();
    if (child == 0) {
      atomic_store(&vfork_child, (pid_t)syscall(SYS_getpid));
      for (;;)
        (void)syscall(SYS_ppoll, NULL, 0, NULL, NULL);
    }
  } else {
    return 6;
  }
  if (child < 0) {
    perror("self-SIGKILL child");
    return 7;
  }
  if (external && pthread_join(killer, NULL) != 0)
    return 11;
  if (!killed_by_sigkill(child))
    return 8;
  printf("self-sigkill-%s-ok\n", mode);
  return 0;
}
