/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/* What the guest finds where its program break starts, before anything has
 * allocated: whether the page after its image is mapped, whether brk(0) is
 * the kernel's start_brk, and whether the break lies in a mapping. Natively,
 * none is mapped and brk(0) is start_brk. Raw syscalls and static buffers
 * only, so that nothing moves the break before it is looked at; an
 * observation that cannot be made fails the run instead of reading as "no". */

#define _GNU_SOURCE
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

extern char end;

static char text[1 << 20];

/* Exits nonzero, naming what failed: an observation the fixture could not
 * make must not read as an absence. */
static void fail(const char *what) {
  syscall(SYS_write, 2, what, strlen(what));
  syscall(SYS_write, 2, "\n", 1);
  syscall(SYS_exit_group, 2);
}

/* The whole of a procfs file into `text`, or a failure: an open or read
 * error, or a file that fills the buffer, which may be truncated. */
static void read_all(const char *path) {
  int fd = (int)syscall(SYS_open, path, O_RDONLY);
  if (fd < 0)
    fail("cannot open a procfs file");
  long total = 0;
  for (;;) {
    long n = syscall(SYS_read, fd, text + total, sizeof text - 1 - total);
    if (n < 0)
      fail("cannot read a procfs file");
    if (n == 0)
      break;
    total += n;
    if (total == (long)sizeof text - 1)
      fail("a procfs file fills the buffer and may be truncated");
  }
  syscall(SYS_close, fd);
  text[total] = 0;
  if (total == 0)
    fail("a procfs file is empty");
}

/* Whether a line of /proc/self/maps covers address. */
static int mapped(unsigned long address) {
  read_all("/proc/self/maps");
  for (char *line = text; *line;) {
    char *dash, *after;
    unsigned long low = strtoul(line, &dash, 16);
    if (dash == line || *dash != '-')
      fail("a /proc/self/maps line does not start with a range");
    unsigned long high = strtoul(dash + 1, &after, 16);
    if (after == dash + 1 || *after != ' ')
      fail("a /proc/self/maps range has no end");
    if (low <= address && address < high)
      return 1;
    char *newline = strchr(line, '\n');
    if (!newline)
      fail("a /proc/self/maps line has no end");
    line = newline + 1;
  }
  return 0;
}

static unsigned long start_brk(void) {
  read_all("/proc/self/stat");
  char *field = strrchr(text, ')');
  /* Fields 3 onward follow the command; start_brk is field 47. */
  for (int number = 2; field && number < 47; number++)
    field = strchr(field + 1, ' ');
  if (!field)
    fail("/proc/self/stat has no field 47");
  char *after;
  unsigned long value = strtoul(field + 1, &after, 10);
  if (after == field + 1)
    fail("/proc/self/stat field 47 is not a number");
  return value;
}

static void say(const char *what, int yes) {
  syscall(SYS_write, 1, what, strlen(what));
  syscall(SYS_write, 1, yes ? " yes\n" : " no\n", yes ? 5 : 4);
}

int main(void) {
  unsigned long after_image = ((unsigned long)&end + 4095) & ~4095UL;
  unsigned long brk = (unsigned long)syscall(SYS_brk, 0);
  int page_mapped = mapped(after_image);
  int break_mapped = mapped(brk);
  unsigned long kernel_start = start_brk();
  say("page after the image mapped:", page_mapped);
  say("brk(0) is the kernel's start_brk:", brk == kernel_start);
  say("break in a mapping:", break_mapped);
  return 0;
}
