// A precise timer requested at the last Tool-observable syscall must fire
// during the following spin, even when a tracer-internal LiteInst stop falls
// between the request and the timer's own stop.
#define _GNU_SOURCE
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

static unsigned char page[4096] __attribute__((aligned(4096)));

// No syscall, and at least one conditional branch per iteration.
static uint64_t spin(uint64_t iterations) {
  volatile uint64_t counter = 0;
  for (uint64_t i = 0; i < iterations; i++) {
    counter += i;
  }
  return counter;
}

static const char *mode_of(int argc, char **argv) {
  return argc > 1 ? argv[1] : "";
}

// The executable's preinit functions run before any shared object's
// initializer, so this getpid precedes the LiteInst runtime's Begin trap.
static void before_runtime(int argc, char **argv, char **envp) {
  (void)envp;
  if (strcmp(mode_of(argc, argv), "begin") == 0 && getpid() <= 0) {
    _exit(14);
  }
}

__attribute__((used, section(".preinit_array"))) static void (*const preinit)(
    int, char **, char **) = before_runtime;

int main(int argc, char **argv) {
  const char *mode = mode_of(argc, argv);
  if (strcmp(mode, "begin") == 0) {
    // The Tool's last event was the getpid in before_runtime. The runtime's
    // Begin and Ready traps and its controller-only mapping syscalls follow.
  } else if (strcmp(mode, "handshake") == 0) {
    // The last Tool-observable syscall before this spin is the runtime's own,
    // issued before its Ready handshake trap.
  } else if (strcmp(mode, "mapping") == 0) {
    // getpid is the Tool's event; mprotect is traced only for the
    // controller's patched-site provenance.
    if (getpid() <= 0) {
      return 10;
    }
    if (mprotect(page, sizeof(page), PROT_READ | PROT_WRITE) != 0) {
      return 11;
    }
  } else if (strcmp(mode, "unsubscribed") == 0) {
    // The first call's seccomp stop patches the syscall(2) wrapper's site. The
    // second reaches the Tool through that site; gettid then arrives there
    // unsubscribed.
    for (int i = 0; i < 2; i++) {
      if (syscall(SYS_getpid) <= 0) {
        return 12;
      }
    }
    if (syscall(SYS_gettid) <= 0) {
      return 13;
    }
  } else {
    return 2;
  }
  // Exit without another syscall, so the spin is the last thing a pending
  // timer can fire in.
  return spin(40000000) != 0 ? 0 : 3;
}
