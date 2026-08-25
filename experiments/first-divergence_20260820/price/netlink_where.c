/* Which memory-hash flag actually SEES a netlink reply, as a function of where
 * the receive buffer lives?
 *
 *   argv[1] = stack | heap | bss | mmap
 *
 * Same netlink RTM_GETLINK exchange in every mode; only the buffer's home
 * changes. The program prints ONLY the byte count, never the contents, so any
 * divergence a run reports must come from a memory hash and not from output.
 *
 * The real case this models: the demos' launcher calls getifaddrs() and its
 * recvmsg buffer is at 0x7fffffff8370 -- inside the [stack] mapping.
 */
#include <linux/netlink.h>
#include <linux/rtnetlink.h>
#include <malloc.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
#include <sys/mman.h>

#define BUFSZ 65536
static unsigned char bss_buf[BUFSZ];

int main(int argc, char **argv) {
    const char *where = argc > 1 ? argv[1] : "stack";
    unsigned char stack_buf[BUFSZ];
    unsigned char *buf;
    if (!strcmp(where, "heap")) {
        /* force brk so the allocation is the [heap] mapping, not an mmap */
        mallopt(M_MMAP_THRESHOLD, 1 << 30);
        mallopt(M_MMAP_MAX, 0);
        buf = malloc(BUFSZ);
        memset(buf, 0, BUFSZ);
    } else if (!strcmp(where, "mmap")) {
        /* An anonymous mmap -- where glibc puts any malloc above M_MMAP_THRESHOLD
         * (128 KiB by default), and where most runtimes put large buffers. It is
         * neither [heap] nor [stack]. */
        buf = mmap(NULL, BUFSZ, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        memset(buf, 0, BUFSZ);
    } else if (!strcmp(where, "bss")) {
        buf = bss_buf;
    } else {
        buf = stack_buf;
        memset(buf, 0, BUFSZ);
    }

    int fd = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_ROUTE);
    struct sockaddr_nl sa; memset(&sa, 0, sizeof sa); sa.nl_family = AF_NETLINK;
    bind(fd, (struct sockaddr *)&sa, sizeof sa);
    struct { struct nlmsghdr nh; struct rtgenmsg g; } req;
    memset(&req, 0, sizeof req);
    req.nh.nlmsg_len = sizeof req;
    req.nh.nlmsg_type = RTM_GETLINK;
    req.nh.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
    req.nh.nlmsg_seq = 1;
    req.g.rtgen_family = AF_PACKET;
    struct sockaddr_nl dst; memset(&dst, 0, sizeof dst); dst.nl_family = AF_NETLINK;
    sendto(fd, &req, sizeof req, 0, (struct sockaddr *)&dst, sizeof dst);

    long total = 0;
    for (int i = 0; i < 4; i++) {
        struct iovec iov = { buf, BUFSZ };
        struct msghdr msg; memset(&msg, 0, sizeof msg);
        msg.msg_iov = &iov; msg.msg_iovlen = 1;
        ssize_t n = recvmsg(fd, &msg, 0);
        if (n <= 0) break;
        total += n;
        struct nlmsghdr *nh = (struct nlmsghdr *)buf;
        if (nh->nlmsg_type == NLMSG_DONE) break;
    }
    close(fd);
    /* length only -- never the contents */
    printf("where=%s total_bytes=%ld buf=%p\n", where, total, (void *)buf);
    return 0;
}
