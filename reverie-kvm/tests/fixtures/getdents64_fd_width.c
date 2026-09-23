#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#define CHECK(condition)                                                        \
  do {                                                                          \
    if (!(condition)) {                                                         \
      fprintf(stderr, "getdents64 check line=%d calls=%u errno=%d\n", __LINE__,   \
              calls, errno);                                                    \
      exit(90);                                                                 \
    }                                                                           \
  } while (0)

#define GUARD 16
#define BUFFER_SIZE 4096
#define CAP (16U * 1024U * 1024U)

static unsigned calls;
static const uint64_t upper_words[] = {
    0, UINT64_C(1) << 32, UINT64_C(1) << 63,
    UINT64_C(0x5a5a5a5a00000000), UINT64_C(0xffffffff00000000)};
static const unsigned counts[] = {0, 1, 23, 24, BUFFER_SIZE};
static const char *primary_names[] = {".", "..", "alpha", "beta-long", "nested"};
static const char *decoy_names[] = {".", "..", "decoy-only"};

struct record {
  uint64_t inode;
  int64_t cookie;
  uint16_t length;
  unsigned char type;
  char name[256];
};

struct records {
  size_t count;
  struct record entries[256];
};

static int all_bytes(const unsigned char *bytes, size_t count,
                     unsigned char value) {
  for (size_t i = 0; i < count; ++i)
    if (bytes[i] != value)
      return 0;
  return 1;
}

static long getdents(uint64_t fd, void *buffer, unsigned count) {
  ++calls;
  errno = 0;
  // Only fd carries noncanonical upper bits. Count stays unsigned32, and the
  // three unused registers must not influence decoding or output.
  return syscall(SYS_getdents64, fd, buffer, (uint64_t)count,
                 UINT64_C(0x123456789abcdef0), UINT64_MAX,
                 UINT64_C(0x800000005a5a5a5a));
}

static void result_is(uint64_t fd, unsigned count, long result,
                      long expected, int error) {
  if (result != expected || errno != error) {
    fprintf(stderr,
            "getdents64 fd=%016lx count=%u result=%ld/%d expected=%ld/%d\n",
            (unsigned long)fd, count, result, errno, expected, error);
    exit(91);
  }
}

static void rewind_directory(int fd) { CHECK(lseek(fd, 0, SEEK_SET) == 0); }

static void parse_records(const unsigned char *bytes, size_t length,
                          struct records *records) {
  records->count = 0;
  size_t offset = 0;
  while (offset < length) {
    CHECK(length - offset >= 24 && records->count < 256);
    struct record *record = &records->entries[records->count++];
    memcpy(&record->inode, bytes + offset, 8);
    memcpy(&record->cookie, bytes + offset + 8, 8);
    memcpy(&record->length, bytes + offset + 16, 2);
    record->type = bytes[offset + 18];
    CHECK(record->inode != 0);
    CHECK(record->length >= 24 && record->length % 8 == 0 &&
          record->length <= length - offset);
    const unsigned char *name = bytes + offset + 19;
    const unsigned char *end = memchr(name, 0, record->length - 19);
    CHECK(end != NULL && end > name && (size_t)(end - name) < sizeof(record->name));
    CHECK(record->length == ((20 + (size_t)(end - name) + 7) & ~(size_t)7));
    CHECK(memchr(name, '/', (size_t)(end - name)) == NULL);
    memcpy(record->name, name, (size_t)(end - name) + 1);
    CHECK(record->type == DT_UNKNOWN || record->type == DT_FIFO ||
          record->type == DT_CHR || record->type == DT_DIR ||
          record->type == DT_BLK || record->type == DT_REG ||
          record->type == DT_LNK || record->type == DT_SOCK);
    for (size_t previous = 0; previous + 1 < records->count; ++previous)
      CHECK(strcmp(records->entries[previous].name, record->name) != 0);
    offset += record->length;
  }
  CHECK(offset == length);
}

static void same_record(const struct record *actual,
                        const struct record *expected) {
  // Padding is unspecified. All named ABI fields, including the opaque cookie,
  // must agree when the same stream is rewound and read through a raw alias.
  CHECK(actual->inode == expected->inode && actual->cookie == expected->cookie &&
        actual->length == expected->length && actual->type == expected->type &&
        strcmp(actual->name, expected->name) == 0);
}

static void same_records(const struct records *actual,
                         const struct records *expected) {
  CHECK(actual->count == expected->count);
  for (size_t i = 0; i < actual->count; ++i)
    same_record(&actual->entries[i], &expected->entries[i]);
}

static void exact_names(int fd, int decoy, const struct records *records) {
  const char **names = decoy ? decoy_names : primary_names;
  const size_t count = decoy ? 3 : 5;
  CHECK(records->count == count);
  unsigned seen = 0;
  for (size_t i = 0; i < count; ++i) {
    const struct record *record = &records->entries[i];
    size_t index = 0;
    while (index < count && strcmp(record->name, names[index]) != 0)
      ++index;
    CHECK(index < count && (seen & (1U << index)) == 0);
    seen |= 1U << index;
    const unsigned char type =
        index < 2 || (!decoy && index == 4) ? DT_DIR : DT_REG;
    CHECK(record->type == type);
    struct stat metadata;
    CHECK(fstatat(fd, record->name, &metadata, AT_SYMLINK_NOFOLLOW) == 0);
    CHECK(record->inode == metadata.st_ino);
    CHECK(type == DT_DIR ? S_ISDIR(metadata.st_mode) : S_ISREG(metadata.st_mode));
  }
  CHECK(seen == (1U << count) - 1);
}

static void read_records(uint64_t raw_fd, unsigned count,
                         struct records *records) {
  unsigned char bytes[GUARD + BUFFER_SIZE + GUARD];
  CHECK(count <= BUFFER_SIZE);
  memset(bytes, 0xa5, sizeof(bytes));
  long result = getdents(raw_fd, bytes + GUARD, count);
  if (result <= 0 || (unsigned long)result > count || errno != 0) {
    fprintf(stderr, "getdents64 positive fd=%016lx count=%u result=%ld/%d\n",
            (unsigned long)raw_fd, count, result, errno);
    exit(92);
  }
  CHECK(all_bytes(bytes, GUARD, 0xa5));
  CHECK(all_bytes(bytes + GUARD + result, BUFFER_SIZE + GUARD - result, 0xa5));
  parse_records(bytes + GUARD, (size_t)result, records);
}

static void expect_zero(uint64_t raw_fd, int invalid_pointer, unsigned count) {
  unsigned char bytes[GUARD + BUFFER_SIZE + GUARD];
  memset(bytes, 0xa5, sizeof(bytes));
  void *buffer = invalid_pointer ? (void *)(uintptr_t)UINTPTR_MAX : bytes + GUARD;
  result_is(raw_fd, count, getdents(raw_fd, buffer, count), 0, 0);
  CHECK(all_bytes(bytes, sizeof(bytes), 0xa5));
}

static void scan(int fd, uint64_t upper, int decoy, struct records *records) {
  rewind_directory(fd);
  read_records(upper | (uint32_t)fd, BUFFER_SIZE, records);
  exact_names(fd, decoy, records);
  expect_zero(upper | (uint32_t)fd, 0, BUFFER_SIZE);
}

static void expect_error(uint64_t raw_fd, int invalid_pointer, unsigned count,
                         int error) {
  unsigned char bytes[GUARD + BUFFER_SIZE + GUARD];
  memset(bytes, 0xa5, sizeof(bytes));
  void *buffer = invalid_pointer ? (void *)(uintptr_t)UINTPTR_MAX : bytes + GUARD;
  result_is(raw_fd, count, getdents(raw_fd, buffer, count), -1, error);
  CHECK(all_bytes(bytes, sizeof(bytes), 0xa5));
}

static void install_at(int source, int target) {
  CHECK(source >= 0);
  if (source != target) {
    CHECK(dup2(source, target) == target);
    CHECK(close(source) == 0);
  }
}

static int inherit_proc_path(void) {
  // The harness supplies a real O_PATH /proc descriptor as stdin. Preserve it
  // before installing ordinary directories at fd0/3; synthetic proc open has a
  // separate, deliberately different O_PATH policy checked below.
  struct stat before, after;
  const int flags = fcntl(0, F_GETFL);
  CHECK(flags >= 0 && (flags & (O_PATH | O_DIRECTORY)) == (O_PATH | O_DIRECTORY));
  CHECK(fstat(0, &before) == 0 && S_ISDIR(before.st_mode));
  const int fd = fcntl(0, F_DUPFD, 301);
  CHECK(fd == 301 && fcntl(fd, F_GETFL) == flags);
  CHECK(fstat(fd, &after) == 0 && S_ISDIR(after.st_mode));
  CHECK(before.st_dev == after.st_dev && before.st_ino == after.st_ino &&
        before.st_mode == after.st_mode);
  puts("getdents64 inherited-proc-opath fd=301 identity=preserved");
  return fd;
}

static void proc_path_open_policy(int guest, int inherited) {
  errno = 0;
  const int fd = open("/proc", O_PATH | O_DIRECTORY);
  if (guest) {
    CHECK(fd == -1 && errno == EINVAL);
  } else {
    CHECK(fd >= 0 && errno == 0);
    struct stat expected, actual;
    CHECK(fstat(inherited, &expected) == 0 && fstat(fd, &actual) == 0);
    CHECK(S_ISDIR(actual.st_mode) && actual.st_dev == expected.st_dev &&
          actual.st_ino == expected.st_ino && actual.st_mode == expected.st_mode);
    CHECK(fcntl(fd, F_GETFL) == fcntl(inherited, F_GETFL));
    CHECK(close(fd) == 0);
  }
  puts(guest ? "getdents64 policy=synthetic-proc-opath open=EINVAL"
             : "getdents64 policy=native-proc-opath open=success");
}

static void error_order(int regular, int path_only, int proc_path_only) {
  const unsigned before = calls;
  const uint32_t invalid[] = {UINT32_C(0x80000000), UINT32_C(0x80000003),
                              UINT32_C(0x80000101), UINT32_MAX,
                              (uint32_t)AT_FDCWD, INT32_MAX, 300};
  const int valid[] = {regular, path_only, proc_path_only};
  const int errors[] = {ENOTDIR, EBADF, EBADF};
  for (size_t high = 0; high < 5; ++high) {
    for (size_t index = 0; index < 7; ++index)
      for (size_t count = 0; count < 5; ++count)
        for (int bad_pointer = 0; bad_pointer < 2; ++bad_pointer)
          expect_error(upper_words[high] | invalid[index], bad_pointer,
                       counts[count], EBADF);
    for (size_t index = 0; index < 3; ++index)
      for (size_t count = 0; count < 5; ++count)
        for (int bad_pointer = 0; bad_pointer < 2; ++bad_pointer)
          expect_error(upper_words[high] | (uint32_t)valid[index], bad_pointer,
                       counts[count], errors[index]);
  }
  CHECK(calls - before == 500);
  printf("getdents64 error-order rows=%u buffers=unchanged\n", calls - before);
}

static void cursor_controls(int fd, const char *path, int decoy,
                            const struct records *baseline) {
  const unsigned before = calls;
  unsigned char *inaccessible = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  CHECK(inaccessible != MAP_FAILED);
  memset(inaccessible, 0xa5, 4096);
  // This descriptor control requires inaccessible-page rejection before any
  // cursor advance. Read-only copyout is a separate existing backend gap; the
  // failing read-only probe is retained as diagnostic evidence for that gap.
  CHECK(mprotect(inaccessible, 4096, PROT_NONE) == 0);
  int alias = dup(fd);
  CHECK(alias >= 0);
  off_t short_positions[3] = {0};
  for (size_t high = 0; high < 5; ++high) {
    uint64_t raw_fd = upper_words[high] | (uint32_t)fd;
    rewind_directory(fd);
    for (size_t count = 0; count < 3; ++count) {
      expect_error(raw_fd, 0, counts[count], EINVAL);
      const off_t position = lseek(fd, 0, SEEK_CUR);
      CHECK(position >= 0 && lseek(alias, 0, SEEK_CUR) == position);
      if (high == 0)
        short_positions[count] = position;
      else
        CHECK(position == short_positions[count]);
    }
    for (size_t count = 3; count < 5; ++count) {
      const off_t before_invalid = lseek(fd, 0, SEEK_CUR);
      CHECK(before_invalid >= 0 && lseek(alias, 0, SEEK_CUR) == before_invalid);
      expect_error(raw_fd, 1, counts[count], EFAULT);
      CHECK(lseek(fd, 0, SEEK_CUR) == before_invalid &&
            lseek(alias, 0, SEEK_CUR) == before_invalid);
      const off_t before_inaccessible = lseek(fd, 0, SEEK_CUR);
      CHECK(before_inaccessible >= 0 &&
            lseek(alias, 0, SEEK_CUR) == before_inaccessible);
      result_is(raw_fd, counts[count], getdents(raw_fd, inaccessible, counts[count]),
                -1, EFAULT);
      CHECK(lseek(fd, 0, SEEK_CUR) == before_inaccessible &&
            lseek(alias, 0, SEEK_CUR) == before_inaccessible);
      CHECK(mprotect(inaccessible, 4096, PROT_READ) == 0);
      CHECK(all_bytes(inaccessible, 4096, 0xa5));
      CHECK(mprotect(inaccessible, 4096, PROT_NONE) == 0);
    }
    struct records actual;
    // Read before rewinding: an EFAULT must not silently consume any entries.
    read_records(raw_fd, BUFFER_SIZE, &actual);
    exact_names(fd, decoy, &actual);
    same_records(&actual, baseline);
    expect_zero(raw_fd, 1, 1);
    rewind_directory(fd);
    read_records(raw_fd, 24, &actual);
    CHECK(actual.count == 1);
    same_record(&actual.entries[0], &baseline->entries[0]);
    struct records rest;
    read_records(upper_words[high] | (uint32_t)alias, BUFFER_SIZE, &rest);
    CHECK(rest.count + 1 == baseline->count);
    for (size_t i = 0; i < rest.count; ++i)
      same_record(&rest.entries[i], &baseline->entries[i + 1]);
    expect_zero(raw_fd, 0, BUFFER_SIZE);
    rewind_directory(alias);
    read_records(raw_fd, BUFFER_SIZE, &actual);
    exact_names(fd, decoy, &actual);
    same_records(&actual, baseline);
    expect_zero(raw_fd, 1, 1);
    int fresh = open(path, O_RDONLY | O_DIRECTORY);
    CHECK(fresh >= 0);
    scan(fresh, upper_words[high], decoy, &actual);
    same_records(&actual, baseline);
    CHECK(close(fresh) == 0);
  }
  CHECK(close(alias) == 0 && munmap(inaccessible, 4096) == 0);
  CHECK(calls - before == 80);
  // Fixed-width native vectors are compared byte-for-byte with every direct
  // KVM and Tool run by the Rust harness, in addition to the alias checks above.
  printf("getdents64 short-count-cookies fd=%d count0=%016lx count1=%016lx "
         "count23=%016lx\n", fd, (unsigned long)short_positions[0],
         (unsigned long)short_positions[1], (unsigned long)short_positions[2]);
  printf("getdents64 cursor fd=%d rows=%u short=EINVAL fault=unchanged dup=shared "
         "rewind=fresh eof=zero\n", fd, calls - before);
}

static void staging_boundary(const struct records *baseline) {
  const unsigned before = calls;
  unsigned char *bytes = mmap(NULL, GUARD + CAP + 1 + GUARD,
                              PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  CHECK(bytes != MAP_FAILED);
  const uint64_t raw_fd = UINT64_C(0xffffffff00000003);
  for (unsigned count = CAP; count <= CAP + 1; ++count) {
    memset(bytes, 0xa5, GUARD + CAP + 1 + GUARD);
    rewind_directory(3);
    long result = getdents(raw_fd, bytes + GUARD, count);
    CHECK(result > 0 && result < BUFFER_SIZE && errno == 0);
    CHECK(all_bytes(bytes, GUARD, 0xa5));
    CHECK(all_bytes(bytes + GUARD + result, CAP + 1 + GUARD - result, 0xa5));
    struct records actual;
    parse_records(bytes + GUARD, (size_t)result, &actual);
    exact_names(3, 0, &actual);
    same_records(&actual, baseline);
    expect_zero(raw_fd, 1, 1);
  }
  CHECK(munmap(bytes, GUARD + CAP + 1 + GUARD) == 0);
  CHECK(calls - before == 4);
  puts("getdents64 canonical-count boundary=16777216,16777217 guards=unchanged");
}

static void proc_policy(int guest) {
  const unsigned before = calls;
  int fd = open("/proc", O_RDONLY | O_DIRECTORY);
  CHECK(fd >= 0);
  int alias = dup(fd);
  CHECK(alias >= 0);
  const int descriptors[] = {fd, alias};
  for (size_t high = 0; high < 5; ++high)
    for (size_t descriptor = 0; descriptor < 2; ++descriptor)
      for (size_t count = 0; count < 5; ++count) {
        uint64_t raw_fd = upper_words[high] | (uint32_t)descriptors[descriptor];
        rewind_directory(fd);
        if (guest) {
          // Synthetic proc is deliberately unenumerable, before pointer and
          // count validation, including through a dup alias.
          expect_zero(raw_fd, 0, counts[count]);
          expect_zero(raw_fd, 1, counts[count]);
          CHECK(lseek(fd, 0, SEEK_CUR) == 0);
        } else if (count < 3) {
          expect_error(raw_fd, 0, counts[count], EINVAL);
          expect_error(raw_fd, 1, counts[count], EINVAL);
        } else {
          struct records actual;
          read_records(raw_fd, counts[count], &actual);
          CHECK(actual.count > 0);
          rewind_directory(fd);
          expect_error(raw_fd, 1, counts[count], EFAULT);
        }
      }
  CHECK(close(alias) == 0 && close(fd) == 0);
  CHECK(calls - before == 100);
  puts(guest ? "getdents64 policy=synthetic-proc rows=100 zero-before-buffer unchanged"
             : "getdents64 policy=native-proc rows=100 records-and-errors checked");
}

int main(int argc, char **argv) {
  CHECK(argc == 5);
  int guest = strcmp(argv[1], "kvm") == 0;
  CHECK(guest || strcmp(argv[1], "native") == 0);
  const int proc_path_only = inherit_proc_path();
  install_at(open(argv[2], O_RDONLY | O_DIRECTORY), 3);
  CHECK(dup2(3, 0) == 0);
  install_at(open(argv[3], O_RDONLY | O_DIRECTORY), 257);
  int regular = open(argv[4], O_RDONLY);
  int path_only = open(argv[2], O_PATH | O_DIRECTORY);
  CHECK(regular >= 0 && path_only >= 0);
  CHECK(dup2(3, 300) == 300 && close(300) == 0);

  const int descriptors[] = {0, 3, 257};
  struct records baselines[3];
  for (size_t descriptor = 0; descriptor < 3; ++descriptor) {
    int fd = descriptors[descriptor], decoy = descriptor == 2;
    for (size_t high = 0; high < 5; ++high) {
      struct records actual;
      scan(fd, upper_words[high], decoy, &actual);
      if (high == 0)
        baselines[descriptor] = actual;
      else
        same_records(&actual, &baselines[descriptor]);
      printf("getdents64 fd=%d upper=%016lx exact=%s\n", fd,
             (unsigned long)upper_words[high],
             decoy ? ".,..,decoy-only" : ".,..,alpha,beta-long,nested");
    }
  }
  CHECK(calls == 30);
  error_order(regular, path_only, proc_path_only);
  for (size_t descriptor = 0; descriptor < 3; ++descriptor)
    cursor_controls(descriptors[descriptor], descriptor == 2 ? argv[3] : argv[2],
                    descriptor == 2, &baselines[descriptor]);
  staging_boundary(&baselines[1]);
  proc_path_open_policy(guest, proc_path_only);
  proc_policy(guest);
  CHECK(close(regular) == 0 && close(path_only) == 0 && close(proc_path_only) == 0);
  CHECK(close(0) == 0 && close(3) == 0 && close(257) == 0);
  CHECK(calls == 874);
  printf("getdents64 checked calls=%u\n", calls);
  return 0;
}
