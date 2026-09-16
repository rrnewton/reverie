#define _GNU_SOURCE
#include <fcntl.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
int main(void) {
    int fd[2]; if (pipe(fd)) return 1;
    int want = 2 << 20;                       /* 2 MiB: ABOVE this host's 1 MiB ceiling */
    int r = fcntl(fd[0], F_SETPIPE_SZ, want);
    printf("set(2MiB) rc=%d errno=%s\n", r, r < 0 ? strerror(errno) : "-");
    printf("resulting = %d\n", fcntl(fd[0], F_GETPIPE_SZ));
    return 0;
}
