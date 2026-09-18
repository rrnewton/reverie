#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/user.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef P_PIDFD
#define P_PIDFD 3
#endif

enum timing {
  CONT_BEFORE_D_RESUME,
  CONT_AFTER_D_RESUME,
  CONT_AFTER_G,
};

struct probe_peeksiginfo_args {
  uint64_t off;
  uint32_t flags;
  int32_t nr;
};

struct shared_state {
  _Atomic uint32_t phase;
  _Atomic uintptr_t trap_rip;
};

static uint32_t shared_phase(const struct shared_state *shared) {
  return atomic_load_explicit(&shared->phase, memory_order_seq_cst);
}

static int pending_sigcont(pid_t child, uint32_t flags, siginfo_t *found) {
  struct probe_peeksiginfo_args args = {
      .off = 0,
      .flags = flags,
      .nr = 16,
  };
  siginfo_t pending[16];
  long count = ptrace(PTRACE_PEEKSIGINFO, child, &args, pending);
  if (count < 0) {
    perror("PTRACE_PEEKSIGINFO");
    return -1;
  }
  int matches = 0;
  for (long index = 0; index < count; ++index) {
    if (pending[index].si_signo == SIGCONT) {
      if (found != NULL) {
        *found = pending[index];
      }
      ++matches;
    }
  }
  return matches;
}

static long long monotonic_millis(void) {
  struct timespec now;
  if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
    perror("clock_gettime");
    exit(2);
  }
  return (long long)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

static int next_notice(int pidfd, siginfo_t *info, int timeout_ms) {
  const long long deadline = monotonic_millis() + timeout_ms;
  do {
    memset(info, 0, sizeof(*info));
    if (waitid(P_PIDFD, (id_t)pidfd, info,
               WEXITED | WSTOPPED | WCONTINUED | WNOHANG | __WALL) != 0) {
      if (errno == EINTR) {
        continue;
      }
      perror("waitid(P_PIDFD)");
      return -1;
    }
    if (info->si_pid != 0) {
      return 1;
    }
    usleep(1000);
  } while (monotonic_millis() < deadline);
  return 0;
}

static int peek_exact_group_stop(int pidfd, int timeout_ms) {
  const long long deadline = monotonic_millis() + timeout_ms;
  do {
    siginfo_t info;
    memset(&info, 0, sizeof(info));
    if (waitid(P_PIDFD, (id_t)pidfd, &info,
               WSTOPPED | WNOHANG | WNOWAIT | __WALL) != 0) {
      if (errno == EINTR) {
        continue;
      }
      perror("peek group stop");
      return -1;
    }
    if (info.si_pid != 0) {
      if (info.si_code == CLD_TRAPPED && info.si_status == SIGSTOP) {
        return 1;
      }
      fprintf(stderr, "peeked non-group notice code=%d status=%d\n",
              info.si_code, info.si_status);
      return -1;
    }
    usleep(1000);
  } while (monotonic_millis() < deadline);
  return 0;
}

static void terminate_tracee(pid_t child) {
  kill(child, SIGKILL);
  ptrace(PTRACE_CONT, child, 0, 0);
  for (int attempt = 0; attempt < 1000; ++attempt) {
    int status = 0;
    pid_t waited = waitpid(child, &status, __WALL | WNOHANG);
    if (waited == child || (waited < 0 && errno == ECHILD)) {
      return;
    }
    usleep(1000);
  }
}

static int require_stop(pid_t child, int expected_signal, const char *label) {
  int status = 0;
  pid_t waited = waitpid(child, &status, __WALL);
  if (waited != child || !WIFSTOPPED(status) ||
      WSTOPSIG(status) != expected_signal) {
    fprintf(stderr, "%s: waited=%d status=%#x expected_signal=%d\n", label,
            (int)waited, status, expected_signal);
    return -1;
  }
  return 0;
}

static int authenticate_controller_trap(pid_t child,
                                        const struct shared_state *shared) {
  siginfo_t info;
  struct user_regs_struct regs;
  memset(&info, 0, sizeof(info));
  memset(&regs, 0, sizeof(regs));
  uintptr_t expected_rip =
      atomic_load_explicit(&shared->trap_rip, memory_order_seq_cst);
  if (ptrace(PTRACE_GETSIGINFO, child, 0, &info) != 0 ||
      ptrace(PTRACE_GETREGS, child, 0, &regs) != 0 ||
      info.si_signo != SIGTRAP ||
      (info.si_code != TRAP_BRKPT && info.si_code != SI_KERNEL) ||
      regs.rip != expected_rip) {
    fprintf(stderr,
            "controller trap mismatch: errno=%d signo=%d code=%d rip=%#llx "
            "expected=%#llx\n",
            errno, info.si_signo, info.si_code,
            (unsigned long long)regs.rip, (unsigned long long)expected_rip);
    return -1;
  }
  return 0;
}

static int exact_pending_process_sigcont(pid_t child) {
  siginfo_t info;
  memset(&info, 0, sizeof(info));
  int per_thread = pending_sigcont(child, 0, NULL);
  int shared = pending_sigcont(child, PTRACE_PEEKSIGINFO_SHARED, &info);
  if (per_thread != 0 || shared != 1 || info.si_signo != SIGCONT ||
      info.si_code != SI_USER || info.si_pid != getpid() ||
      info.si_uid != getuid()) {
    fprintf(stderr,
            "pending SIGCONT mismatch: thread=%d shared=%d signo=%d code=%d "
            "pid=%d uid=%d expected_pid=%d expected_uid=%d\n",
            per_thread, shared, info.si_signo, info.si_code, info.si_pid,
            info.si_uid, getpid(), getuid());
    return -1;
  }
  return 0;
}

static int run_probe(enum timing timing) {
  struct shared_state *shared =
      mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1,
           0);
  if (shared == MAP_FAILED) {
    perror("mmap");
    return 2;
  }
  atomic_init(&shared->phase, 0);
  atomic_init(&shared->trap_rip, 0);

  pid_t child = fork();
  if (child < 0) {
    perror("fork");
    return 2;
  }
  if (child == 0) {
    if (ptrace(PTRACE_TRACEME, 0, 0, 0) != 0) {
      _exit(101);
    }
    raise(SIGSTOP);
    sigset_t private_mask;
    sigemptyset(&private_mask);
    sigaddset(&private_mask, SIGCONT);
    sigaddset(&private_mask, SIGUSR1);
    if (sigprocmask(SIG_BLOCK, &private_mask, NULL) != 0) {
      _exit(102);
    }
    atomic_store_explicit(&shared->phase, 1, memory_order_seq_cst);
    raise(SIGSTOP);
    __asm__ volatile("leaq 1f(%%rip), %%rax\n\t"
                     "movq %%rax, %0\n\t"
                     "int3\n\t"
                     "1:"
                     : "=m"(shared->trap_rip)
                     :
                     : "rax", "memory");
    atomic_store_explicit(&shared->phase, 2, memory_order_seq_cst);
    _exit(0);
  }

  int pidfd = (int)syscall(SYS_pidfd_open, child, 0);
  if (pidfd < 0) {
    perror("pidfd_open");
    terminate_tracee(child);
    return 2;
  }
  if (require_stop(child, SIGSTOP, "initial stop") != 0 ||
      ptrace(PTRACE_CONT, child, 0, 0) != 0 ||
      require_stop(child, SIGSTOP, "delivery stop D") != 0) {
    perror("initial continuation");
    terminate_tracee(child);
    return 2;
  }

  siginfo_t stop_info;
  memset(&stop_info, 0, sizeof(stop_info));
  if (ptrace(PTRACE_GETSIGINFO, child, 0, &stop_info) != 0 ||
      stop_info.si_signo != SIGSTOP) {
    fprintf(stderr, "D GETSIGINFO failed: errno=%d signo=%d code=%d\n", errno,
            stop_info.si_signo, stop_info.si_code);
    terminate_tracee(child);
    return 1;
  }
  if (pending_sigcont(child, 0, NULL) != 0 ||
      pending_sigcont(child, PTRACE_PEEKSIGINFO_SHARED, NULL) != 0) {
    fprintf(stderr, "SIGCONT was already pending at D\n");
    terminate_tracee(child);
    return 1;
  }

  if (timing == CONT_BEFORE_D_RESUME && kill(child, SIGCONT) != 0) {
    perror("SIGCONT before D resume");
    terminate_tracee(child);
    return 2;
  }
  if (ptrace(PTRACE_CONT, child, 0, SIGSTOP) != 0) {
    perror("resume D with SIGSTOP");
    terminate_tracee(child);
    return 2;
  }
  if (timing == CONT_AFTER_D_RESUME) {
    if (peek_exact_group_stop(pidfd, 2000) != 1) {
      fprintf(stderr, "G was not visible before synchronized after-d SIGCONT\n");
      terminate_tracee(child);
      return 1;
    }
    if (kill(child, SIGCONT) != 0) {
      perror("SIGCONT after D resume");
      terminate_tracee(child);
      return 2;
    }
  }

  int got_g = 0;
  int got_c = 0;
  int sequence[2] = {0, 0};
  int sequence_len = 0;
  struct user_regs_struct hidden_regs;
  struct user_regs_struct sentinel_regs;
  uint64_t hidden_mask = 0;
  uint64_t sentinel_mask = 0;

  while ((!got_g || !got_c) && sequence_len < 2) {
    siginfo_t notice;
    int observed = next_notice(pidfd, &notice, 2000);
    if (observed <= 0) {
      fprintf(stderr,
              "notice timeout/error: timing=%d got_g=%d got_c=%d phase=%u\n",
              timing, got_g, got_c, shared_phase(shared));
      terminate_tracee(child);
      close(pidfd);
      return 1;
    }
    fprintf(stderr, "notice[%d]: code=%d status=%d phase=%u\n", sequence_len,
            notice.si_code, notice.si_status, shared_phase(shared));
    if (notice.si_code == CLD_TRAPPED && notice.si_status == SIGSTOP) {
      got_g = 1;
      sequence[sequence_len++] = 1;
      errno = 0;
      memset(&stop_info, 0, sizeof(stop_info));
      if (ptrace(PTRACE_GETSIGINFO, child, 0, &stop_info) != -1 ||
          errno != EINVAL) {
        fprintf(stderr, "G GETSIGINFO did not return EINVAL: errno=%d\n", errno);
        terminate_tracee(child);
        close(pidfd);
        return 1;
      }
      if (ptrace(PTRACE_GETREGS, child, 0, &hidden_regs) != 0 ||
          ptrace(PTRACE_GETSIGMASK, child, sizeof(hidden_mask), &hidden_mask) !=
              0) {
        perror("read G state");
        terminate_tracee(child);
        close(pidfd);
        return 1;
      }
      sentinel_regs = hidden_regs;
      sentinel_regs.r15 ^= UINT64_C(0x5a5a5a5aa5a5a5a5);
      sentinel_mask = hidden_mask ^ (UINT64_C(1) << (SIGUSR1 - 1));
      if (ptrace(PTRACE_SETREGS, child, 0, &sentinel_regs) != 0 ||
          ptrace(PTRACE_SETSIGMASK, child, sizeof(sentinel_mask),
                 &sentinel_mask) != 0) {
        perror("write G sentinel state");
        terminate_tracee(child);
        close(pidfd);
        return 1;
      }
      if (timing == CONT_AFTER_G && kill(child, SIGCONT) != 0) {
        perror("SIGCONT after G");
        terminate_tracee(child);
        close(pidfd);
        return 2;
      }
    } else if (notice.si_code == CLD_CONTINUED &&
               notice.si_status == SIGCONT) {
      got_c = 1;
      sequence[sequence_len++] = 2;
    } else if (timing == CONT_BEFORE_D_RESUME && !got_g &&
               notice.si_code == CLD_TRAPPED &&
               notice.si_status == SIGTRAP) {
      if (authenticate_controller_trap(child, shared) != 0 ||
          exact_pending_process_sigcont(child) != 0 ||
          shared_phase(shared) != 1) {
        fprintf(stderr,
                "canceled-before-G trap/pending state was not exact: phase=%u\n",
                shared_phase(shared));
        terminate_tracee(child);
        close(pidfd);
        return 1;
      }
      if (ptrace(PTRACE_CONT, child, 0, 0) != 0) {
        perror("resume canceled-before-G controller trap");
        terminate_tracee(child);
        close(pidfd);
        return 1;
      }
      siginfo_t terminal;
      int terminal_observed = next_notice(pidfd, &terminal, 2000);
      if (terminal_observed != 1 || terminal.si_code != CLD_EXITED ||
          terminal.si_status != 0 || shared_phase(shared) != 2) {
        fprintf(stderr,
                "canceled-before-G terminal mismatch: observed=%d code=%d "
                "status=%d phase=%u\n",
                terminal_observed, terminal.si_code, terminal.si_status,
                shared_phase(shared));
        terminate_tracee(child);
        close(pidfd);
        return 1;
      }
      fprintf(stderr,
              "PASS timing=%d authenticated canceled-before-G with pending "
              "SIGCONT\n",
              timing);
      close(pidfd);
      munmap(shared, 4096);
      return 0;
    } else {
      fprintf(stderr, "unexpected notice code=%d status=%d\n", notice.si_code,
              notice.si_status);
      terminate_tracee(child);
      close(pidfd);
      return 1;
    }
  }

  if (timing == CONT_BEFORE_D_RESUME || !got_g || !got_c ||
      sequence_len != 2 || sequence[0] != 1 || sequence[1] != 2 ||
      shared_phase(shared) != 1) {
    fprintf(stderr,
            "nonlinear D/G/C: timing=%d sequence=%d,%d got_g=%d got_c=%d "
            "phase=%u\n",
            timing, sequence[0], sequence[1], got_g, got_c,
            shared_phase(shared));
    terminate_tracee(child);
    close(pidfd);
    return 1;
  }

  struct user_regs_struct after_c_regs;
  uint64_t after_c_mask = 0;
  if (ptrace(PTRACE_GETREGS, child, 0, &after_c_regs) != 0 ||
      ptrace(PTRACE_GETSIGMASK, child, sizeof(after_c_mask), &after_c_mask) !=
          0 ||
      memcmp(&after_c_regs, &sentinel_regs, sizeof(after_c_regs)) != 0 ||
      after_c_mask != sentinel_mask) {
    fprintf(stderr, "G lost ptrace operability or sentinel state after C\n");
    terminate_tracee(child);
    close(pidfd);
    return 1;
  }
  if (ptrace(PTRACE_SETSIGMASK, child, sizeof(hidden_mask), &hidden_mask) != 0 ||
      ptrace(PTRACE_SETREGS, child, 0, &hidden_regs) != 0 ||
      ptrace(PTRACE_CONT, child, 0, 0) != 0) {
    perror("restore/resume G");
    terminate_tracee(child);
    close(pidfd);
    return 1;
  }

  if (require_stop(child, SIGTRAP, "controller completion trap") != 0 ||
      authenticate_controller_trap(child, shared) != 0 ||
      exact_pending_process_sigcont(child) != 0 ||
      ptrace(PTRACE_CONT, child, 0, 0) != 0) {
    fprintf(stderr, "completion trap did not retain pending blocked SIGCONT\n");
    terminate_tracee(child);
    close(pidfd);
    return 1;
  }

  siginfo_t terminal;
  int terminal_observed = next_notice(pidfd, &terminal, 2000);
  if (terminal_observed != 1 || terminal.si_code != CLD_EXITED ||
      terminal.si_status != 0 || shared_phase(shared) != 2) {
    fprintf(stderr,
            "terminal mismatch: observed=%d code=%d status=%d phase=%u\n",
            terminal_observed, terminal.si_code, terminal.si_status,
            shared_phase(shared));
    terminate_tracee(child);
    close(pidfd);
    return 1;
  }
  fprintf(stderr, "PASS timing=%d exact D/G/C and one G resume\n", timing);
  close(pidfd);
  munmap(shared, 4096);
  return 0;
}

int main(int argc, char **argv) {
  if (argc != 2) {
    fprintf(stderr, "usage: %s before-d|after-d|after-g\n", argv[0]);
    return 2;
  }
  struct utsname kernel;
  if (uname(&kernel) != 0) {
    perror("uname");
    return 2;
  }
  fprintf(stderr, "kernel=%s release=%s machine=%s pid=%d\n",
          kernel.sysname, kernel.release, kernel.machine, getpid());
  enum timing timing;
  if (strcmp(argv[1], "before-d") == 0) {
    timing = CONT_BEFORE_D_RESUME;
  } else if (strcmp(argv[1], "after-d") == 0) {
    timing = CONT_AFTER_D_RESUME;
  } else if (strcmp(argv[1], "after-g") == 0) {
    timing = CONT_AFTER_G;
  } else {
    fprintf(stderr, "unknown timing: %s\n", argv[1]);
    return 2;
  }
  return run_probe(timing);
}
