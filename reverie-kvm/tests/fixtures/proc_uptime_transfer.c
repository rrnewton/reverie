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
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#define CHECK(condition)                                                       \
  do {                                                                        \
    if (!(condition)) {                                                       \
      int saved_errno = errno;                                                \
      fprintf(stderr, "proc transfer line=%d check=%s errno=%d\n", __LINE__,    \
              #condition, saved_errno);                                       \
      return 1;                                                               \
    }                                                                         \
  } while (0)

struct sender {
  int socket;
  int descriptors[2];
  ssize_t sent;
  int send_errno;
  int closed[2];
};

static void *send_and_close(void *argument) {
  struct sender *sender = argument;
  char byte = 'p';
  union {
    struct cmsghdr alignment;
    char bytes[CMSG_SPACE(2 * sizeof(int))];
  } control = {0};
  struct iovec vector = {.iov_base = &byte, .iov_len = 1};
  struct msghdr message = {
      .msg_iov = &vector,
      .msg_iovlen = 1,
      .msg_control = control.bytes,
      .msg_controllen = sizeof(control.bytes),
  };
  struct cmsghdr *header = CMSG_FIRSTHDR(&message);
  header->cmsg_level = SOL_SOCKET;
  header->cmsg_type = SCM_RIGHTS;
  header->cmsg_len = CMSG_LEN(sizeof(sender->descriptors));
  memcpy(CMSG_DATA(header), sender->descriptors, sizeof(sender->descriptors));
  sender->sent = sendmsg(sender->socket, &message, 0);
  sender->send_errno = errno;
  sender->closed[0] = close(sender->descriptors[0]);
  sender->closed[1] = close(sender->descriptors[1]);
  return NULL;
}

static int canonical_uptime_path(int fd) {
  char path[64];
  char target[128];
  int length = snprintf(path, sizeof(path), "/proc/self/fd/%d", fd);
  CHECK(length > 0 && (size_t)length < sizeof(path));
  ssize_t bytes = readlink(path, target, sizeof(target) - 1);
  CHECK(bytes > 0 && (size_t)bytes < sizeof(target) - 1);
  target[bytes] = '\0';
  if (strcmp(target, "/proc/uptime") != 0) {
    fprintf(stderr, "proc transfer fd=%d canonical-path=%s expected=/proc/uptime\n",
            fd, target);
    return 1;
  }
  return 0;
}

int main(void) {
  int sockets[2];
  CHECK(socketpair(AF_UNIX, SOCK_DGRAM, 0, sockets) == 0);
  struct sender sender = {
      .socket = sockets[0],
      .descriptors = {open("/proc/uptime", O_RDONLY),
                      open("/proc/uptime", O_PATH | O_NOFOLLOW)},
  };
  CHECK(sender.descriptors[0] >= 0 && sender.descriptors[1] >= 0);
  struct stat before[2];
  int before_flags[2];
  for (int i = 0; i < 2; ++i) {
    CHECK(canonical_uptime_path(sender.descriptors[i]) == 0);
    CHECK(fstat(sender.descriptors[i], &before[i]) == 0);
    CHECK(S_ISREG(before[i].st_mode));
    before_flags[i] = fcntl(sender.descriptors[i], F_GETFL);
    CHECK(before_flags[i] >= 0);
  }
  CHECK(before[0].st_dev == before[1].st_dev &&
        before[0].st_ino == before[1].st_ino);
  CHECK((before_flags[0] & O_ACCMODE) == O_RDONLY);
  CHECK((before_flags[1] & (O_PATH | O_NOFOLLOW)) == (O_PATH | O_NOFOLLOW));

  pthread_t thread;
  CHECK(pthread_create(&thread, NULL, send_and_close, &sender) == 0);
  void *result = NULL;
  CHECK(pthread_join(thread, &result) == 0 && result == NULL);
  errno = sender.send_errno;
  CHECK(sender.sent == 1 && sender.closed[0] == 0 && sender.closed[1] == 0);
  for (int i = 0; i < 2; ++i) {
    errno = 0;
    CHECK(fcntl(sender.descriptors[i], F_GETFD) == -1 && errno == EBADF);
  }

  /* Joining and checking EBADF precede receive: only the queued rights retain
   * the sender's open descriptions, even though both threads share an fd table.
   */
  char byte = 0;
  union {
    struct cmsghdr alignment;
    char bytes[CMSG_SPACE(2 * sizeof(int))];
  } control = {0};
  struct iovec vector = {.iov_base = &byte, .iov_len = 1};
  struct msghdr message = {
      .msg_iov = &vector,
      .msg_iovlen = 1,
      .msg_control = control.bytes,
      .msg_controllen = sizeof(control.bytes),
  };
  CHECK(recvmsg(sockets[1], &message, MSG_CMSG_CLOEXEC) == 1 && byte == 'p');
  CHECK((message.msg_flags & (MSG_CTRUNC | MSG_TRUNC)) == 0);
  struct cmsghdr *header = CMSG_FIRSTHDR(&message);
  CHECK(header != NULL && header->cmsg_level == SOL_SOCKET &&
        header->cmsg_type == SCM_RIGHTS &&
        header->cmsg_len == CMSG_LEN(2 * sizeof(int)));
  CHECK(CMSG_NXTHDR(&message, header) == NULL);
  int received[2];
  memcpy(received, CMSG_DATA(header), sizeof(received));
  CHECK(received[0] >= 0 && received[1] >= 0 && received[0] != received[1]);
  for (int i = 0; i < 2; ++i) {
    /* Check the canonical path first so a failed baseline identifies the
     * fixed-proc metadata loss rather than a later consequence of that loss.
     */
    CHECK(canonical_uptime_path(received[i]) == 0);
    CHECK(fcntl(received[i], F_GETFL) == before_flags[i]);
    CHECK(fcntl(received[i], F_GETFD) == FD_CLOEXEC);
    struct stat after;
    CHECK(fstat(received[i], &after) == 0);
    CHECK(after.st_dev == before[i].st_dev && after.st_ino == before[i].st_ino &&
          after.st_mode == before[i].st_mode && after.st_size == before[i].st_size);
  }
  char content[128];
  CHECK(read(received[0], content, sizeof(content)) > 0);
  errno = 0;
  CHECK(read(received[1], content, 1) == -1 && errno == EBADF);
  CHECK(close(received[0]) == 0 && close(received[1]) == 0);
  CHECK(close(sockets[0]) == 0 && close(sockets[1]) == 0);
  puts("proc uptime transfer identity ok");
  return 0;
}
