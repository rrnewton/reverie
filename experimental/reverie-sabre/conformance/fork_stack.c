/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#include <sys/wait.h>
#include <unistd.h>

extern long framed_fork(void);

int main(void) {
  long child = framed_fork();
  if (child == 0) {
    _exit(0);
  }
  if (child < 0) {
    return 1;
  }

  int status = 0;
  if (waitpid((pid_t)child, &status, 0) != child) {
    return 2;
  }
  if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
    return 3;
  }
  return 0;
}
