#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "physical-stdio-rights line=%d errno=%d check=%s\n", __LINE__, errno, #x); _exit(82); } } while (0)

union control_buffer {
  struct cmsghdr align;
  unsigned char bytes[CMSG_SPACE(sizeof(int))];
};

static uintmax_t number(const char *text) {
  char *end;
  errno = 0;
  uintmax_t result = strtoumax(text, &end, 10);
  CHECK(errno == 0 && end != text && *end == '\0');
  return result;
}

static void identity(int fd, uintmax_t dev, uintmax_t ino, struct stat *result) {
  CHECK(fstat(fd, result) == 0);
  CHECK((uintmax_t)result->st_dev == dev && (uintmax_t)result->st_ino == ino);
}

static void complete(int report, const char *message) {
  size_t length = strlen(message);
  CHECK(write(report, message, length) == (ssize_t)length);
}

static void exercise_transfer(int donor, int native_alias, uintmax_t dev, uintmax_t ino) {
  int pair[2];
  CHECK(socketpair(AF_UNIX, SOCK_DGRAM, 0, pair) == 0);
  struct stat original, received;
  identity(donor, dev, ino, &original);
  CHECK(S_ISREG(original.st_mode) || S_ISCHR(original.st_mode));
  int original_flags = fcntl(donor, F_GETFL);
  CHECK(original_flags >= 0 && (original_flags & O_ACCMODE) == O_RDWR);
  off_t original_offset = lseek(donor, 0, SEEK_CUR);
  CHECK(original_offset >= 0);

  char byte = 'R';
  struct iovec iov = {&byte, 1};
  union control_buffer sent = {0};
  struct msghdr send = {.msg_iov = &iov, .msg_iovlen = 1,
    .msg_control = sent.bytes, .msg_controllen = sizeof(sent.bytes)};
  struct cmsghdr *header = CMSG_FIRSTHDR(&send);
  CHECK(header != NULL);
  header->cmsg_level = SOL_SOCKET;
  header->cmsg_type = SCM_RIGHTS;
  header->cmsg_len = CMSG_LEN(sizeof(int));
  memcpy(CMSG_DATA(header), &donor, sizeof(donor));
  CHECK(sendmsg(pair[0], &send, 0) == 1);

  byte = 0;
  union control_buffer control;
  memset(&control, 0xa5, sizeof(control));
  struct msghdr receive = {.msg_iov = &iov, .msg_iovlen = 1,
    .msg_control = control.bytes, .msg_controllen = sizeof(control.bytes)};
  CHECK(recvmsg(pair[1], &receive, MSG_CMSG_CLOEXEC) == 1 && byte == 'R');
  CHECK(!(receive.msg_flags & (MSG_CTRUNC | MSG_TRUNC)));
  CHECK(receive.msg_controllen == CMSG_SPACE(sizeof(int)));
  header = CMSG_FIRSTHDR(&receive);
  CHECK(header != NULL && header->cmsg_level == SOL_SOCKET && header->cmsg_type == SCM_RIGHTS);
  CHECK(header->cmsg_len == CMSG_LEN(sizeof(int)) && CMSG_NXTHDR(&receive, header) == NULL);
  int fd = -1;
  memcpy(&fd, CMSG_DATA(header), sizeof(fd));
  CHECK(fd >= 0 && fd != donor && fcntl(fd, F_GETFD) == FD_CLOEXEC);
  identity(fd, dev, ino, &received);
  CHECK(received.st_mode == original.st_mode && received.st_size == original.st_size);
  CHECK(fcntl(fd, F_GETFL) == original_flags);

  int changed_flags = original_flags ^ O_APPEND;
  CHECK(fcntl(fd, F_SETFL, changed_flags) == 0);
  CHECK(fcntl(fd, F_GETFL) == changed_flags && fcntl(donor, F_GETFL) == changed_flags);
  off_t moved = S_ISREG(original.st_mode) ? 17 : 0;
  CHECK(lseek(fd, 17, SEEK_SET) == moved && lseek(donor, 0, SEEK_CUR) == moved);
  if (native_alias) {
    // This establishes actual sharing with native physical stdout/stderr,
    // independently of inode equality. Linux permits the SCM transfer.
    for (int standard = 1; standard <= 2; ++standard) {
      struct stat physical;
      identity(standard, dev, ino, &physical);
      CHECK(fcntl(standard, F_GETFL) == changed_flags);
      CHECK(lseek(standard, 0, SEEK_CUR) == moved);
    }
  }
  CHECK(close(donor) == 0);
  CHECK(fcntl(fd, F_GETFL) == changed_flags && lseek(fd, 0, SEEK_CUR) == moved);
  char probe = '?';
  ssize_t count = pread(fd, &probe, 1, 0);
  CHECK(S_ISREG(original.st_mode) ? count == 1 && probe == 's' : count == 0 && probe == '?');
  CHECK(fcntl(fd, F_SETFL, original_flags) == 0);
  CHECK(lseek(fd, original_offset, SEEK_SET) == original_offset);
  if (native_alias) {
    for (int standard = 1; standard <= 2; ++standard) {
      CHECK(fcntl(standard, F_GETFL) == original_flags);
      CHECK(lseek(standard, 0, SEEK_CUR) == original_offset);
    }
  }
  CHECK(close(fd) == 0 && close(pair[0]) == 0 && close(pair[1]) == 0);
}

static void exercise_refusal(uintmax_t dev, uintmax_t ino) {
  struct stat before, after;
  identity(0, dev, ino, &before);
  int flags = fcntl(0, F_GETFL), descriptor_flags = fcntl(0, F_GETFD);
  off_t offset = lseek(0, 0, SEEK_CUR);
  CHECK(flags >= 0 && descriptor_flags >= 0 && offset >= 0);
  int pair[2];
  CHECK(socketpair(AF_UNIX, SOCK_DGRAM, 0, pair) == 0);
  char byte = 'R';
  struct iovec iov = {&byte, 1};
  union control_buffer sent;
  memset(&sent, 0xa5, sizeof(sent));
  struct msghdr send = {.msg_iov = &iov, .msg_iovlen = 1,
    .msg_control = sent.bytes, .msg_controllen = sizeof(sent.bytes)};
  struct cmsghdr *header = CMSG_FIRSTHDR(&send);
  CHECK(header != NULL);
  header->cmsg_level = SOL_SOCKET;
  header->cmsg_type = SCM_RIGHTS;
  header->cmsg_len = CMSG_LEN(sizeof(int));
  int donor = 0;
  memcpy(CMSG_DATA(header), &donor, sizeof(donor));
  union control_buffer sent_before = sent;
  errno = 0;
  CHECK(sendmsg(pair[0], &send, 0) == -1 && errno == ENOSYS);
  CHECK(byte == 'R' && memcmp(&sent, &sent_before, sizeof(sent)) == 0);
  CHECK(send.msg_controllen == sizeof(sent.bytes));

  union control_buffer control;
  memset(&control, 0xa5, sizeof(control));
  union control_buffer control_before = control;
  byte = 'Q';
  struct msghdr receive = {.msg_iov = &iov, .msg_iovlen = 1,
    .msg_control = control.bytes, .msg_controllen = sizeof(control.bytes)};
  errno = 0;
  CHECK(recvmsg(pair[1], &receive, MSG_DONTWAIT | MSG_CMSG_CLOEXEC) == -1 && errno == EAGAIN);
  CHECK(byte == 'Q' && memcmp(&control, &control_before, sizeof(control)) == 0);
  CHECK(receive.msg_controllen == sizeof(control.bytes) && receive.msg_flags == 0);
  CHECK(fcntl(0, F_GETFL) == flags && fcntl(0, F_GETFD) == descriptor_flags);
  CHECK(lseek(0, 0, SEEK_CUR) == offset);
  identity(0, dev, ino, &after);
  CHECK(after.st_mode == before.st_mode && after.st_size == before.st_size);
  CHECK(close(pair[0]) == 0 && close(pair[1]) == 0);
}

int main(int argc, char **argv) {
  alarm(15);
  CHECK(argc == 6);
  uintmax_t dev = number(argv[3]), ino = number(argv[4]), report_number = number(argv[5]);
  CHECK(report_number <= INT_MAX);
  int report = (int)report_number;
  if (strcmp(argv[1], "open") == 0) {
    int fd = open(argv[2], O_RDWR | O_CLOEXEC);
    CHECK(fd >= 0 && fcntl(fd, F_GETFD) == FD_CLOEXEC);
    exercise_transfer(fd, 0, dev, ino);
    complete(report, "physical-stdio-independent-rights-ok\n");
  } else if (strcmp(argv[1], "stdin-native") == 0) {
    exercise_transfer(0, 1, dev, ino);
    complete(report, "physical-stdio-native-alias-rights-ok\n");
  } else {
    CHECK(strcmp(argv[1], "stdin-refuse") == 0);
    exercise_refusal(dev, ino);
    complete(report, "physical-stdio-alias-refused-ok\n");
  }
  return 0;
}
