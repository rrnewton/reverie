/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/*
 * Exits for the child-exit publication test: a thread of the root that ends
 * while its process goes on, a child process that starts and joins a thread
 * of its own and then exits with status 7. The root waits for the child,
 * checks its status, and ends with exit_group(0). Prints "ok" when every
 * call returned what Linux returns.
 */

#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static void *thread_body(void *arg) {
  (void)arg;
  return NULL;
}

static int run_and_join_a_thread(void) {
  pthread_t thread;
  if (pthread_create(&thread, NULL, thread_body, NULL) != 0) {
    return -1;
  }
  return pthread_join(thread, NULL) == 0 ? 0 : -1;
}

int main(void) {
  if (run_and_join_a_thread() != 0) {
    puts("root thread failed");
    return 1;
  }
  pid_t exited = fork();
  if (exited < 0) {
    puts("fork failed");
    return 1;
  }
  if (exited == 0) {
    _exit(run_and_join_a_thread() == 0 ? 7 : 1);
  }
  int status = 0;
  if (waitpid(exited, &status, 0) != exited || !WIFEXITED(status) ||
      WEXITSTATUS(status) != 7) {
    printf("exited child status %#x\n", status);
    return 1;
  }
  puts("ok");
  fflush(stdout);
  syscall(SYS_exit_group, 0);
  return 0;
}
