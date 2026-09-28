/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

struct observation {
  uint64_t case_id, number, source_fd, args[6];
  int64_t result, before, after;
  unsigned char buffer[8], contents[8];
};
_Static_assert(sizeof(struct observation) == 112, "complete raw record layout");

/* In particular, do not pass fd through a C int-typed read/pread/pwrite API. */
static long raw_call(uint64_t number, const uint64_t args[6]) {
  register uint64_t r10 __asm__("r10") = args[3];
  register uint64_t r8 __asm__("r8") = args[4];
  register uint64_t r9 __asm__("r9") = args[5];
  long result;
  __asm__ volatile("syscall"
                   : "=a"(result)
                   : "0"(number), "D"(args[0]), "S"(args[1]), "d"(args[2]),
                     "r"(r10), "r"(r8), "r"(r9)
                   : "rcx", "r11", "memory");
  return result;
}

int main(int argc, char **argv) {
  if (argc != 2) return 10;
  uint64_t number;
  if (strcmp(argv[1], "read") == 0) number = SYS_read;
  else if (strcmp(argv[1], "pread") == 0) number = SYS_pread64;
  else if (strcmp(argv[1], "pwrite") == 0) number = SYS_pwrite64;
  else return 11;

  for (unsigned int index = 0; index < 5; ++index) {
    int fd = open("scalar-fd-data", O_CREAT | O_TRUNC | O_RDWR, 0600);
    if (fd < 0 || write(fd, "ABCDEFGH", 8) != 8 || lseek(fd, 1, SEEK_SET) != 1) return 12;
    int alias = dup(fd);
    if (alias < 0) return 13;
    struct observation out = {0};
    out.case_id = index;
    out.number = number;
    out.source_fd = (uint32_t)alias;
    memset(out.buffer, 'U', sizeof(out.buffer));
    if (number == SYS_pwrite64) memcpy(out.buffer, "xyz", 3);
    uint64_t prefix = index == 0 ? 0 : index == 2 ? UINT64_C(0xffffffff00000000) : UINT64_C(0x100000000);
    out.args[0] = prefix | (index == 3 ? UINT32_MAX : (uint32_t)alias);
    out.args[1] = (uintptr_t)out.buffer;
    out.args[2] = 3;
    out.args[3] = 4;
    out.args[4] = UINT64_C(0x76543210fedcba98);
    out.args[5] = UINT64_C(0x89abcdef01234567);
    if (index == 4 && close(alias) != 0) return 14;
    /* This fixture is single-threaded; no operation can reuse the closed alias
       between close and the raw syscall. Libtest runs it in a separate process. */
    out.before = lseek(fd, 0, SEEK_CUR);
    out.result = raw_call(number, out.args);
    out.after = lseek(fd, 0, SEEK_CUR);
    if (pread(fd, out.contents, sizeof(out.contents), 0) != sizeof(out.contents)) return 15;
    /* Retain every raw argument/result before an oracle failure exits. */
    if (write(STDOUT_FILENO, &out, sizeof(out)) != sizeof(out)) return 16;

    int valid = index < 3;
    if (out.result != (valid ? 3 : -EBADF)) return 20 + index;
    if (out.before != 1 || out.after != (valid && number == SYS_read ? 4 : 1)) return 30 + index;
    if (index != 4 && lseek(alias, 0, SEEK_CUR) != out.after) return 40 + index;
    const char *buffer = number == SYS_pwrite64 ? "xyzUUUUU" :
      valid ? (number == SYS_read ? "BCDUUUUU" : "EFGUUUUU") : "UUUUUUUU";
    const char *contents = valid && number == SYS_pwrite64 ? "ABCDxyzH" : "ABCDEFGH";
    if (memcmp(out.buffer, buffer, 8) != 0 || memcmp(out.contents, contents, 8) != 0) return 50 + index;
    if (index != 4 && close(alias) != 0) return 60;
    if (close(fd) != 0) return 61;
  }
  return 0;
}
