/* The O_APPEND shape again, through a door #2694 did not close: run 1 changes
 * the supervisor's stderr mode, run 2 observes the change. */
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>
int main(void) {
    struct stat st;
    if (fstat(2, &st) != 0) { perror("fstat"); return 1; }
    printf("stderr_mode=%o\n", st.st_mode & 07777);
    if ((st.st_mode & 07777) != 0600) {
        if (fchmod(2, 0600) != 0) { perror("fchmod"); return 1; }
    }
    return 0;
}
