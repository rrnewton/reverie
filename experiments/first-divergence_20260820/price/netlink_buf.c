/* Does a syscall-boundary hash of recvmsg's OUTPUT BUFFER catch the divergence
 * we already found?
 *
 * This performs the same RTM_GETLINK dump the demos' python launcher performs
 * via getifaddrs(), receives it with recvmsg(), and hashes EXACTLY the bytes
 * recvmsg wrote -- msg_iov[0].iov_base for the returned length. That is
 * precisely the buffer a targeted check would hash, so the printed digest is
 * what such a check would record.
 *
 * FNV-1a rather than sha256 only to avoid a link dependency; any divergence in
 * the bytes changes it.
 */
#include <linux/netlink.h>
#include <linux/rtnetlink.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static unsigned long long fnv1a(const unsigned char *p, size_t n) {
    unsigned long long h = 1469598103934665603ULL;
    for (size_t i = 0; i < n; i++) { h ^= p[i]; h *= 1099511628211ULL; }
    return h;
}

int main(void) {
    int fd = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_ROUTE);
    if (fd < 0) { perror("socket"); return 1; }
    struct sockaddr_nl sa; memset(&sa, 0, sizeof sa); sa.nl_family = AF_NETLINK;
    if (bind(fd, (struct sockaddr *)&sa, sizeof sa) < 0) { perror("bind"); return 1; }

    struct { struct nlmsghdr nh; struct rtgenmsg g; } req;
    memset(&req, 0, sizeof req);
    req.nh.nlmsg_len = sizeof req;
    req.nh.nlmsg_type = RTM_GETLINK;
    req.nh.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
    req.nh.nlmsg_seq = 1;
    req.g.rtgen_family = AF_PACKET;
    struct sockaddr_nl dst; memset(&dst, 0, sizeof dst); dst.nl_family = AF_NETLINK;
    if (sendto(fd, &req, sizeof req, 0, (struct sockaddr *)&dst, sizeof dst) < 0) {
        perror("sendto"); return 1;
    }

    static unsigned char buf[65536];
    for (int i = 0; i < 4; i++) {
        struct iovec iov = { buf, sizeof buf };
        struct msghdr msg; memset(&msg, 0, sizeof msg);
        msg.msg_iov = &iov; msg.msg_iovlen = 1;
        ssize_t n = recvmsg(fd, &msg, 0);
        if (n <= 0) break;
        /* Exactly the bytes recvmsg wrote -- what a targeted check would hash. */
        printf("recvmsg #%d len=%zd buffer_hash=%016llx\n", i, n, fnv1a(buf, (size_t)n));
        struct nlmsghdr *nh = (struct nlmsghdr *)buf;
        if (nh->nlmsg_type == NLMSG_DONE) break;
    }
    close(fd);
    return 0;
}
