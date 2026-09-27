// Guest for the host-hybrid syscall-restart tests in hybrid.rs.
//
// Every syscall under test goes through one asm site, `restart_site`, so the
// first (subscribed, seccomp-trapped) call patches it and every later call
// reaches the tracer through the host-hybrid int3 trap. The per-site trap and
// hook counters are printed when the LiteInst preload provides them, and as
// "-" under plain ptrace, so one expected line format serves both backends.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <inttypes.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

// Must match hybrid.rs.
#define WARM_FD 0x7e56
#define MAGIC_FD 0x7e57
#define QUERY_FD 0x7e58
#define STRESS_SIGNALS 300

// long restart_site(long nr, long a0, long a1, long a2, long a3)
__asm__(".text\n"
        ".p2align 4\n"
        ".global restart_site\n"
        ".type restart_site,@function\n"
        "restart_site:\n"
        "mov %rdi, %rax\n"
        "mov %rsi, %rdi\n"
        "mov %rdx, %rsi\n"
        "mov %rcx, %rdx\n"
        "mov %r8, %r10\n"
        ".global restart_site_syscall\n"
        "restart_site_syscall:\n"
        "syscall\n"
        "nop\n"
        "nop\n"
        "nop\n"
        "ret\n"
        ".size restart_site, .-restart_site\n");

extern long restart_site(long nr, long a0, long a1, long a2, long a3);
extern unsigned char restart_site_syscall;

typedef uint64_t (*count_fn)(uint64_t);

static void print_site_counts(void) {
  count_fn traps = (count_fn)dlsym(RTLD_DEFAULT, "reverie_liteinst_site_trap_count");
  count_fn hooks = (count_fn)dlsym(RTLD_DEFAULT, "reverie_liteinst_site_hook_count");
  uint64_t site = (uint64_t)(uintptr_t)&restart_site_syscall;
  if (traps == NULL || hooks == NULL) {
    printf(" traps=- hooks=-\n");
  } else {
    printf(" traps=%" PRIu64 " hooks=%" PRIu64 "\n", traps(site), hooks(site));
  }
}

// The first call through the site: a subscribed read the Tool answers with 0.
static void warm_up(void) {
  long result = restart_site(SYS_read, WARM_FD, 0, 0, 0);
  if (result != 0) {
    fprintf(stderr, "warm-up read returned %ld\n", result);
    exit(30);
  }
}

static int64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

// A real kernel interruption: a 400 ms nanosleep through the site, with a
// SIGURG (ignored by default, but still reported to a ptracer) at 100 ms.
static int interrupted_sleep(void) {
  timer_t timer;
  struct sigevent event;
  memset(&event, 0, sizeof(event));
  event.sigev_notify = SIGEV_THREAD_ID;
  event.sigev_signo = SIGURG;
  event._sigev_un._tid = (pid_t)syscall(SYS_gettid);
  if (timer_create(CLOCK_MONOTONIC, &event, &timer) != 0) {
    perror("timer_create");
    return 31;
  }
  struct itimerspec when;
  memset(&when, 0, sizeof(when));
  when.it_value.tv_nsec = 100 * 1000 * 1000;
  warm_up();
  if (timer_settime(timer, 0, &when, NULL) != 0) {
    perror("timer_settime");
    return 32;
  }
  struct timespec request = {.tv_sec = 0, .tv_nsec = 400 * 1000 * 1000};
  struct timespec remaining = {0, 0};
  int64_t start = now_ns();
  long result = restart_site(SYS_nanosleep, (long)&request, (long)&remaining, 0, 0);
  int64_t elapsed = now_ns() - start;
  printf("sleep-result=%ld slept-enough=%d", result, elapsed >= 400 * 1000 * 1000);
  print_site_counts();
  return 0;
}

static int pipe_write_fd;

static void *late_writer(void *unused) {
  (void)unused;
  struct timespec pause = {.tv_sec = 0, .tv_nsec = 400 * 1000 * 1000};
  nanosleep(&pause, NULL);
  if (write(pipe_write_fd, "x", 1) != 1) {
    exit(41);
  }
  return NULL;
}

// A real kernel ERESTARTSYS: a blocking pipe readv through the site (the Tool
// subscribes read, not readv), with a SIGURG (ignored by default, but still
// reported to a ptracer) at 100 ms and the data written at 400 ms. The readv
// must restart and return the byte, never -512 or EINTR.
static int interrupted_readv(void) {
  int fds[2];
  if (pipe(fds) != 0) {
    perror("pipe");
    return 40;
  }
  pipe_write_fd = fds[1];
  timer_t timer;
  struct sigevent event;
  memset(&event, 0, sizeof(event));
  event.sigev_notify = SIGEV_THREAD_ID;
  event.sigev_signo = SIGURG;
  event._sigev_un._tid = (pid_t)syscall(SYS_gettid);
  if (timer_create(CLOCK_MONOTONIC, &event, &timer) != 0) {
    perror("timer_create");
    return 31;
  }
  struct itimerspec when;
  memset(&when, 0, sizeof(when));
  when.it_value.tv_nsec = 100 * 1000 * 1000;
  warm_up();
  pthread_t writer;
  if (pthread_create(&writer, NULL, late_writer, NULL) != 0) {
    return 42;
  }
  if (timer_settime(timer, 0, &when, NULL) != 0) {
    perror("timer_settime");
    return 32;
  }
  char byte = 0;
  struct iovec iov = {.iov_base = &byte, .iov_len = 1};
  long result = restart_site(SYS_readv, fds[0], (long)&iov, 1, 0);
  if (pthread_join(writer, NULL) != 0) {
    return 43;
  }
  printf("readv-result=%ld byte=%c", result, byte == 0 ? '0' : byte);
  print_site_counts();
  return 0;
}

// A completed syscall with a signal pending on return: SIGURG (ignored by
// default, but still reported to a ptracer) that the thread sends itself. A
// re-executed tgkill would make the Tool see a second SIGURG.
static int completed_with_pending_signal(void) {
  warm_up();
  long result = restart_site(SYS_tgkill, getpid(), syscall(SYS_gettid), SIGURG, 0);
  printf("tgkill-result=%ld", result);
  print_site_counts();
  return 0;
}

static pid_t stress_target;
static volatile int stress_done;

// The number of SIGURG signal stops the Tool has seen so far.
static long tool_signal_count(void) { return syscall(SYS_read, QUERY_FD, 0, 0); }

static void *stress_sender(void *unused) {
  (void)unused;
  for (long i = 0; i < STRESS_SIGNALS; ++i) {
    if (syscall(SYS_tgkill, getpid(), stress_target, SIGURG) != 0) {
      perror("tgkill");
      exit(34);
    }
    // Standard signals coalesce: wait until the Tool has seen this one before
    // sending the next, so every signal must be reported exactly once.
    int64_t deadline = now_ns() + (int64_t)10 * 1000 * 1000 * 1000;
    while (tool_signal_count() < i + 1) {
      if (now_ns() > deadline) {
        fprintf(stderr, "signal %ld never reached the Tool\n", i);
        exit(35);
      }
      struct timespec pause = {.tv_sec = 0, .tv_nsec = 20 * 1000};
      nanosleep(&pause, NULL);
    }
  }
  __atomic_store_n(&stress_done, 1, __ATOMIC_SEQ_CST);
  return NULL;
}

// SIGURG races the site's unsubscribed syscalls: it can arrive before the
// private-page step (never run), during a sleep (interrupted), or as the
// syscall completes. Every result must be the real one, and every signal must
// reach the Tool exactly once.
static int stress(void) {
  warm_up();
  stress_target = (pid_t)syscall(SYS_gettid);
  long parent = getppid();
  pthread_t sender;
  if (pthread_create(&sender, NULL, stress_sender, NULL) != 0) {
    return 36;
  }
  long iterations = 0;
  long bad = 0;
  while (!__atomic_load_n(&stress_done, __ATOMIC_SEQ_CST)) {
    if (restart_site(SYS_getppid, 0, 0, 0, 0) != parent) {
      ++bad;
    }
    struct timespec pause = {.tv_sec = 0, .tv_nsec = 50 * 1000};
    if (restart_site(SYS_nanosleep, (long)&pause, 0, 0, 0) != 0) {
      ++bad;
    }
    ++iterations;
  }
  if (pthread_join(sender, NULL) != 0) {
    return 37;
  }
  printf("stress-bad=%ld ran=%d tool-signals=%ld", bad, iterations > 0,
         tool_signal_count());
  print_site_counts();
  return 0;
}

// The guest's own seccomp filter traps getppid (SECCOMP_RET_TRAP), so the
// kernel queues a synchronous SIGSYS at syscall entry, ahead of the
// single-step report the tracer's private-page step queues at exit.
static int seccomp_trap(void) {
  warm_up();
  struct sock_filter filter[] = {
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getppid, 0, 1),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
  };
  struct sock_fprog program = {
      .len = sizeof(filter) / sizeof(filter[0]),
      .filter = filter,
  };
  if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 ||
      prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program) != 0) {
    perror("seccomp");
    return 39;
  }
  long result = restart_site(SYS_getppid, 0, 0, 0, 0);
  printf("getppid-result=%ld", result);
  print_site_counts();
  return 0;
}

static void guest_handler(int signo) { (void)signo; }

int main(int argc, char **argv) {
  if (argc != 2) {
    return 2;
  }
  const char *mode = argv[1];
  if (strcmp(mode, "read") == 0 || strcmp(mode, "handler") == 0) {
    if (strcmp(mode, "handler") == 0) {
      struct sigaction action;
      memset(&action, 0, sizeof(action));
      action.sa_handler = guest_handler;
      if (sigaction(SIGUSR1, &action, NULL) != 0) {
        return 38;
      }
    }
    char byte = 0;
    warm_up();
    long result = restart_site(SYS_read, MAGIC_FD, (long)&byte, 1, 0);
    printf("read-result=%ld", result);
    print_site_counts();
    return 0;
  }
  if (strcmp(mode, "sleep") == 0) {
    return interrupted_sleep();
  }
  if (strcmp(mode, "readv") == 0) {
    return interrupted_readv();
  }
  if (strcmp(mode, "tgkill") == 0) {
    return completed_with_pending_signal();
  }
  if (strcmp(mode, "stress") == 0) {
    return stress();
  }
  if (strcmp(mode, "seccomp") == 0) {
    return seccomp_trap();
  }
  return 3;
}
