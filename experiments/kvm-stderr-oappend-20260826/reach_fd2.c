#define _GNU_SOURCE
/* From inside the guest, attempt every OTHER way of touching stderr that is not
 * fcntl(F_SETFL), and report what the kernel (or hermit) said. The harness
 * separately measures whether the SUPERVISOR's descriptor changed. */
#include <fcntl.h>
#ifndef F_SETPIPE_SZ
#define F_SETPIPE_SZ 1031
#endif
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/stat.h>

static void try_(const char *name, long rc) {
    printf("%-18s rc=%ld%s%s\n", name, rc,
           rc < 0 ? " errno=" : "", rc < 0 ? strerror(errno) : "");
}

int main(void) {
    try_("lseek_set_100",  (long)lseek(2, 100, SEEK_SET));
    try_("ftruncate_0",    (long)ftruncate(2, 0));
    try_("fchmod_0600",    (long)fchmod(2, 0600));
    try_("F_SETOWN_self",  (long)fcntl(2, F_SETOWN, getpid()));
    try_("F_SETPIPE_SZ",   (long)fcntl(2, F_SETPIPE_SZ, 1 << 20));
    struct flock fl = {.l_type = F_WRLCK, .l_whence = SEEK_SET,
                       .l_start = 0, .l_len = 1};
    try_("F_SETLK_wrlck",  (long)fcntl(2, F_SETLK, &fl));
    return 0;
}
