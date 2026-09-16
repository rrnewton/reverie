#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
int main(void) {
    int f = fcntl(0, F_GETFL);
    printf("getfl=%d\n", f); fflush(stdout);
    int r = fcntl(0, F_SETFL, f | O_NONBLOCK);
    printf("setfl=%d errno=%s\n", r, r < 0 ? strerror(errno) : "-"); fflush(stdout);
    char b = 0;
    ssize_t n = recv(0, &b, 1, 0);
    printf("recv=%zd errno=%s byte=%d\n", n, n < 0 ? strerror(errno) : "-", (int)b);
    fflush(stdout);
    return 0;
}
