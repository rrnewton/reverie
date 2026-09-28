#include <assert.h>
#include <errno.h>
#include <stddef.h>
#include <sys/types.h>
#include <unistd.h>
extern ssize_t late_probe(unsigned char *, size_t, unsigned);
int main(void) {
  unsigned char b[4] = {0, 1, 2, 3};
  errno = 0;
  assert(late_probe(b, 4, 0x1357) == 137);
  assert(b[0] == 0xea && b[1] == 1 && b[2] == 2 && b[3] == 3);
  assert(errno == EDOM);
  assert(write(1, "CLIENT_OK\n", 10) == 10);
  return 0;
}
