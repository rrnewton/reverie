/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Each mode ends one process image with a SIGKILL that it sends to itself.
// DynamoRIO runs no exit callback for SIGKILL, so the client must end that
// image's protected evidence stream before the kill is delivered.

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
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

int main(int argc, char** argv) {
  if (argc != 2)
    return 2;
  const char* mode = argv[1];
  if (strcmp(mode, "root") == 0) {
    raise(SIGKILL);
    return 3;
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
