#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

static void result(const char *name, long value) {
  printf("%s ret=%ld errno=%d(%s)\n", name, value,
         value == -1 ? errno : 0, value == -1 ? strerror(errno) : "Success");
}

static int fresh_writer(int pair[2]) {
  if (pipe2(pair, O_CLOEXEC) != 0) {
    perror("pipe2");
    return -1;
  }
  return pair[1];
}

static void close_pair(int pair[2]) {
  close(pair[0]);
  close(pair[1]);
}

int main(void) {
  int pair[2];
  struct stat st;
  struct statx stx;
  struct timespec times[2] = {
      {.tv_sec = 1640995199, .tv_nsec = 0},
      {.tv_sec = 1640995199, .tv_nsec = 0},
  };
  char output[64];

  int fd = fresh_writer(pair);
  if (fd < 0) return 100;
  if (fstat(fd, &st) != 0) return 101;
  printf("initial type=%#o mode=%#o uid=%u gid=%u mtime=%ld\n",
         st.st_mode & S_IFMT, st.st_mode & 07777, st.st_uid, st.st_gid,
         (long)st.st_mtime);
  errno = 0;
  long rc = syscall(SYS_fchmodat2, fd, "", 0640, AT_EMPTY_PATH);
  result("fchmodat2-empty", rc);
  if (fstat(fd, &st) != 0) return 102;
  printf("fchmodat2-after type=%#o mode=%#o\n", st.st_mode & S_IFMT,
         st.st_mode & 07777);
  close_pair(pair);

  fd = fresh_writer(pair);
  if (fd < 0) return 103;
  errno = 0;
  rc = syscall(SYS_fchmodat2, fd, "", 0640,
               AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW);
  result("fchmodat2-empty-nofollow", rc);
  close_pair(pair);

  fd = fresh_writer(pair);
  if (fd < 0) return 104;
  errno = 0;
  rc = utimensat(fd, "", times, AT_EMPTY_PATH);
  result("utimensat-empty", rc);
  if (fstat(fd, &st) != 0) return 105;
  printf("utimensat-empty-after atime=%ld.%09ld mtime=%ld.%09ld\n",
         (long)st.st_atim.tv_sec, st.st_atim.tv_nsec,
         (long)st.st_mtim.tv_sec, st.st_mtim.tv_nsec);
  close_pair(pair);

  fd = fresh_writer(pair);
  if (fd < 0) return 106;
  errno = 0;
  rc = utimensat(fd, NULL, times, 0);
  result("utimensat-null", rc);
  if (fstat(fd, &st) != 0) return 107;
  printf("utimensat-null-after atime=%ld.%09ld mtime=%ld.%09ld\n",
         (long)st.st_atim.tv_sec, st.st_atim.tv_nsec,
         (long)st.st_mtim.tv_sec, st.st_mtim.tv_nsec);
  close_pair(pair);

  fd = fresh_writer(pair);
  if (fd < 0) return 108;
  errno = 0;
  rc = utimensat(fd, "", times, AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW);
  result("utimensat-empty-nofollow", rc);
  close_pair(pair);

  fd = fresh_writer(pair);
  if (fd < 0) return 109;
  errno = 0;
  rc = fchownat(fd, "", (uid_t)-1, (gid_t)-1, AT_EMPTY_PATH);
  result("fchownat-empty-noop", rc);
  errno = 0;
  rc = fchownat(fd, "", getuid(), getgid(), AT_EMPTY_PATH);
  result("fchownat-empty-own-ids", rc);
  errno = 0;
  rc = fchownat(fd, "", (uid_t)-1, (gid_t)-1,
                AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW);
  result("fchownat-empty-nofollow", rc);
  close_pair(pair);

  fd = fresh_writer(pair);
  if (fd < 0) return 110;
  char target[128];
  snprintf(target, sizeof(target), "native-empty-probe-link-%ld", (long)getpid());
  errno = 0;
  rc = linkat(fd, "", AT_FDCWD, target, AT_EMPTY_PATH);
  result("linkat-empty", rc);
  printf("linkat-empty-created=%d\n", lstat(target, &st) == 0);
  if (rc == 0) unlink(target);
  errno = 0;
  rc = linkat(fd, "", AT_FDCWD, target,
              AT_EMPTY_PATH | AT_SYMLINK_FOLLOW);
  result("linkat-empty-follow", rc);
  printf("linkat-empty-follow-created=%d\n", lstat(target, &st) == 0);
  if (rc == 0) unlink(target);
  close_pair(pair);

  fd = fresh_writer(pair);
  if (fd < 0) return 111;
  errno = 0;
  rc = fstatat(fd, "", &st, AT_EMPTY_PATH);
  result("newfstatat-empty", rc);
  if (rc == 0)
    printf("newfstatat-empty-stat type=%#o mode=%#o uid=%u gid=%u\n",
           st.st_mode & S_IFMT, st.st_mode & 07777, st.st_uid, st.st_gid);
  errno = 0;
  rc = fstatat(fd, "", &st, AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW);
  result("newfstatat-empty-nofollow", rc);

  errno = 0;
  memset(&stx, 0, sizeof(stx));
  rc = syscall(SYS_statx, fd, "", AT_EMPTY_PATH, STATX_BASIC_STATS, &stx);
  result("statx-empty", rc);
  if (rc == 0)
    printf("statx-empty-stat type=%#o mode=%#o uid=%u gid=%u mask=%#x\n",
           stx.stx_mode & S_IFMT, stx.stx_mode & 07777, stx.stx_uid,
           stx.stx_gid, stx.stx_mask);
  errno = 0;
  rc = syscall(SYS_statx, fd, "", AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW,
               STATX_BASIC_STATS, &stx);
  result("statx-empty-nofollow", rc);

  const int modes[] = {F_OK, R_OK, W_OK, X_OK, R_OK | W_OK,
                       R_OK | X_OK, W_OK | X_OK};
  for (size_t i = 0; i < sizeof(modes) / sizeof(modes[0]); ++i) {
    char name[64];
    snprintf(name, sizeof(name), "faccessat2-empty-mode-%d", modes[i]);
    errno = 0;
    rc = syscall(SYS_faccessat2, fd, "", modes[i], AT_EMPTY_PATH);
    result(name, rc);
  }
  errno = 0;
  rc = syscall(SYS_faccessat2, fd, "", X_OK,
               AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW);
  result("faccessat2-empty-x-nofollow", rc);

  errno = 0;
  rc = readlinkat(fd, "", output, sizeof(output));
  result("readlinkat-empty", rc);
  close_pair(pair);
  return 0;
}
