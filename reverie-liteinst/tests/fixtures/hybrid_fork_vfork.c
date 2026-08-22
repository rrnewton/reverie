/* A forked child that then vforks. The vfork refusal is raised in the CHILD
   task, which has no outer cleanup guard of its own and whose `Err` is only
   logged, so the root goes on to exit zero. The session must refuse to report
   that success. This is the shape `system()`, `popen()` and `posix_spawn()`
   take in a forked child; it exists to make a silent green impossible, not to
   be supported. */
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

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
  pid_t child = fork();
  if (child < 0) {
    return 10;
  }
  if (child == 0) {
    /* vfork's contract is satisfied: the new task does nothing but _exit. */
    pid_t grandchild = vfork();
    if (grandchild == 0) {
      _exit(0);
    }
    if (grandchild < 0) {
      _exit(11);
    }
    int grandchild_status = 0;
    (void)waitpid(grandchild, &grandchild_status, 0);
    _exit(0);
  }
  int status = 0;
  (void)waitpid(child, &status, 0);
  puts("fork-vfork-root-finished");
  return 0;
}
