/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/* Asynchronous signal mode on a pipe this program makes, on /dev/null, on
 * /dev/urandom and on the standard input it inherits (a pipe from the test).
 * Each row's result is written to stdout as one 8-byte word: the call's
 * value, or minus its errno. Every row that turns the mode on is turned off
 * again; no owner is ever set, so no signal can be sent.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sys/ioctl.h>
#include <unistd.h>

static long result(long value) {
  return value < 0 ? -(long)errno : value;
}

static long fioasync(int fd, int on) {
  return result(ioctl(fd, FIOASYNC, &on));
}

static long setfl(int fd, int flags) {
  return result(fcntl(fd, F_SETFL, flags));
}

static long async_bit(int fd) {
  int flags = fcntl(fd, F_GETFL);
  return flags < 0 ? -(long)errno : (flags & O_ASYNC) != 0;
}

int main(void) {
  long rows[13];
  int n = 0;
  int fds[2];
  if (pipe(fds) != 0)
    return 90;
  rows[n++] = fioasync(fds[0], 1);
  rows[n++] = async_bit(fds[0]);
  rows[n++] = fioasync(fds[0], 0);
  int flags = fcntl(fds[0], F_GETFL);
  rows[n++] = setfl(fds[0], flags | O_ASYNC);
  rows[n++] = async_bit(fds[0]);
  setfl(fds[0], flags);

  int null = open("/dev/null", O_RDONLY | O_CLOEXEC);
  if (null < 0)
    return 91;
  rows[n++] = fioasync(null, 1);
  flags = fcntl(null, F_GETFL);
  rows[n++] = setfl(null, flags | O_ASYNC);
  rows[n++] = async_bit(null);

  int urandom = open("/dev/urandom", O_RDONLY | O_CLOEXEC);
  if (urandom < 0)
    return 92;
  rows[n++] = fioasync(urandom, 1);
  fioasync(urandom, 0);
  flags = fcntl(urandom, F_GETFL);
  rows[n++] = setfl(urandom, flags | O_ASYNC);
  setfl(urandom, flags);

  rows[n++] = fioasync(STDIN_FILENO, 1);
  fioasync(STDIN_FILENO, 0);
  flags = fcntl(STDIN_FILENO, F_GETFL);
  rows[n++] = setfl(STDIN_FILENO, flags | O_ASYNC);
  rows[n++] = async_bit(STDIN_FILENO);
  setfl(STDIN_FILENO, flags);

  if (n != (int)(sizeof(rows) / sizeof(rows[0])))
    return 93;
  return write(STDOUT_FILENO, rows, sizeof(rows)) == (ssize_t)sizeof(rows)
      ? 0
      : 94;
}
