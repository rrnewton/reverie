// ptx6.c: can the exec zap move a held leader from a non-EXIT ptrace stop into its
// EXIT stop behind the tracer's back, so that a resume the tracer meant for the
// earlier stop consumes the EXIT stop?
//
// usage: ptx6 <mode> <iterations>
//   mode[0]: 's' = leader held in a SIGALRM signal-delivery stop (tgkill from tracer)
//            'i' = leader held in a PTRACE_EVENT_STOP trap (PTRACE_INTERRUPT)
//   mode[1]: 'c' = after T's exec has started, issue the PTRACE_CONT meant for the held stop
//            'w' = after T's exec has started, waitpid(L, WNOHANG) first
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PTRACE_EVENT_STOP
#define PTRACE_EVENT_STOP 128
#endif

static int gate_r_child;
static void *t_main(void *a) {
  (void)a;
  char c;
  while (read(gate_r_child, &c, 1) < 0 && errno == EINTR) {}
  execl("/bin/true", "true", (char *)0);
  _exit(99);
}

static char out[4096];
static void add(const char *fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  size_t n = strlen(out);
  vsnprintf(out + n, sizeof out - n, fmt, ap);
  va_end(ap);
}

static void siginfo(pid_t p, const char *k) {
  siginfo_t si;
  memset(&si, 0, sizeof si);
  if (ptrace(PTRACE_GETSIGINFO, p, 0, &si) == 0)
    add(" %s=signo%d/code0x%x", k, si.si_signo, si.si_code);
  else
    add(" %s=E%s", k, strerrorname_np(errno));
}
static void getmsg(pid_t p, const char *k) {
  unsigned long m = 0;
  if (ptrace(PTRACE_GETEVENTMSG, p, 0, &m) == 0) add(" %s=0x%lx", k, m);
  else add(" %s=E%s", k, strerrorname_np(errno));
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

static void desc(char *b, size_t n, pid_t p, pid_t L, pid_t T, int st) {
  const char *w = p == L ? "L" : p == T ? "T" : "O";
  if (WIFEXITED(st)) snprintf(b, n, "X%s:%d,", w, WEXITSTATUS(st));
  else if (WIFSIGNALED(st)) snprintf(b, n, "K%s:%d,", w, WTERMSIG(st));
  else if (WIFSTOPPED(st)) {
    int ev = st >> 16;
    if (ev == PTRACE_EVENT_EXIT) snprintf(b, n, "EXIT@%s,", w);
    else if (ev == PTRACE_EVENT_EXEC) snprintf(b, n, "EXEC@%s,", w);
    else if (ev == PTRACE_EVENT_STOP) snprintf(b, n, "ESTOP@%s,", w);
    else if (ev == PTRACE_EVENT_CLONE) snprintf(b, n, "CLONE@%s,", w);
    else snprintf(b, n, "SIG%d@%s,", WSTOPSIG(st), w);
  } else snprintf(b, n, "?%x,", st);
}

static void one(const char *mode, int iter) {
  out[0] = 0;
  int sync[2], gate[2];
  pipe(sync);
  pipe(gate);
  pid_t L = fork();
  if (L == 0) {
    close(sync[1]);
    close(gate[1]);
    gate_r_child = gate[0];
    char c;
    while (read(sync[0], &c, 1) < 0 && errno == EINTR) {}
    pthread_t t;
    pthread_create(&t, 0, t_main, 0);
    for (;;) pause();
  }
  close(sync[0]);
  close(gate[0]);
  ptrace(PTRACE_SEIZE, L, 0,
         PTRACE_O_TRACECLONE | PTRACE_O_TRACEEXEC | PTRACE_O_TRACEEXIT | PTRACE_O_EXITKILL);
  write(sync[1], "g", 1);
  close(sync[1]);
  add("iter=%d mode=%s", iter, mode);
  pid_t T = 0;
  int t_started = 0, clone_done = 0, st;
  char order[512] = "", b[64];
  alarm(10);
  // phase 1: let T start and block on the gate
  while (!(t_started && clone_done)) {
    pid_t p = waitpid(-1, &st, __WALL);
    if (p < 0) { add(" P1ERR=%s", strerrorname_np(errno)); goto drain; }
    int ev = st >> 16;
    if (WIFSTOPPED(st) && ev == PTRACE_EVENT_CLONE) {
      unsigned long m = 0;
      ptrace(PTRACE_GETEVENTMSG, p, 0, &m);
      T = m;
      clone_done = 1;
    } else if (WIFSTOPPED(st) && ev == PTRACE_EVENT_STOP) {
      t_started = 1;
    }
    ptrace(PTRACE_CONT, p, 0, 0);
  }
  usleep(3000);
  add(" pre_state=%c", proc_state(L));
  // phase 2: hold the leader in a non-EXIT stop
  if (mode[0] == 's') syscall(SYS_tgkill, L, L, SIGALRM);
  else ptrace(PTRACE_INTERRUPT, L, 0, 0);
  if (waitpid(L, &st, __WALL) != L) { add(" HOLDERR=%s", strerrorname_np(errno)); goto drain; }
  desc(b, sizeof b, L, L, T, st);
  add(" held=%s", b);
  siginfo(L, "si_held");
  // phase 3: let T exec (the zap happens while L is held)
  write(gate[1], "x", 1);
  usleep(5000);
  add(" state_after_gate=%c", proc_state(L));
  siginfo(L, "si_now");
  getmsg(L, "msg_now");
  if (mode[1] == 'c') {
    long r = ptrace(PTRACE_CONT, L, 0, 0);  // meant for the held stop, suppresses SIGALRM
    add(" stale_cont=%s", r == 0 ? "ok" : strerrorname_np(errno));
  } else {
    pid_t q = waitpid(L, &st, __WALL | WNOHANG);
    if (q == L) {
      desc(b, sizeof b, L, L, T, st);
      add(" wnohang=%s", b);
      getmsg(L, "msg_after_wait");
    } else add(" wnohang=%d", q);
    ptrace(PTRACE_CONT, L, 0, 0);
  }
drain:
  for (;;) {
    pid_t p = waitpid(-1, &st, __WALL);
    if (p < 0) break;
    desc(b, sizeof b, p, L, T, st);
    strncat(order, b, sizeof order - strlen(order) - 1);
    if (WIFSTOPPED(st)) {
      if ((st >> 16) == PTRACE_EVENT_EXIT && p == L && !strstr(order, "EXEC")) getmsg(p, "late_exit_msg");
      if ((st >> 16) == PTRACE_EVENT_EXEC) getmsg(p, "exec_msg");
      ptrace(PTRACE_CONT, p, 0, 0);
    }
  }
  alarm(0);
  close(gate[1]);
  add(" T=%d order=%s", T, order);
  puts(out);
  fflush(stdout);
}

int main(int argc, char **argv) {
  if (argc < 3) return 2;
  int n = atoi(argv[2]);
  for (int i = 0; i < n; i++) one(argv[1], i);
  return 0;
}
