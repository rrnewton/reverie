#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

static long call_preadv(int fd, struct iovec *iov, int count) {
    errno = 0;
    long result = syscall(SYS_preadv, fd, iov, count, 0UL, 0UL);
    printf("result=%ld errno=%d (%s)\n", result, errno, strerror(errno));
    return result;
}

int main(void) {
    const char *path = "/boot/config-7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf";
    int fd = open(path, O_RDONLY | O_DIRECT | O_CLOEXEC);
    if (fd < 0) { perror("open"); return 1; }
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    unsigned char *mapping = mmap(NULL, 2 * page, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mapping == MAP_FAILED) { perror("mmap"); return 1; }
    if (mprotect(mapping + page, page, PROT_NONE) != 0) { perror("mprotect"); return 1; }

    struct iovec original[] = {{mapping, 2 * page}};
    printf("original one-vector protected suffix: ");
    call_preadv(fd, original, 1);

    struct iovec translated[] = {{mapping, page}, {(void *)(uintptr_t)1, page}};
    printf("translated split/dangling suffix: ");
    call_preadv(fd, translated, 2);

    struct iovec split_aligned[] = {{mapping, page}, {(void *)(uintptr_t)page, page}};
    printf("split/aligned-invalid suffix: ");
    call_preadv(fd, split_aligned, 2);

    return 0;
}
