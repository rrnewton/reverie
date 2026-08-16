#include <errno.h>
#include <stdio.h>
#include <sys/prctl.h>
int main(void) {
  errno = 0;
  int r = prctl(PR_GET_DUMPABLE, 0, 0, 0, 0);
  printf("PR_GET_DUMPABLE rc=%d errno=%d\n", r, errno);
  return 0;
}
