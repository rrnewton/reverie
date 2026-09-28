#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/syscall.h>
#include <unistd.h>

#define HIGH_WORD UINT64_C(0x5a5a5a5a00000000)
#define ALL_ONES_HIGH_WORD UINT64_C(0xffffffff00000000)
#define LOW_SIGN_BIT UINT64_C(0x80000000)

#define CHECK(expression)                                                      \
  do {                                                                         \
    if (!(expression)) {                                                       \
      fprintf(stderr, "failure line=%d errno=%d\n", __LINE__, errno);          \
      return 1;                                                                \
    }                                                                          \
  } while (0)

static int all_bytes_are(const void *object, size_t size, unsigned char byte) {
  const unsigned char *bytes = object;
  for (size_t index = 0; index < size; ++index) {
    if (bytes[index] != byte) return 0;
  }
  return 1;
}

int main(void) {
  int fd = open(".", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
  CHECK(fd >= 0);

  const uint64_t positive[] = {
      HIGH_WORD | (uint32_t)fd,
      ALL_ONES_HIGH_WORD | (uint32_t)fd,
  };
  for (size_t index = 0; index < sizeof(positive) / sizeof(positive[0]); ++index) {
    struct stat metadata;
    struct statfs filesystem;
    memset(&metadata, 0xa5, sizeof(metadata));
    memset(&filesystem, 0xa5, sizeof(filesystem));
    CHECK(syscall(SYS_fstat, positive[index], &metadata) == 0);
    CHECK(S_ISDIR(metadata.st_mode));
    CHECK(syscall(SYS_fstatfs, positive[index], &filesystem) == 0);
    CHECK(filesystem.f_type != 0);

    errno = 0;
    CHECK(syscall(SYS_fstat, positive[index], (void *)1) == -1 && errno == EFAULT);
    errno = 0;
    CHECK(syscall(SYS_fstatfs, positive[index], (void *)1) == -1 && errno == EFAULT);
  }

  const uint64_t invalid[] = {
      HIGH_WORD | LOW_SIGN_BIT | (uint32_t)fd,
      UINT64_MAX,
  };
  for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); ++index) {
    struct stat metadata;
    struct statfs filesystem;
    memset(&metadata, 0xa5, sizeof(metadata));
    memset(&filesystem, 0xa5, sizeof(filesystem));
    errno = 0;
    CHECK(syscall(SYS_fstat, invalid[index], &metadata) == -1 && errno == EBADF);
    CHECK(all_bytes_are(&metadata, sizeof(metadata), 0xa5));
    errno = 0;
    CHECK(syscall(SYS_fstatfs, invalid[index], &filesystem) == -1 && errno == EBADF);
    CHECK(all_bytes_are(&filesystem, sizeof(filesystem), 0xa5));
  }

  int closed = dup(fd);
  CHECK(closed >= 0);
  CHECK(close(closed) == 0);
  struct stat metadata;
  struct statfs filesystem;
  memset(&metadata, 0xa5, sizeof(metadata));
  memset(&filesystem, 0xa5, sizeof(filesystem));
  errno = 0;
  CHECK(syscall(SYS_fstat, HIGH_WORD | (uint32_t)closed, &metadata) == -1 &&
        errno == EBADF);
  CHECK(all_bytes_are(&metadata, sizeof(metadata), 0xa5));
  errno = 0;
  CHECK(syscall(SYS_fstatfs, HIGH_WORD | (uint32_t)closed, &filesystem) == -1 &&
        errno == EBADF);
  CHECK(all_bytes_are(&filesystem, sizeof(filesystem), 0xa5));

  errno = 0;
  CHECK(syscall(SYS_fstat, HIGH_WORD | (uint32_t)closed, (void *)1) == -1 &&
        errno == EBADF);
  errno = 0;
  CHECK(syscall(SYS_fstatfs, HIGH_WORD | (uint32_t)closed, (void *)1) == -1 &&
        errno == EBADF);

  CHECK(close(fd) == 0);
  puts("fstat-fstatfs-lowword-ok");
  return 0;
}
