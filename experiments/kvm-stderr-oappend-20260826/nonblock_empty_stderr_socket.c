/* No data is available, so a correct O_NONBLOCK recv must return EAGAIN
 * immediately. Anything that blocks here has lost the guest's flag. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
int main(void) {
    int f = fcntl(2, F_GETFL);
    if (fcntl(2, F_SETFL, f | O_NONBLOCK) < 0) { perror("F_SETFL"); return 1; }
    char b = 0;
    ssize_t n = recv(2, &b, 1, 0);
    printf("recv=%zd errno=%s\n", n, n < 0 ? strerror(errno) : "-");
    fflush(stdout);
    return 0;
}
