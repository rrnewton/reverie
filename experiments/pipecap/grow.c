#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
int main(void) {
    int fd[2]; if (pipe(fd)) return 1;
    printf("initial   = %d\n", fcntl(fd[0], F_GETPIPE_SZ));
    int r = fcntl(fd[0], F_SETPIPE_SZ, 1 << 20);   /* 1 MiB: allowed only if host ceiling permits */
    printf("set(1MiB) = %d\n", r);
    printf("after     = %d\n", fcntl(fd[0], F_GETPIPE_SZ));
    return 0;
}
