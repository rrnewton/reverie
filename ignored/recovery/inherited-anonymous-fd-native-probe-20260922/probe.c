#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef STATX_MNT_ID_UNIQUE
#define STATX_MNT_ID_UNIQUE 0x00004000U
#endif

static void fail(const char *what) {
  dprintf(STDERR_FILENO, "%s: %s\n", what, strerror(errno));
  _exit(111);
}

static void require(int condition, const char *what) {
  if (!condition) {
    dprintf(STDERR_FILENO, "invariant failed: %s\n", what);
    _exit(112);
  }
}

static void fdinfo_identity(int fd, unsigned long long *inode,
                            unsigned long long *mount) {
  char path[64];
  snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", fd);
  int info = open(path, O_RDONLY | O_CLOEXEC);
  if (info < 0) fail("open fdinfo");
  char bytes[16384];
  size_t used = 0;
  while (used + 1 < sizeof(bytes)) {
    ssize_t count = read(info, bytes + used, sizeof(bytes) - 1 - used);
    if (count < 0) fail("read fdinfo");
    if (count == 0) break;
    used += (size_t)count;
  }
  require(close(info) == 0, "close fdinfo");
  require(used + 1 < sizeof(bytes), "fdinfo bounded");
  bytes[used] = 0;

  int found_inode = 0, found_mount = 0;
  char *save = NULL;
  for (char *line = strtok_r(bytes, "\n", &save); line;
       line = strtok_r(NULL, "\n", &save)) {
    unsigned long long value;
    if (sscanf(line, "ino:%llu", &value) == 1) {
      require(!found_inode, "single fdinfo ino");
      *inode = value;
      found_inode = 1;
    } else if (sscanf(line, "mnt_id:%llu", &value) == 1) {
      require(!found_mount, "single fdinfo mnt_id");
      *mount = value;
      found_mount = 1;
    }
  }
  require(found_inode && found_mount, "fdinfo identity fields");
}

static void observe(int report, const char *phase, const char *object, int fd) {
  struct stat st;
  struct statx basic, legacy, unique;
  memset(&basic, 0, sizeof(basic));
  memset(&legacy, 0, sizeof(legacy));
  memset(&unique, 0, sizeof(unique));
  if (fstat(fd, &st)) fail("fstat");
  if (statx(fd, "", AT_EMPTY_PATH, STATX_BASIC_STATS, &basic)) fail("statx basic");
  if (statx(fd, "", AT_EMPTY_PATH, STATX_BASIC_STATS | STATX_MNT_ID, &legacy))
    fail("statx legacy mount");
  if (statx(fd, "", AT_EMPTY_PATH, STATX_MNT_ID_UNIQUE, &unique))
    fail("statx unique mount");

  char proc_path[64], link[4096];
  snprintf(proc_path, sizeof(proc_path), "/proc/self/fd/%d", fd);
  ssize_t link_length = readlink(proc_path, link, sizeof(link) - 1);
  if (link_length < 0) fail("readlink proc fd");
  require((size_t)link_length < sizeof(link), "bounded proc link");
  link[link_length] = 0;

  unsigned long long info_inode = 0, info_mount = 0;
  fdinfo_identity(fd, &info_inode, &info_mount);

  require((basic.stx_mask & (STATX_TYPE | STATX_INO)) == (STATX_TYPE | STATX_INO),
          "basic statx mask");
  require(basic.stx_ino == (uint64_t)st.st_ino, "basic statx inode");
  require(basic.stx_dev_major == major(st.st_dev) &&
              basic.stx_dev_minor == minor(st.st_dev),
          "basic statx device");
  require(legacy.stx_mask & STATX_MNT_ID, "legacy mount mask");
  require(unique.stx_mask & STATX_MNT_ID_UNIQUE, "unique mount mask");
  require(info_inode == (unsigned long long)st.st_ino, "fdinfo inode");
  require(info_mount == legacy.stx_mnt_id, "fdinfo legacy mount");
  if (basic.stx_mask & STATX_MNT_ID)
    require(basic.stx_mnt_id == legacy.stx_mnt_id,
            "opportunistic basic mount identity");

  const char *type = "other";
  if (S_ISFIFO(st.st_mode)) type = "pipe";
  else if (S_ISSOCK(st.st_mode)) type = "socket";
  else if (S_ISREG(st.st_mode)) type = "regular";
  else if (S_ISCHR(st.st_mode)) type = "char";
  if (S_ISFIFO(st.st_mode) || S_ISSOCK(st.st_mode)) {
    char expected[128];
    snprintf(expected, sizeof(expected), "%s:[%llu]", type,
             (unsigned long long)st.st_ino);
    require(strcmp(link, expected) == 0, "anonymous proc link identity");
  }

  if (dprintf(report,
              "phase=%s pid=%ld object=%s fd=%d type=%s dev=%u:%u ino=%llu "
              "basic_mask=0x%x basic_dev=%u:%u basic_ino=%llu basic_mnt=%llu "
              "legacy_mask=0x%x legacy_mnt=%llu unique_mask=0x%x "
              "unique_mnt=%llu fdinfo_ino=%llu fdinfo_mnt=%llu link=%s\n",
              phase, (long)getpid(), object, fd, type, major(st.st_dev),
              minor(st.st_dev), (unsigned long long)st.st_ino, basic.stx_mask,
              basic.stx_dev_major, basic.stx_dev_minor,
              (unsigned long long)basic.stx_ino,
              (unsigned long long)basic.stx_mnt_id, legacy.stx_mask,
              (unsigned long long)legacy.stx_mnt_id, unique.stx_mask,
              (unsigned long long)unique.stx_mnt_id, info_inode, info_mount,
              link) < 0)
    fail("write report");
}

static void observe_objects(int report, const char *phase, const int fds[6]) {
  observe(report, phase, "pipe", fds[0]);
  observe(report, phase, "pipe", fds[1]);
  observe(report, phase, "pipe", fds[2]);
  observe(report, phase, "socket0", fds[3]);
  observe(report, phase, "socket1", fds[4]);
  observe(report, phase, "socket0", fds[5]);
}

static void observe_stdio(int report, const char *phase, const int fds[4]) {
  observe(report, phase, "stdout", fds[0]);
  observe(report, phase, "stderr", fds[1]);
  observe(report, phase, "stdout", fds[2]);
  observe(report, phase, "stderr", fds[3]);
}

static void exec_mode(const char *self, const char *mode, int report,
                      const int *fds, size_t count) {
  char report_text[32], fd_text[6][32];
  char *args[11] = {(char *)self, (char *)mode, report_text};
  snprintf(report_text, sizeof(report_text), "%d", report);
  require(count <= 6, "exec fd count");
  for (size_t i = 0; i < count; ++i) {
    snprintf(fd_text[i], sizeof(fd_text[i]), "%d", fds[i]);
    args[3 + i] = fd_text[i];
  }
  args[3 + count] = NULL;
  execv(self, args);
  fail("execv");
}

static int wait_clean(pid_t child) {
  int status = 0;
  if (waitpid(child, &status, 0) != child) return -1;
  return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
}

static void post_exec(const char *mode, int argc, char **argv) {
  size_t count = !strcmp(mode, "objects-exec") ? 6 : 4;
  require(argc == (int)(3 + count), "exec argument count");
  int report = atoi(argv[2]);
  int fds[6];
  for (size_t i = 0; i < count; ++i) fds[i] = atoi(argv[3 + i]);
  if (count == 6) observe_objects(report, "exec", fds);
  else observe_stdio(report, "exec", fds);
  pid_t child = fork();
  if (child < 0) fail("post-exec fork");
  if (child == 0) {
    if (count == 6) observe_objects(report, "postexec-fork", fds);
    else observe_stdio(report, "postexec-fork", fds);
    _exit(0);
  }
  require(wait_clean(child) == 0, "post-exec child status");
}

static void run_objects(const char *self, const char *report_path) {
  int report = open(report_path, O_WRONLY | O_CREAT | O_TRUNC | O_APPEND, 0600);
  if (report < 0) fail("open report");
  int pipe_fds[2], sockets[2];
  if (pipe(pipe_fds)) fail("pipe");
  if (socketpair(AF_UNIX, SOCK_STREAM, 0, sockets)) fail("socketpair");
  int fds[6] = {pipe_fds[0], pipe_fds[1], dup(pipe_fds[0]),
                sockets[0], sockets[1], dup(sockets[0])};
  require(fds[2] >= 0 && fds[5] >= 0, "dup objects");
  observe_objects(report, "parent", fds);
  pid_t child = fork();
  if (child < 0) fail("fork objects");
  if (child == 0) {
    observe_objects(report, "fork", fds);
    exec_mode(self, "objects-exec", report, fds, 6);
  }
  require(wait_clean(child) == 0, "objects child status");
  require(close(report) == 0, "close report");
}

static void run_stdio(const char *self, const char *report_path) {
  int report = open(report_path, O_WRONLY | O_CREAT | O_TRUNC | O_APPEND, 0600);
  if (report < 0) fail("open report");
  int fds[4] = {STDOUT_FILENO, STDERR_FILENO, dup(STDOUT_FILENO), dup(STDERR_FILENO)};
  require(fds[2] >= 0 && fds[3] >= 0, "dup stdio");
  observe_stdio(report, "parent", fds);
  pid_t child = fork();
  if (child < 0) fail("fork stdio");
  if (child == 0) {
    observe_stdio(report, "fork", fds);
    exec_mode(self, "stdio-exec", report, fds, 4);
  }
  require(wait_clean(child) == 0, "stdio child status");
  require(close(report) == 0, "close report");
}

int main(int argc, char **argv) {
  if (argc < 3) return 2;
  if (!strcmp(argv[1], "objects")) run_objects(argv[0], argv[2]);
  else if (!strcmp(argv[1], "stdio")) run_stdio(argv[0], argv[2]);
  else if (!strcmp(argv[1], "objects-exec") || !strcmp(argv[1], "stdio-exec"))
    post_exec(argv[1], argc, argv);
  else return 2;
  return 0;
}
