/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// The modes "root", "fork", "vfork", "clone-vfork" and "clone3-vfork" each end
// one process image with a SIGKILL that it sends to itself. DynamoRIO runs no
// exit callback for SIGKILL, so the client must end that image's protected
// evidence stream before the kill is delivered. In "root-returned", a seccomp
// filter makes the root's self-directed SIGKILL return an error, which leaves
// the root running after its stream has ended. "root-returned-error-denied"
// adds a second filter that fails every write of argv[2] bytes, the length of
// the ERROR frame payload that reports the returned kill, so that report never
// reaches the collector. In "root-after-thread", the root creates and joins one
// short-lived thread before it kills itself. In "threaded", a sibling thread
// keeps creating and joining short-lived threads, whose exits run client
// callbacks, while the main thread sends the process SIGKILL. In
// "zombie-child", the root sends SIGKILL to a child that has exited but is not
// yet reaped; that kill succeeds without being self-directed, and the root must
// run on normally. In "vfork-external", a thread of the parent kills the vfork
// child instead. A vfork or CLONE_VFORK child that dies by SIGKILL also leaves
// its DynamoRIO thread record behind in the address space it shared with the
// parent, which must not stop the parent's own process exit from running.
// "clone-vfork" creates its child with clone, and "clone3-vfork" with the
// clone3 call that glibc's posix_spawn makes.

#define _GNU_SOURCE
#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static _Alignas(16) char clone_stack[64 * 1024];
static _Atomic pid_t vfork_child;

static void kill_current_thread(void) {
  (void)syscall(SYS_tgkill, syscall(SYS_getpid), syscall(SYS_gettid), SIGKILL);
}

static int clone_vfork_child(void* argument) {
  (void)argument;
  kill_current_thread();
  return 9;
}

// The leading fields of the kernel's struct clone_args, a size that clone3
// accepts.
struct clone3_arguments {
  uint64_t flags;
  uint64_t pidfd;
  uint64_t child_tid;
  uint64_t parent_tid;
  uint64_t exit_signal;
  uint64_t stack;
  uint64_t stack_size;
  uint64_t tls;
};

// Creates a child with clone3, CLONE_VM and CLONE_VFORK, as glibc's posix_spawn
// does, and returns the child's pid to the parent. The child runs on
// clone_stack, so it cannot return into this function's frame. It sends itself
// SIGKILL with raw syscalls instead, and exits with status 9 if that returns.
static pid_t clone3_vfork_self_sigkill(void) {
  struct clone3_arguments arguments;
  memset(&arguments, 0, sizeof(arguments));
  arguments.flags = CLONE_VM | CLONE_VFORK;
  arguments.exit_signal = SIGCHLD;
  arguments.stack = (uint64_t)(uintptr_t)clone_stack;
  arguments.stack_size = sizeof(clone_stack);
  long result;
  __asm__ volatile(
      "syscall\n\t"
      "test %%rax, %%rax\n\t"
      "jnz 1f\n\t"
      "mov %[getpid], %%eax\n\t"
      "syscall\n\t"
      "mov %%rax, %%rdi\n\t"
      "mov %%rax, %%rsi\n\t"
      "mov %[sigkill], %%edx\n\t"
      "mov %[tgkill], %%eax\n\t"
      "syscall\n\t"
      "mov $9, %%edi\n\t"
      "mov %[exit], %%eax\n\t"
      "syscall\n\t"
      "1:\n\t"
      : "=a"(result)
      : "0"((long)SYS_clone3), "D"(&arguments),
        "S"(sizeof(arguments)), [getpid] "i"(SYS_getpid),
        [tgkill] "i"(SYS_tgkill), [sigkill] "i"(SIGKILL), [exit] "i"(SYS_exit)
      : "rcx", "r11", "rdx", "memory");
  if (result < 0) {
    errno = (int)-result;
    return -1;
  }
  return (pid_t)result;
}

static void* kill_vfork_child(void* argument) {
  (void)argument;
  pid_t child;
  while ((child = atomic_load(&vfork_child)) == 0)
    sched_yield();
  (void)kill(child, SIGKILL);
  return NULL;
}

static int killed_by_sigkill(pid_t child) {
  int status = 0;
  pid_t waited;
  do {
    waited = waitpid(child, &status, 0);
  } while (waited < 0 && errno == EINTR);
  return waited == child && WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL;
}

static int install_filter(struct sock_filter* filter, unsigned short length) {
  struct sock_fprog program = {.len = length, .filter = filter};
  if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
    return -1;
  return prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program, 0, 0);
}

// Makes kill, tkill and tgkill fail with EPERM whenever they send SIGKILL.
static int refuse_sigkill_syscalls(void) {
  struct sock_filter filter[] = {
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 0, 8),
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_kill, 2, 0),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_tkill, 1, 0),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_tgkill, 2, 4),
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
               offsetof(struct seccomp_data, args[1])),
      BPF_STMT(BPF_JMP | BPF_JA, 1),
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
               offsetof(struct seccomp_data, args[2])),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SIGKILL, 1, 0),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM),
  };
  return install_filter(filter,
                        (unsigned short)(sizeof(filter) / sizeof(filter[0])));
}

// Makes every write of exactly `length` bytes fail with EIO.
static int refuse_writes_of_length(unsigned int length) {
  struct sock_filter filter[] = {
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 0, 6),
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_write, 0, 4),
      // The count's low and high 32-bit halves, in little-endian order.
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
               offsetof(struct seccomp_data, args[2])),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, length, 0, 2),
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
               offsetof(struct seccomp_data, args[2]) + 4),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 0, 1, 0),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EIO),
  };
  return install_filter(filter,
                        (unsigned short)(sizeof(filter) / sizeof(filter[0])));
}

static atomic_int joined_threads;

static void* exit_at_once(void* argument) {
  return argument;
}

// Keeps creating and joining short-lived threads until the process is killed.
static void* churn_threads(void* argument) {
  (void)argument;
  for (;;) {
    pthread_t thread;
    if (pthread_create(&thread, NULL, exit_at_once, NULL) != 0 ||
        pthread_join(thread, NULL) != 0)
      _exit(17);
    atomic_fetch_add(&joined_threads, 1);
  }
  return NULL;
}

static int kill_zombie_child(void) {
  siginfo_t info;
  int status = 0;
  pid_t child = fork();
  if (child < 0) {
    perror("zombie child");
    return 7;
  }
  if (child == 0)
    _exit(0);
  // WNOWAIT leaves the exited child unreaped, so its pid cannot be reused
  // while the root signals it.
  memset(&info, 0, sizeof(info));
  if (waitid(P_PID, (id_t)child, &info, WEXITED | WNOWAIT) != 0)
    return 13;
  if (kill(child, SIGKILL) != 0)
    return 14;
  if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
      WEXITSTATUS(status) != 0)
    return 15;
  return 0;
}

int main(int argc, char** argv) {
  if (argc < 2)
    return 2;
  const char* mode = argv[1];
  if (strcmp(mode, "root-returned-error-denied") == 0) {
    char* end = NULL;
    unsigned long length = argc == 3 ? strtoul(argv[2], &end, 10) : 0;
    if (length == 0 || *end != '\0')
      return 2;
    if (refuse_writes_of_length((unsigned int)length) != 0) {
      perror("seccomp write filter");
      return 16;
    }
    mode = "root-returned";
  } else if (argc != 2) {
    return 2;
  }
  if (strcmp(mode, "root") == 0) {
    raise(SIGKILL);
    return 3;
  }
  if (strcmp(mode, "root-returned") == 0) {
    if (refuse_sigkill_syscalls() != 0) {
      perror("seccomp filter");
      return 16;
    }
    raise(SIGKILL);
    return 12;
  }
  if (strcmp(mode, "root-after-thread") == 0) {
    pthread_t thread;
    if (pthread_create(&thread, NULL, exit_at_once, NULL) != 0 ||
        pthread_join(thread, NULL) != 0)
      return 20;
    raise(SIGKILL);
    return 21;
  }
  if (strcmp(mode, "threaded") == 0) {
    pthread_t churn;
    if (pthread_create(&churn, NULL, churn_threads, NULL) != 0)
      return 18;
    while (atomic_load(&joined_threads) == 0)
      sched_yield();
    raise(SIGKILL);
    return 19;
  }
  if (strcmp(mode, "zombie-child") == 0) {
    int result = kill_zombie_child();
    if (result == 0)
      printf("self-sigkill-%s-ok\n", mode);
    return result;
  }

  pthread_t killer;
  int external = strcmp(mode, "vfork-external") == 0;
  if (external && pthread_create(&killer, NULL, kill_vfork_child, NULL) != 0)
    return 10;
  pid_t child;
  if (strcmp(mode, "fork") == 0) {
    child = fork();
    if (child == 0) {
      (void)kill(getpid(), SIGKILL);
      _exit(4);
    }
  } else if (strcmp(mode, "vfork") == 0) {
    child = vfork();
    if (child == 0) {
      kill_current_thread();
      _exit(5);
    }
  } else if (strcmp(mode, "clone-vfork") == 0) {
    child = clone(clone_vfork_child, clone_stack + sizeof(clone_stack),
                  CLONE_VM | CLONE_VFORK | SIGCHLD, NULL);
  } else if (strcmp(mode, "clone3-vfork") == 0) {
    child = clone3_vfork_self_sigkill();
  } else if (external) {
    child = vfork();
    if (child == 0) {
      atomic_store(&vfork_child, (pid_t)syscall(SYS_getpid));
      for (;;)
        (void)syscall(SYS_ppoll, NULL, 0, NULL, NULL);
    }
  } else {
    return 6;
  }
  if (child < 0) {
    perror("self-SIGKILL child");
    return 7;
  }
  if (external && pthread_join(killer, NULL) != 0)
    return 11;
  if (!killed_by_sigkill(child))
    return 8;
  printf("self-sigkill-%s-ok\n", mode);
  return 0;
}
