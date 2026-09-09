#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

static void probe(const char *name, int dfd, const char *path,
                  unsigned mode, unsigned flags) {
    errno = 0;
    long rc = syscall(SYS_fchmodat2, dfd, path, mode, flags);
    printf("%s rc=%ld errno=%d\n", name, rc, errno);
}

int main(void) {
    char dir[] = "/tmp/reverie-fchmodat2.XXXXXX";
    if (!mkdtemp(dir)) return 1;
    char file[256], link[256], missing[256];
    snprintf(file, sizeof(file), "%s/file", dir);
    snprintf(link, sizeof(link), "%s/link", dir);
    snprintf(missing, sizeof(missing), "%s/missing", dir);
    int createfd = open(file, O_CREAT | O_RDWR, 0600);
    if (createfd < 0) return 2;
    close(createfd);
    if (symlink("file", link) != 0) return 3;
    int filefd = open(file, O_PATH | O_CLOEXEC);
    int linkfd = open(link, O_PATH | O_NOFOLLOW | O_CLOEXEC);
    if (filefd < 0 || linkfd < 0) return 4;

    probe("regular-0", AT_FDCWD, file, 0640, 0);
    probe("regular-nofollow", AT_FDCWD, file, 0600, AT_SYMLINK_NOFOLLOW);
    probe("symlink-follow", AT_FDCWD, link, 0644, 0);
    probe("symlink-nofollow", AT_FDCWD, link, 0600, AT_SYMLINK_NOFOLLOW);
    probe("empty-filefd", filefd, "", 0640, AT_EMPTY_PATH);
    probe("empty-filefd-both", filefd, "", 0600,
          AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW);
    probe("empty-linkfd", linkfd, "", 0644, AT_EMPTY_PATH);
    probe("empty-linkfd-both", linkfd, "", 0600,
          AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW);
    probe("invalid-flags-bad-pointer", -1, (const char *)1, 0600, 0xffff);
    probe("valid-flags-bad-pointer-bad-fd", -1, (const char *)1, 0600, 0);
    probe("empty-without-flag-bad-fd", -1, "", 0600, 0);
    probe("empty-with-flag-bad-fd", -1, "", 0600, AT_EMPTY_PATH);
    probe("relative-missing-bad-fd", -1, "missing", 0600, 0);
    probe("missing", AT_FDCWD, missing, 0600, 0);

    close(filefd);
    close(linkfd);
    unlink(link);
    unlink(file);
    rmdir(dir);
    return 0;
}
