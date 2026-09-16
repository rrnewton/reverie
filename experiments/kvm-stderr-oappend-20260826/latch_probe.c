/* O_NONBLOCK first, O_APPEND second -- the order that latches. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
int main(void) {
    int f = fcntl(2, F_GETFL);
    fcntl(2, F_SETFL, f | O_NONBLOCK);          /* forwarded by design */
    fcntl(2, F_SETFL, f | O_NONBLOCK | O_APPEND); /* must stay contained */
    int g = fcntl(2, F_GETFL);
    printf("guest_append=%d guest_nonblock=%d\n",
           (g & O_APPEND) != 0, (g & O_NONBLOCK) != 0);
    return 0;
}
