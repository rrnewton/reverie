/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#define _GNU_SOURCE

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_getpid
#define SYS_getpid 461
#endif

int main(void) {
  uint32_t size = 0;

  errno = 0;
  long result = syscall(SYS_getpid, NULL, &size, 0U);
  if (result != -1 || errno != ENOSYS) {
    fprintf(stderr,
            "lsm_list_modules returned %ld with errno %d (%s), expected "
            "ENOSYS\n",
            result, errno, strerror(errno));
    return 1;
  }
  puts("lsm_list_modules deterministically unavailable");
  return 0;
}
