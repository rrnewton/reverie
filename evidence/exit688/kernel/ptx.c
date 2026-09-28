// ptx.c: probe what a ptracer sees for a thread-group leader when a
// non-leader thread calls execve.  One line of key=value output per iteration.
//
// usage: ptx <scenario> <iterations>
// scenarios:
//   S1p  leader in pause(); T execs; GETEVENTMSG immediately at leader EXIT stop, then CONT
//   S1f  leader in futex wait; otherwise as S1p
//   S2d  as S1p but usleep(1..50 ms) between leader EXIT stop and GETEVENTMSG
//   S2h  as S1p but hold leader at EXIT stop: poll waitpid(WNOHANG) 30 ms, check nothing
//        arrives, then GETEVENTMSG + /proc state, then CONT
//   S3a  leader syscall(SYS_exit,7); T gated.  At leader EXIT stop: GETEVENTMSG, then open
//        the gate (T execs), never resume leader; poll GETEVENTMSG(L) until ESRCH
//   S3b  as S3a but no GETEVENTMSG before opening the gate; after gate, usleep(2ms), GETEVENTMSG
//   S3c  leader SYS_exit(7), T execs immediately (ungated race). GETEVENTMSG immediately, CONT
//   S3r  leader SYS_exit(7); T gated. At leader EXIT stop: GETEVENTMSG, CONT leader, then
//        read /proc/L/task/L/stat exit_code + waitid(WNOHANG|WNOWAIT) while leader is a
//        delayed zombie, then open the gate
//   S3x  leader SYS_exit(7), T execs immediately; usleep(1..50ms) then GETEVENTMSG, CONT
//   S3p  as S3a gate-open-and-hold, but sample /proc/L/task/L/stat (state,exit_code) for 20ms
//   S4a  as S1p but PTRACE_CONT first, then GETEVENTMSG
//   S4b  as S1p but tgkill(L,SIGALRM) at the EXIT stop, then GETEVENTMSG, then CONT
//   S4c  as S1p but PTRACE_INTERRUPT at the EXIT stop, then GETEVENTMSG, then CONT
//   S4d  as S1p but tgkill(L,SIGKILL) at the EXIT stop, then GETEVENTMSG
//   S5c  as S1p, but T is run with PTRACE_SYSCALL; at T's execve syscall-entry stop the
//        tracer PTRACE_SINGLESTEPs; at the exec event it resumes with PTRACE_CONT
//   S5s  as S5c but resumes the exec event with PTRACE_SINGLESTEP
//   S4e  as S1p with a tracer helper thread spraying tgkill(L,SIGALRM) every ~50us
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/futex.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/user.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef PTRACE_EVENT_STOP
#define PTRACE_EVENT_STOP 128
#endif

static const char *sc;
static int gate_w = -1;
static volatile int futex_word = 0;
static int gate_r_child = -1;
static int child_mode; // 0 pause, 1 futex, 2 self-exit
static int child_gated;

static void *t_main(void *arg) {
  (void)arg;
  if (child_gated) {
    char c;
    while (read(gate_r_child, &c, 1) < 0 && errno == EINTR) {}
  }
  execl("/bin/true", "true", (char *)0);
  _exit(99);
}

static void child(int sync_r) {
  char c;
  while (read(sync_r, &c, 1) < 0 && errno == EINTR) {}
  pthread_t t;
  pthread_create(&t, 0, t_main, 0);
  if (child_mode == 2) syscall(SYS_exit, 7);
  for (;;) {
    if (child_mode == 1)
      syscall(SYS_futex, &futex_word, FUTEX_WAIT, 0, 0, 0, 0);
    else
      pause();
  }
}

static long now_us(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return ts.tv_sec * 1000000L + ts.tv_nsec / 1000;
}

static char out[4096];
static void add(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
#include <stdarg.h>
static void add(const char *fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  size_t n = strlen(out);
  vsnprintf(out + n, sizeof out - n, fmt, ap);
  va_end(ap);
}

static void getmsg(pid_t p, const char *key) {
  unsigned long m = 0xdeadbeef;
  errno = 0;
  long r = ptrace(PTRACE_GETEVENTMSG, p, 0, &m);
  if (r == 0) add(" %s=0x%lx", key, m);
  else add(" %s=E%s", key, strerrorname_np(errno));
}

static char proc_state(pid_t p) {
  char path[64], buf[512];
  snprintf(path, sizeof path, "/proc/%d/task/%d/stat", p, p);
  int fd = open(path, O_RDONLY);
  if (fd < 0) return '-';
  ssize_t n = read(fd, buf, sizeof buf - 1);
  close(fd);
  if (n <= 0) return '-';
  buf[n] = 0;
  char *q = strrchr(buf, ')');
  return q && q[1] && q[2] ? q[2] : '?';
}

// last field of /proc/p/task/p/stat is exit_code
static long proc_exit_code(pid_t p) {
  char path[64], buf[2048];
  snprintf(path, sizeof path, "/proc/%d/task/%d/stat", p, p);
  int fd = open(path, O_RDONLY);
  if (fd < 0) return -1;
  ssize_t n = read(fd, buf, sizeof buf - 1);
  close(fd);
  if (n <= 0) return -1;
  buf[n] = 0;
  while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == ' ')) buf[--n] = 0;
  char *s = strrchr(buf, ' ');
  return s ? atol(s + 1) : -1;
}

static atomic_int spray_on;
static atomic_int spray_pid;
static atomic_long spray_count;
static void *spray(void *a) {
  (void)a;
  for (;;) {
    if (atomic_load(&spray_on) && atomic_load(&spray_pid) > 0) {
      int p = atomic_load(&spray_pid);
      if (syscall(SYS_tgkill, p, p, SIGALRM) == 0) atomic_fetch_add(&spray_count, 1);
    }
    usleep(50);
  }
  return 0;
}

static volatile sig_atomic_t timed_out;
static void on_alarm(int s) { (void)s; timed_out = 1; }

static void one(int iter) {
  out[0] = 0;
  int sync[2], gate[2];
  pipe(sync);
  pipe(gate);
  child_mode = (!strcmp(sc, "S1f")) ? 1 : (sc[1] == '3') ? 2 : 0;
  child_gated = !strcmp(sc, "S3a") || !strcmp(sc, "S3b") || !strcmp(sc, "S3r") || !strcmp(sc, "S3p");
  pid_t L = fork();
  if (L == 0) {
    close(sync[1]);
    close(gate[1]);
    gate_r_child = gate[0];
    child(sync[0]);
    _exit(98);
  }
  close(sync[0]);
  close(gate[0]);
  gate_w = gate[1];
  long opts = PTRACE_O_TRACECLONE | PTRACE_O_TRACEEXEC | PTRACE_O_TRACEEXIT |
              PTRACE_O_TRACESYSGOOD | PTRACE_O_EXITKILL;
  if (ptrace(PTRACE_SEIZE, L, 0, opts) != 0) {
    printf("iter=%d sc=%s SEIZE_FAIL=%s\n", iter, sc, strerror(errno));
    kill(L, SIGKILL);
    waitpid(L, 0, __WALL);
    return;
  }
  write(sync[1], "g", 1);
  close(sync[1]);
  if (!strcmp(sc, "S4e")) {
    atomic_store(&spray_pid, L);
    atomic_store(&spray_on, 1);
  }
  add("iter=%d sc=%s L=%d", iter, sc, L);
  pid_t T = 0;
  int leader_exit_seen = 0, exec_seen = 0, leader_held = 0, leader_exited_report = 0, t_started = 0;
  char order[256] = "";
  timed_out = 0;
  alarm(5);
  for (;;) {
    int st;
    pid_t p = waitpid(-1, &st, __WALL);
    if (p < 0) {
      if (errno == EINTR && timed_out) {
        add(" TIMEOUT");
        kill(L, SIGKILL);
        timed_out = 0;
        alarm(5);
        continue;
      }
      if (errno == ECHILD) break;
      add(" WAITERR=%s", strerrorname_np(errno));
      break;
    }
    const char *who = p == L ? "L" : (p == T ? "T" : "O");
    if (WIFEXITED(st) || WIFSIGNALED(st)) {
      char b[48];
      if (WIFEXITED(st)) snprintf(b, sizeof b, "X%s:%d,", who, WEXITSTATUS(st));
      else snprintf(b, sizeof b, "K%s:%d,", who, WTERMSIG(st));
      strncat(order, b, sizeof order - strlen(order) - 1);
      if (p == L && !exec_seen) leader_exited_report = 1;
      continue;
    }
    if (!WIFSTOPPED(st)) continue;
    int ev = st >> 16, sig = WSTOPSIG(st);
    if (ev == PTRACE_EVENT_CLONE) {
      unsigned long m = 0;
      ptrace(PTRACE_GETEVENTMSG, p, 0, &m);
      T = (pid_t)m;
      ptrace(PTRACE_CONT, p, 0, 0);
      continue;
    }
    if (ev == PTRACE_EVENT_STOP) {
      if (p != L) t_started = 1;
      ptrace((sc[1] == '5' && p == T && !exec_seen) ? PTRACE_SYSCALL : PTRACE_CONT, p, 0, 0);
      continue;
    }
    if (ev == PTRACE_EVENT_EXEC) {
      char b[48];
      snprintf(b, sizeof b, "EXEC@%s,", who);
      strncat(order, b, sizeof order - strlen(order) - 1);
      exec_seen = 1;
      getmsg(p, "exec_msg");
      add(" T=%d", T);
      // Can the old leader's status be recovered now?
      siginfo_t si;
      memset(&si, 0, sizeof si);
      int r = waitid(P_PID, T, &si, WEXITED | WNOHANG | WNOWAIT | __WALL);
      add(" waitid_formerT=%s", r == 0 ? (si.si_pid ? "HIT" : "none") : strerrorname_np(errno));
      add(" proc_formerT=%c", proc_state(T));
      if (leader_held) add(" held_leader_tid_now=%c", proc_state(L));
      if (!strcmp(sc, "S5s")) ptrace(PTRACE_SINGLESTEP, p, 0, 0);
      else ptrace(PTRACE_CONT, p, 0, 0);
      continue;
    }
    if (ev == PTRACE_EVENT_EXIT) {
      char b[48];
      snprintf(b, sizeof b, "EXIT@%s,", who);
      strncat(order, b, sizeof order - strlen(order) - 1);
      if (p != L || exec_seen) {
        if (p == T) getmsg(p, "T_exit_msg");
        ptrace(PTRACE_CONT, p, 0, 0);
        continue;
      }
      leader_exit_seen = 1;
      add(" t_started_at_lexit=%d", t_started);
      if (!strcmp(sc, "S1p") || !strcmp(sc, "S1f") || !strcmp(sc, "S3c") || !strcmp(sc, "S4e") || sc[1] == '5') {
        getmsg(L, "lmsg");
        errno = 0;
        long r = ptrace(PTRACE_CONT, L, 0, 0);
        add(" lcont=%s", r == 0 ? "ok" : strerrorname_np(errno));
      } else if (!strcmp(sc, "S2d")) {
        int d = 1000 + rand() % 49000;
        usleep(d);
        add(" delay_us=%d", d);
        getmsg(L, "lmsg");
        ptrace(PTRACE_CONT, L, 0, 0);
      } else if (!strcmp(sc, "S2h")) {
        long t0 = now_us();
        int arrived = 0;
        while (now_us() - t0 < 30000) {
          int s2;
          pid_t q = waitpid(-1, &s2, __WALL | WNOHANG);
          if (q > 0) {
            arrived++;
            add(" held_arrival=%d:%x", q, s2);
          }
          usleep(500);
        }
        add(" held_arrivals=%d held_state=%c", arrived, proc_state(L));
        getmsg(L, "lmsg");
        ptrace(PTRACE_CONT, L, 0, 0);
      } else if (!strcmp(sc, "S3a") || !strcmp(sc, "S3b")) {
        if (!strcmp(sc, "S3a")) getmsg(L, "lmsg_pre");
        add(" state_pre=%c", proc_state(L));
        write(gate_w, "x", 1);
        leader_held = 1;
        if (!strcmp(sc, "S3b")) {
          usleep(2000);
          getmsg(L, "lmsg_post");
        } else {
          long t0 = now_us();
          int tries = 0;
          for (;;) {
            unsigned long m = 0;
            tries++;
            if (ptrace(PTRACE_GETEVENTMSG, L, 0, &m) != 0) {
              add(" esrch_after_us=%ld tries=%d errno=%s", now_us() - t0, tries,
                  strerrorname_np(errno));
              break;
            }
            if (m != 0x700) add(" lmsg_changed=0x%lx", m);
            if (now_us() - t0 > 1000000) { add(" NO_ESRCH_1s"); break; }
          }
        }
        add(" state_post=%c", proc_state(L));
        // Do not resume L.  If the exec completes, the kernel woke it.
      } else if (!strcmp(sc, "S3x")) {
        int d = 1000 + rand() % 49000;
        usleep(d);
        add(" delay_us=%d", d);
        getmsg(L, "lmsg");
        errno = 0;
        long r = ptrace(PTRACE_CONT, L, 0, 0);
        add(" lcont=%s", r == 0 ? "ok" : strerrorname_np(errno));
      } else if (!strcmp(sc, "S3p")) {
        // leader exited itself and is held at EXIT; open the gate and watch
        // /proc/L/task/L/stat (state, exit_code) as fast as possible
        write(gate_w, "x", 1);
        leader_held = 1;
        char lastst = 0;
        long lastec = -2;
        int changes = 0, zhits = 0;
        long t0 = now_us();
        while (now_us() - t0 < 20000 && changes < 12) {
          char stc = proc_state(L);
          long ec = proc_exit_code(L);
          if (stc == 'Z' && ec == 1792) zhits++;
          if (stc != lastst || ec != lastec) {
            add(" [%c,%ld]", stc, ec);
            lastst = stc;
            lastec = ec;
            changes++;
          }
        }
        add(" zombie_1792_samples=%d", zhits);
      } else if (!strcmp(sc, "S3r")) {
        getmsg(L, "lmsg");
        ptrace(PTRACE_CONT, L, 0, 0);
        usleep(2000);
        add(" zombie_state=%c proc_exit_code=%ld", proc_state(L), proc_exit_code(L));
        siginfo_t si;
        memset(&si, 0, sizeof si);
        int r = waitid(P_PID, L, &si, WEXITED | WNOHANG | WNOWAIT | __WALL);
        add(" waitid_L=%s", r == 0 ? (si.si_pid ? "HIT" : "none") : strerrorname_np(errno));
        int st2;
        pid_t q = waitpid(L, &st2, __WALL | WNOHANG);
        add(" waitpid_L=%d", q);
        write(gate_w, "x", 1);
      } else if (!strcmp(sc, "S4a")) {
        errno = 0;
        long r = ptrace(PTRACE_CONT, L, 0, 0);
        add(" lcont=%s", r == 0 ? "ok" : strerrorname_np(errno));
        getmsg(L, "lmsg_after_cont");
      } else if (!strcmp(sc, "S4b")) {
        syscall(SYS_tgkill, L, L, SIGALRM);
        getmsg(L, "lmsg");
        add(" state=%c", proc_state(L));
        ptrace(PTRACE_CONT, L, 0, 0);
      } else if (!strcmp(sc, "S4c")) {
        errno = 0;
        long r = ptrace(PTRACE_INTERRUPT, L, 0, 0);
        add(" intr=%s", r == 0 ? "ok" : strerrorname_np(errno));
        getmsg(L, "lmsg");
        ptrace(PTRACE_CONT, L, 0, 0);
      } else if (!strcmp(sc, "S4d")) {
        syscall(SYS_tgkill, L, L, SIGKILL);
        getmsg(L, "lmsg");
        errno = 0;
        long r = ptrace(PTRACE_CONT, L, 0, 0);
        add(" lcont=%s", r == 0 ? "ok" : strerrorname_np(errno));
      }
      continue;
    }
    if (sig == (SIGTRAP | 0x80)) {
      if (sc[1] == '5' && p == T && !exec_seen) {
        struct user_regs_struct r;
        ptrace(PTRACE_GETREGS, p, 0, &r);
        if (r.orig_rax == SYS_execve && (long)r.rax == -ENOSYS) {  // entry stop
          strncat(order, "T_EXECVE_ENTRY_STEP,", sizeof order - strlen(order) - 1);
          ptrace(PTRACE_SINGLESTEP, p, 0, 0);
        } else {
          ptrace(PTRACE_SYSCALL, p, 0, 0);
        }
        continue;
      }
      {
        char b[48];
        snprintf(b, sizeof b, "SYSSTOP@%s,", who);
        strncat(order, b, sizeof order - strlen(order) - 1);
      }
      ptrace(PTRACE_CONT, p, 0, 0);
      continue;
    }
    // signal-delivery stop: suppress SIGALRM, log others and suppress too
    {
      char b[48];
      if (sig != SIGALRM) {
        snprintf(b, sizeof b, "SIG%d@%s,", sig, who);
        strncat(order, b, sizeof order - strlen(order) - 1);
      }
      ptrace(PTRACE_CONT, p, 0, 0);
    }
  }
  alarm(0);
  atomic_store(&spray_on, 0);
  atomic_store(&spray_pid, 0);
  close(gate_w);
  add(" leader_exit_stop=%d exec=%d leader_wexit_before_exec=%d order=%s", leader_exit_seen,
      exec_seen, leader_exited_report, order);
  if (!strcmp(sc, "S4e")) add(" sprayed=%ld", atomic_exchange(&spray_count, 0));
  puts(out);
  fflush(stdout);
}

int main(int argc, char **argv) {
  if (argc < 3) return 2;
  sc = argv[1];
  int n = atoi(argv[2]);
  srand(getpid());
  struct sigaction sa;
  memset(&sa, 0, sizeof sa);
  sa.sa_handler = on_alarm;
  sigaction(SIGALRM, &sa, 0);  // no SA_RESTART: waitpid returns EINTR
  if (!strcmp(sc, "S4e")) {
    // helper thread must not take the tracer's own SIGALRM: block it there
    sigset_t s, o;
    sigemptyset(&s);
    sigaddset(&s, SIGALRM);
    pthread_sigmask(SIG_BLOCK, &s, &o);
    pthread_t h;
    pthread_create(&h, 0, spray, 0);
    pthread_sigmask(SIG_SETMASK, &o, 0);
  }
  for (int i = 0; i < n; i++) one(i);
  return 0;
}
