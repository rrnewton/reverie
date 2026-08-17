/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#define _POSIX_C_SOURCE 200809L

#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
  pid_t child = fork();
  int status = 0;
  if (child < 0)
    return 2;
  if (child == 0) {
    for (;;)
      pause();
  }
  if (kill(child, SIGKILL) != 0 || waitpid(child, &status, 0) != child)
    return 3;
  if (!WIFSIGNALED(status) || WTERMSIG(status) != SIGKILL)
    return 4;
  puts("sigkill-wait-ok");
  return 0;
}
