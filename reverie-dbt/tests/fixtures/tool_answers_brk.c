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
 * only, so that nothing moves the break before it is looked at. */

#define _GNU_SOURCE
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

extern char end;

static char text[1 << 20];

static long read_all(const char *path) {
  int fd = (int)syscall(SYS_open, path, O_RDONLY);
  if (fd < 0)
    return -1;
  long total = 0;
  for (;;) {
    long n = syscall(SYS_read, fd, text + total, sizeof text - 1 - total);
    if (n <= 0)
      break;
    total += n;
  }
  syscall(SYS_close, fd);
  text[total] = 0;
  return total;
}

/* Whether a line of /proc/self/maps covers address. */
static int mapped(unsigned long address) {
  if (read_all("/proc/self/maps") < 0)
    return -1;
  for (char *line = text; *line; line = strchr(line, '\n') ? strchr(line, '\n') + 1 : line + strlen(line)) {
    char *dash;
    unsigned long low = strtoul(line, &dash, 16);
    unsigned long high = strtoul(dash + 1, NULL, 16);
    if (low <= address && address < high)
      return 1;
  }
  return 0;
}

static unsigned long start_brk(void) {
  if (read_all("/proc/self/stat") < 0)
    return 0;
  char *field = strrchr(text, ')');
  /* Fields 3 onward follow the command; start_brk is field 47. */
  for (int number = 2; field && number < 47; number++)
    field = strchr(field + 1, ' ');
  return field ? strtoul(field + 1, NULL, 10) : 0;
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
  say("page after the image mapped:", page_mapped == 1);
  say("brk(0) is the kernel's start_brk:", brk == start_brk());
  say("break in a mapping:", break_mapped == 1);
  return 0;
}
