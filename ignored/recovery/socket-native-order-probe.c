#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <unistd.h>

static void print_getopt(const char *label, long fd, int option, void *value,
                         socklen_t *length) {
  errno = 0;
  long result = syscall(SYS_getsockopt, fd, SOL_SOCKET, option, value, length);
  int saved = errno;
  printf("getopt %-26s result=%ld errno=%d", label, result, saved);
  if ((uintptr_t)length > 4096) printf(" length=%u", *length);
  if ((uintptr_t)value > 4096) {
    unsigned char *bytes = value;
    printf(" bytes=%02x%02x%02x%02x", bytes[0], bytes[1], bytes[2], bytes[3]);
  }
  putchar('\n');
}

static void print_name(const char *label, long number, long fd, void *address,
                       socklen_t *length) {
  errno = 0;
  long result = syscall(number, fd, address, length);
  int saved = errno;
  printf("name   %-26s result=%ld errno=%d", label, result, saved);
  if ((uintptr_t)length > 4096) printf(" length=%u", *length);
  if ((uintptr_t)address > 4096) {
    unsigned char *bytes = address;
    printf(" bytes=%02x%02x%02x%02x", bytes[0], bytes[1], bytes[2], bytes[3]);
  }
  putchar('\n');
}

int main(void) {
  int inet = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
  int ordinary = open("/dev/null", O_RDONLY | O_CLOEXEC);
  if (inet < 0 || ordinary < 0 || listen(inet, 1) != 0) return 90;

  int enabled = 1;
  if (setsockopt(inet, SOL_SOCKET, SO_REUSEADDR, &enabled, sizeof(enabled)) ||
      setsockopt(inet, SOL_SOCKET, SO_KEEPALIVE, &enabled, sizeof(enabled)) ||
      setsockopt(inet, SOL_SOCKET, SO_BROADCAST, &enabled, sizeof(enabled))) return 91;

  for (int option_index = 0; option_index < 6; ++option_index) {
    const int options[] = {SO_TYPE, SO_DOMAIN, SO_ACCEPTCONN,
                           SO_REUSEADDR, SO_KEEPALIVE, SO_BROADCAST};
    const char *names[] = {"SO_TYPE", "SO_DOMAIN", "SO_ACCEPTCONN",
                           "SO_REUSEADDR", "SO_KEEPALIVE", "SO_BROADCAST"};
    int value = 0x5a5a5a5a;
    socklen_t length = sizeof(value);
    print_getopt(names[option_index], inet, options[option_index], &value, &length);
  }

  for (socklen_t capacity = 0; capacity <= 8; ++capacity) {
    unsigned char value[8];
    memset(value, 0xa5, sizeof(value));
    socklen_t length = capacity;
    char label[32];
    snprintf(label, sizeof(label), "SO_TYPE length %u", capacity);
    print_getopt(label, inet, SO_TYPE, capacity == 0 ? NULL : value, &length);
  }

  unsigned char value[8];
  memset(value, 0xa5, sizeof(value));
  socklen_t huge = UINT_MAX;
  print_getopt("SO_TYPE UINT_MAX", inet, SO_TYPE, value, &huge);
  socklen_t full = sizeof(int);
  print_getopt("bad fd, bad pointers", -1, SO_TYPE, (void *)1, (socklen_t *)1);
  print_getopt("file, bad pointers", ordinary, SO_TYPE, (void *)1, (socklen_t *)1);
  full = sizeof(int);
  print_getopt("unsupported, null value", inet, SO_ERROR + 0x4000, NULL, &full);
  print_getopt("unsupported, bad value", inet, SO_ERROR + 0x4000, (void *)1, &full);
  print_getopt("unsupported, bad length", inet, SO_ERROR + 0x4000, value,
               (socklen_t *)1);
  print_getopt("SO_TYPE bad value", inet, SO_TYPE, (void *)1, &full);
  print_getopt("SO_TYPE bad length", inet, SO_TYPE, value, (socklen_t *)1);

  uint32_t overlap = sizeof(overlap);
  print_getopt("SO_DOMAIN overlap", inet, SO_DOMAIN, &overlap,
               (socklen_t *)&overlap);

  int pair[2];
  if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, pair) != 0) return 92;
  for (int peer = 0; peer <= 1; ++peer) {
    long number = peer ? SYS_getpeername : SYS_getsockname;
    const char *kind = peer ? "getpeername" : "getsockname";
    for (socklen_t capacity = 0; capacity <= 4; ++capacity) {
      unsigned char address[8];
      memset(address, 0xa5, sizeof(address));
      socklen_t length = capacity;
      char label[40];
      snprintf(label, sizeof(label), "%s length %u", kind, capacity);
      print_name(label, number, pair[0], address, &length);
    }
  }

  unsigned char address[8];
  memset(address, 0xa5, sizeof(address));
  socklen_t zero = 0;
  socklen_t address_capacity = sizeof(address);
  print_name("getsockname null addr len0", SYS_getsockname, pair[0], NULL, &zero);
  address_capacity = sizeof(address);
  print_name("getsockname null addr full", SYS_getsockname, pair[0], NULL,
             &address_capacity);
  socklen_t huge_name = UINT_MAX;
  print_name("getsockname UINT_MAX", SYS_getsockname, pair[0], address,
             &huge_name);
  uint32_t name_overlap = sizeof(name_overlap);
  print_name("getsockname overlap", SYS_getsockname, pair[0], &name_overlap,
             (socklen_t *)&name_overlap);
  print_name("getsockname bad length", SYS_getsockname, pair[0], address,
             (socklen_t *)1);
  print_name("getsockname bad fd ptrs", SYS_getsockname, -1, (void *)1,
             (socklen_t *)1);
  print_name("getsockname file ptrs", SYS_getsockname, ordinary, (void *)1,
             (socklen_t *)1);

  int disconnected = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (disconnected < 0) return 93;
  print_name("peer disconnected ptrs", SYS_getpeername, disconnected, (void *)1,
             (socklen_t *)1);

  memset(address, 0xa5, sizeof(address));
  print_name("peer noisy fd", SYS_getpeername,
             (long)(((uint64_t)0x5a5aa5a5 << 32) | (uint32_t)pair[0]), address,
             &address_capacity);

  close(disconnected);
  close(pair[0]);
  close(pair[1]);
  close(ordinary);
  close(inet);
  return 0;
}
