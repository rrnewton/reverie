/* Two generations of children: the root forks a child, and that child forks a
   grandchild. The grandchild's PTRACE_EVENT_FORK is reported to a NON-ROOT
   parent, which is the case that the LiteInst cleanup guard's newborn
   registration has to cover; registering newborns only for the root leaves the
   grandchild unregistered and `handle_new_task` aborts on it.

   The two pipes are a deterministic continuation barrier. The grandchild
   cannot exit (and therefore cannot create SIGCHLD) until the root receives a
   byte that the non-root parent writes only after its fork returns. An
   accidental post-NewChild single-step stops that parent before the byte and
   cannot be masked by a racing SIGCHLD. */
#include <errno.h>
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int write_byte(int fd, char value) {
  ssize_t written;
  do {
    written = write(fd, &value, 1);
  } while (written < 0 && errno == EINTR);
  return written == 1;
}

static int read_byte(int fd, char expected) {
  char value = 0;
  ssize_t received;
  do {
    received = read(fd, &value, 1);
  } while (received < 0 && errno == EINTR);
  return received == 1 && value == expected;
}

static int reap(pid_t child) {
  int status = 0;
  if (waitpid(child, &status, 0) != child) {
    return 0;
  }
  return WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

int main(int argc, char **argv) {
  if (argc != 3 || prctl(PR_SET_NAME, argv[1], 0, 0, 0) != 0) {
    return 9;
  }
  FILE *pid_file = fopen(argv[2], "w");
  if (pid_file == NULL) {
    return 8;
  }
  fprintf(pid_file, "%ld\n", (long)getpid());
  if (fclose(pid_file) != 0) {
    return 7;
  }

  int parent_ready[2];
  int release_grandchild[2];
  if (pipe(parent_ready) != 0) {
    return 10;
  }
  if (pipe(release_grandchild) != 0) {
    close(parent_ready[0]);
    close(parent_ready[1]);
    return 11;
  }

  pid_t child = fork();
  if (child < 0) {
    close(parent_ready[0]);
    close(parent_ready[1]);
    close(release_grandchild[0]);
    close(release_grandchild[1]);
    return 12;
  }
  if (child == 0) {
    close(parent_ready[0]);
    close(release_grandchild[1]);
    pid_t grandchild = fork();
    if (grandchild < 0) {
      close(parent_ready[1]);
      close(release_grandchild[0]);
      _exit(13);
    }
    if (grandchild == 0) {
      close(parent_ready[1]);
      int released = read_byte(release_grandchild[0], 'G');
      close(release_grandchild[0]);
      _exit(released ? 0 : 14);
    }

    close(release_grandchild[0]);
    int continued = write_byte(parent_ready[1], 'P');
    close(parent_ready[1]);
    int grandchild_ok = reap(grandchild);
    _exit(continued && grandchild_ok ? 0 : 15);
  }

  close(parent_ready[1]);
  close(release_grandchild[0]);
  int parent_continued = read_byte(parent_ready[0], 'P');
  close(parent_ready[0]);
  int released = parent_continued && write_byte(release_grandchild[1], 'G');
  close(release_grandchild[1]);
  if (!reap(child)) {
    return 16;
  }
  if (!parent_continued) {
    return 17;
  }
  if (!released) {
    return 18;
  }
  puts("fork-tree-followed");
  return 0;
}
