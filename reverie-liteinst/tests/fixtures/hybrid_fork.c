#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
  pid_t child = fork();
  if (child < 0) {
    return 10;
  }
  if (child == 0) {
    _exit(0);
  }
  int status = 0;
  return waitpid(child, &status, 0) == child ? 0 : 11;
}
