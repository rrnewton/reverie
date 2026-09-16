/* Reports through a FILE, not stdout, because `record start --verify` does not
 * pass the guest's stdout through and a probe that reads stdout there measures
 * nothing. Conditional F_SETFL, like awk's, so run 1 and run 2 differ. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(void) {
    const char *out = getenv("FLAG_REPORT");
    int before = fcntl(2, F_GETFL);
    int attempted = 0, rc = 0;
    if (before >= 0 && !(before & O_APPEND)) {
        attempted = 1;
        rc = fcntl(2, F_SETFL, before | O_APPEND);
    }
    int after = fcntl(2, F_GETFL);
    if (out) {
        FILE *f = fopen(out, "a");
        if (f) {
            fprintf(f, "attempted=%d setfl_rc=%d guest_fd2_before=0x%x guest_fd2_after=0x%x\n",
                    attempted, rc, before, after);
            fclose(f);
        }
    }
    printf("42\n");
    return 0;
}
