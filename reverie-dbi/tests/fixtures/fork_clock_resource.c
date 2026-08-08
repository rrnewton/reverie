/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/*
 * Regression fixture for native virtual-clock and virtual-resource
 * virtualization in copied (forked) DBI children.
 *
 * The DynamoRIO backend runs the Reverie Tool runtime only in the root of the
 * traced tree; copied/forked children are served entirely by the native-only
 * path. Before this fixture's fix, that path applied only identity
 * virtualization, so a forked child read real host time and real host rlimits
 * while the root process saw the deterministic native virtual clock and virtual
 * limits. This fixture reads clock_gettime through libc's vDSO path repeatedly
 * before and after two ordered children. The first child continues from fork;
 * the second execs this fixture so a new DynamoRIO client image remaps the
 * shared state and patches its newly loaded vDSO. The harness checks all
 * sixteen observations as one strictly increasing, fine-grained sequence. A
 * private COW counter, an exec-time reset, a frozen clock, or a first-read-only
 * match therefore cannot pass.
 *
 *   - Virtual CLOCK_MONOTONIC advances one microsecond per read across the
 *     complete process tree; no process restarts or owns a private clock.
 *   - Virtual RLIMIT_NOFILE is 1048576, distinct from a typical host soft limit.
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum { CLOCK_READS = 4 };

// TODO-HUMAN-REVIEW(PR-shared-dbi-clock): Review the shared-clock lifecycle probe.
static int probe(const char *who) {
  struct rlimit rl = {0, 0};
  for (int read = 0; read < CLOCK_READS; ++read) {
    struct timespec ts = {0, 0};
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0)
      return 1;
    unsigned long long nanoseconds =
        (unsigned long long)ts.tv_sec * 1000000000ULL +
        (unsigned long long)ts.tv_nsec;
    printf("%s_mono_ns[%d]=%llu\n", who, read, nanoseconds);
  }
  // Raw syscall: prlimit64(pid=0 -> current, new=NULL, old=&rl).
  if (syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, (void *)0, &rl) != 0)
    return 1;
  printf("%s_nofile=%llu\n", who, (unsigned long long)rl.rlim_cur);
  fflush(stdout);
  return 0;
}

static int wait_for_child(pid_t child) {
  int status = 0;
  if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
      WEXITSTATUS(status) != 0)
    return 1;
  return 0;
}

int main(int argc, char **argv) {
  if (argc == 2 && strcmp(argv[1], "--exec-child") == 0)
    return probe("exec-child");

  if (probe("parent-before") != 0)
    return 1;

  pid_t child = fork();
  if (child < 0)
    return 2;
  if (child == 0) {
    int result = probe("fork-child");
    // _exit so the inherited stdio buffer is not flushed twice.
    syscall(SYS_exit, result);
  }
  if (wait_for_child(child) != 0)
    return 3;

  child = fork();
  if (child < 0)
    return 4;
  if (child == 0) {
    execl("/proc/self/exe", argv[0], "--exec-child", (char *)0);
    syscall(SYS_exit, 5);
  }
  if (wait_for_child(child) != 0)
    return 6;

  return probe("parent-after");
}
