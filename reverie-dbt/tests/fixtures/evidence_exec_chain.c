/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#define _GNU_SOURCE

#include <errno.h>
#include <stdio.h>
#include <unistd.h>

extern char **environ;

int main(void) {
  char *const missing_argv[] = {(char *)"missing-evidence-exec", NULL};
  char *const echo_argv[] = {(char *)"echo", (char *)"exec-chain-ok", NULL};

  execve("/definitely/missing-evidence-exec", missing_argv, environ);
  if (errno != ENOENT) {
    perror("failed exec did not return ENOENT");
    return 2;
  }

  execve("/bin/echo", echo_argv, environ);
  perror("exec /bin/echo");
  return 3;
}
