/* Discriminating probe: does execveat SUCCEED (process replaced) or get refused? */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>
extern char **environ;
int main(void) {
  char *const av[] = {"/bin/echo", "EXECVEAT-SUCCEEDED-PROCESS-REPLACED", NULL};
  errno = 0;
  long r = syscall(SYS_execveat, AT_FDCWD, "/bin/echo", av, environ, 0);
  printf("EXECVEAT-REFUSED r=%ld errno=%d\n", r, errno);
  return 0;
}
