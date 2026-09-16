#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

typedef uint64_t (*site_counter_fn)(uint64_t);
typedef uint64_t (*syscall_counter_fn)(int64_t);
typedef uint64_t (*total_counter_fn)(void);

static site_counter_fn trap_count;
static site_counter_fn hook_count;
static syscall_counter_fn syscall_count;
static total_counter_fn total_count;
static uintptr_t syscall_site;

struct counters {
    uint64_t traps;
    uint64_t hooks;
    uint64_t fallbacks;
    uint64_t total;
};

static struct counters snapshot(void) {
    if (trap_count == NULL) {
        return (struct counters){0, 0, 0, 0};
    }
    return (struct counters){trap_count(syscall_site), hook_count(syscall_site),
        syscall_count(SYS_ftruncate), total_count()};
}

static int check_file(const char *kind, int descriptor, off_t requested) {
    struct stat before_stat = {0};
    if (descriptor < 0 || fstat(descriptor, &before_stat) != 0) {
        perror("fixture setup");
        return 2;
    }
    unsigned char original[8];
    memcpy(original, (void *)syscall_site, sizeof original);
    struct counters before = snapshot();
    errno = 0;
    int result = ftruncate(descriptor, requested);
    int observed_errno = errno;
    struct counters after = snapshot();
    int unchanged = memcmp(original, (void *)syscall_site, sizeof original) == 0;
    struct stat after_stat = {0};
    errno = 0;
    int stat_result = fstat(descriptor, &after_stat);
    int stat_errno = errno;
    printf("{\"kind\":\"%s\",\"fd\":%d,\"requested\":%jd,"
           "\"before_size\":%jd,\"result\":%d,\"errno\":%d,"
           "\"is_eopnotsupp\":%s,\"stat_result\":%d,\"stat_errno\":%d,"
           "\"after_size\":%jd,\"site_bytes_unchanged\":%s,"
           "\"trap_before\":%" PRIu64 ",\"trap_after\":%" PRIu64 ","
           "\"hook_before\":%" PRIu64 ",\"hook_after\":%" PRIu64 ","
           "\"fallback_before\":%" PRIu64 ",\"fallback_after\":%" PRIu64 ","
           "\"total_before\":%" PRIu64 ",\"total_after\":%" PRIu64 "}\n",
           kind, descriptor, (intmax_t)requested, (intmax_t)before_stat.st_size,
           result, observed_errno, observed_errno == EOPNOTSUPP ? "true" : "false",
           stat_result, stat_errno, (intmax_t)after_stat.st_size,
           unchanged ? "true" : "false", before.traps, after.traps,
           before.hooks, after.hooks, before.fallbacks, after.fallbacks,
           before.total, after.total);
    if (close(descriptor) != 0 || stat_result != 0) {
        return 2;
    }
    return result == 0 && after_stat.st_size == requested ? 0 : 1;
}

int main(int argc, char **argv) {
    if (argc != 2 || (strcmp(argv[1], "native") != 0 && strcmp(argv[1], "typed") != 0)) {
        return 2;
    }
    trap_count = (site_counter_fn)dlsym(RTLD_DEFAULT, "reverie_liteinst_site_trap_count");
    hook_count = (site_counter_fn)dlsym(RTLD_DEFAULT, "reverie_liteinst_site_hook_count");
    syscall_count = (syscall_counter_fn)dlsym(RTLD_DEFAULT, "reverie_liteinst_fallback_syscall_count");
    total_count = (total_counter_fn)dlsym(RTLD_DEFAULT, "reverie_liteinst_fallback_dispatch_count");
    int available = trap_count != NULL && hook_count != NULL && syscall_count != NULL && total_count != NULL;
    if ((strcmp(argv[1], "typed") == 0) != available ||
        (!available && (trap_count || hook_count || syscall_count || total_count))) {
        fputs("unexpected runtime counter availability\n", stderr);
        return 2;
    }
    unsigned char *function = dlsym(RTLD_DEFAULT, "ftruncate");
    Dl_info library;
    if (function == NULL || dladdr(function, &library) == 0) {
        return 2;
    }
    for (size_t offset = 0; offset + 1 < 32; ++offset) {
        if (function[offset] == 0x0f && function[offset + 1] == 0x05) {
            syscall_site = (uintptr_t)(function + offset);
            break;
        }
    }
    if (syscall_site == 0) {
        fputs("libc ftruncate syscall site not located\n", stderr);
        return 2;
    }
    printf("{\"mode\":\"%s\",\"counters_available\":%s,\"libc\":\"%s\","
           "\"function_offset\":\"0x%" PRIxPTR "\",\"syscall_offset\":\"0x%" PRIxPTR "\"}\n",
           argv[1], available ? "true" : "false", library.dli_fname,
           (uintptr_t)function - (uintptr_t)library.dli_fbase,
           syscall_site - (uintptr_t)library.dli_fbase);
    int anonymous = open("/tmp", O_TMPFILE | O_RDWR, 0600);
    if (anonymous < 0 || write(anonymous, "hello\n", 6) != 6) {
        perror("anonymous file setup");
        return 2;
    }
    int anonymous_result = check_file("anonymous", anonymous, 10);
    char path[] = "/tmp/ftruncate-counter-XXXXXX";
    int regular = mkstemp(path);
    int regular_result = check_file("regular", regular, 4096);
    if (regular >= 0 && unlink(path) != 0) {
        return 2;
    }
    return anonymous_result > regular_result ? anonymous_result : regular_result;
}
