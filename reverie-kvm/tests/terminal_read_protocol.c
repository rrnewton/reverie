/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/* Standalone, bounded by the caller. Compile the actual terminal_read.c with
 * -DRVK_READ_TEST -std=c11 -pthread -fexceptions, matching production. Gate
 * hooks are C-only and contain no
 * cancellation point, including the public-return/disable interval. */
#define _GNU_SOURCE
#include "../src/terminal_read.h"

#include <assert.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/inotify.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

_Static_assert(ATOMIC_INT_LOCK_FREE == 2, "test gates must be lock-free");

enum { EVENT_COUNT = RVK_READ_TEST_AFTER_JOIN + 1 };
static _Atomic unsigned reached[EVENT_COUNT];
static _Atomic bool hold[EVENT_COUNT];
static _Atomic bool released[EVENT_COUNT];
static _Atomic int event_tid[EVENT_COUNT];
static _Atomic bool capture_context;
static struct {
  sigset_t mask;
  stack_t altstack;
  int mask_error;
  int altstack_result;
  int altstack_errno;
} child_context;

void rvk_read_test_hook(struct rvk_read *op, enum rvk_read_test_event event) {
  (void)op;
  atomic_store_explicit(&event_tid[event], (int)syscall(SYS_gettid),
                        memory_order_relaxed);
  if (event == RVK_READ_TEST_BEFORE_ENABLE && atomic_load(&capture_context)) {
    /* This hook runs only with child cancellation disabled. The return hook
     * remains atomics/pause only; no context query runs in that interval. */
    child_context.mask_error = pthread_sigmask(SIG_SETMASK, NULL, &child_context.mask);
    child_context.altstack_result = sigaltstack(NULL, &child_context.altstack);
    child_context.altstack_errno = errno;
  }
  atomic_fetch_add_explicit(&reached[event], 1, memory_order_release);
  while (atomic_load_explicit(&hold[event], memory_order_relaxed) &&
         !atomic_load_explicit(&released[event], memory_order_acquire)) {
    __asm__ volatile("pause" ::: "memory");
  }
}

static void reset(void) {
  atomic_store(&capture_context, false);
  memset(&child_context, 0, sizeof(child_context));
  for (unsigned i = 0; i < EVENT_COUNT; ++i) {
    atomic_store(&reached[i], 0);
    atomic_store(&hold[i], false);
    atomic_store(&released[i], false);
    atomic_store(&event_tid[i], 0);
  }
}

static uint64_t monotonic_ns(void) {
  struct timespec now;
  assert(clock_gettime(CLOCK_MONOTONIC, &now) == 0);
  return (uint64_t)now.tv_sec * 1000000000 + (uint64_t)now.tv_nsec;
}

static void await_event(enum rvk_read_test_event event) {
  uint64_t deadline = monotonic_ns() + 5000000000;
  while (atomic_load_explicit(&reached[event], memory_order_acquire) == 0) {
    assert(monotonic_ns() < deadline);
    struct timespec pause = {.tv_nsec = 1000000};
    nanosleep(&pause, NULL);
  }
}

static void gate(enum rvk_read_test_event event) {
  atomic_store(&hold[event], true);
}

static void release(enum rvk_read_test_event event) {
  atomic_store_explicit(&released[event], true, memory_order_release);
}

static struct rvk_read_snapshot snapshot(struct rvk_read *op) {
  struct rvk_read_snapshot state = {0};
  assert(rvk_read_snapshot(op, &state) == 0);
  return state;
}

static struct rvk_read_snapshot outcome(struct rvk_read *op) {
  for (;;) {
    uint64_t epoch = rvk_read_epoch(op);
    struct rvk_read_snapshot state = snapshot(op);
    if (state.outcome != RVK_READ_PENDING) {
      return state;
    }
    assert(rvk_read_wait(op, epoch) == 0);
  }
}

static struct rvk_read *prepare(int fd) {
  int error = -1;
  struct rvk_read *op = rvk_read_new(fd, 1, 0, &error);
  assert(op != NULL && error == 0);
  return op;
}

static int null_fd(void) {
  int fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
  assert(fd >= 0);
  return fd;
}

static int inotify_fd(bool nonblocking) {
  int fd = inotify_init1(IN_CLOEXEC | (nonblocking ? IN_NONBLOCK : 0));
  assert(fd >= 0);
  return fd;
}

static void print_proc_file(const char *path) {
  FILE *file = fopen(path, "re");
  assert(file != NULL);
  char line[4096];
  printf("BEGIN %s\n", path);
  while (fgets(line, sizeof(line), file) != NULL) {
    assert(fputs(line, stdout) >= 0);
  }
  assert(!ferror(file));
  assert(fclose(file) == 0);
  printf("END %s\n", path);
}

struct call {
  struct rvk_read *op;
  int result;
};

static void *start_call(void *opaque) {
  struct call *call = opaque;
  call->result = rvk_read_start(call->op);
  return NULL;
}

static void *cancel_call(void *opaque) {
  struct call *call = opaque;
  call->result = rvk_read_request_cancel(call->op);
  return NULL;
}

static void *finish_call(void *opaque) {
  struct call *call = opaque;
  call->result = rvk_read_finish(call->op);
  return NULL;
}

static pthread_t launch(void *(*entry)(void *), struct call *call) {
  pthread_t thread;
  call->result = -1;
  assert(pthread_create(&thread, NULL, entry, call) == 0);
  return thread;
}

static void joined(pthread_t thread, struct call *call, int expected) {
  assert(pthread_join(thread, NULL) == 0);
  assert(call->result == expected);
}

static void finish_and_destroy(struct rvk_read *op, int fd) {
  assert(rvk_read_finish(op) == 0);
  struct rvk_read_snapshot state = snapshot(op);
  assert(state.senders == 0);
  assert(state.state == RVK_READ_JOINED || state.state == RVK_READ_NO_THREAD);
  assert(rvk_read_finish(op) == 0); /* Does not join a second time. */
  assert(rvk_read_destroy(op) == 0);
  assert(fcntl(fd, F_GETFD) >= 0); /* C never owns/closes the endpoint. */
  assert(close(fd) == 0);
}

static void before_start(void) {
  reset();
  int fd = inotify_fd(false);
  struct rvk_read *op = prepare(fd);
  assert(rvk_read_request_cancel(op) == 0);
  assert(rvk_read_start(op) == 0);
  struct rvk_read_snapshot state = outcome(op);
  assert(state.terminal && state.outcome == RVK_READ_NOT_STARTED);
  assert(state.state == RVK_READ_NO_THREAD && !state.handle_published);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_CREATE]) == 0);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_READ]) == 0);
  finish_and_destroy(op, fd);
  puts("PASS terminal-before-start: no pthread/read/join");
}

static void before_create(void) {
  reset();
  gate(RVK_READ_TEST_BEFORE_CREATE);
  gate(RVK_READ_TEST_BEFORE_ENABLE);
  int fd = inotify_fd(false);
  struct rvk_read *op = prepare(fd);
  struct call creator = {.op = op};
  pthread_t thread = launch(start_call, &creator);
  await_event(RVK_READ_TEST_BEFORE_CREATE);
  assert(rvk_read_request_cancel(op) == 0);
  assert(!snapshot(op).handle_published);
  release(RVK_READ_TEST_BEFORE_CREATE);
  await_event(RVK_READ_TEST_BEFORE_ENABLE);
  joined(thread, &creator, 0);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_CANCEL]) == 1);
  release(RVK_READ_TEST_BEFORE_ENABLE);
  assert(outcome(op).outcome == RVK_READ_CANCELED);
  finish_and_destroy(op, fd);
  puts("PASS terminal-during-create: latched through handle publication");
}

static void before_publication(void) {
  reset();
  gate(RVK_READ_TEST_AFTER_CREATE);
  gate(RVK_READ_TEST_BEFORE_ENABLE);
  int fd = inotify_fd(false);
  struct rvk_read *op = prepare(fd);
  struct call creator = {.op = op};
  pthread_t thread = launch(start_call, &creator);
  await_event(RVK_READ_TEST_AFTER_CREATE);
  await_event(RVK_READ_TEST_BEFORE_ENABLE);
  assert(rvk_read_request_cancel(op) == 0);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_CANCEL]) == 0);
  release(RVK_READ_TEST_AFTER_CREATE);
  joined(thread, &creator, 0);
  assert(snapshot(op).handle_published);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_CANCEL]) == 1);
  release(RVK_READ_TEST_BEFORE_ENABLE);
  assert(outcome(op).outcome == RVK_READ_CANCELED);
  finish_and_destroy(op, fd);
  puts("PASS terminal-before-handle-publication: queued public cancellation");
}

static void early_completion(void) {
  reset();
  gate(RVK_READ_TEST_AFTER_CREATE);
  int fd = null_fd();
  struct rvk_read *op = prepare(fd);
  struct call creator = {.op = op};
  pthread_t thread = launch(start_call, &creator);
  await_event(RVK_READ_TEST_AFTER_CREATE);
  struct rvk_read_snapshot state = outcome(op);
  assert(state.outcome == RVK_READ_RETURNED && state.result == 0);
  assert(!state.handle_published);
  assert(rvk_read_request_cancel(op) == 0);
  release(RVK_READ_TEST_AFTER_CREATE);
  joined(thread, &creator, 0);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_CANCEL]) == 0);
  assert(snapshot(op).outcome == RVK_READ_RETURNED);
  finish_and_destroy(op, fd);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_JOIN]) == 1);
  puts("PASS early-completion: no send window; one physical join");
}

static void normal_completion(void) {
  reset();
  gate(RVK_READ_TEST_AFTER_OUTCOME);
  int fd = null_fd();
  struct rvk_read *op = prepare(fd);
  assert(rvk_read_start(op) == 0);
  await_event(RVK_READ_TEST_AFTER_OUTCOME);
  struct rvk_read_snapshot state = outcome(op);
  assert(state.outcome == RVK_READ_RETURNED && state.result == 0);
  assert(!state.terminal && state.error_number == 0);
  assert(rvk_read_destroy(op) == EBUSY);
  struct call finisher = {.op = op};
  pthread_t thread = launch(finish_call, &finisher);
  await_event(RVK_READ_TEST_BEFORE_JOIN);
  assert(snapshot(op).state == RVK_READ_JOINING);
  assert(atomic_load(&reached[RVK_READ_TEST_AFTER_JOIN]) == 0);
  assert(rvk_read_destroy(op) == EBUSY);
  assert(fcntl(fd, F_GETFD) >= 0);
  release(RVK_READ_TEST_AFTER_OUTCOME);
  joined(thread, &finisher, 0);
  assert(!snapshot(op).terminal);
  finish_and_destroy(op, fd);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_JOIN]) == 1);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_CANCEL]) == 0);
  puts("PASS normal-zero: outcome is not retirement; one join before release");
}

static void before_read(void) {
  reset();
  gate(RVK_READ_TEST_BEFORE_READ);
  int fd = inotify_fd(false);
  struct rvk_read *op = prepare(fd);
  assert(rvk_read_start(op) == 0);
  await_event(RVK_READ_TEST_BEFORE_READ);
  assert(rvk_read_finish(op) == EBUSY);
  assert(rvk_read_destroy(op) == EBUSY);
  assert(rvk_read_request_cancel(op) == 0);
  release(RVK_READ_TEST_BEFORE_READ);
  assert(outcome(op).outcome == RVK_READ_CANCELED);
  assert(atomic_load(&reached[RVK_READ_TEST_AFTER_READ]) == 0);
  finish_and_destroy(op, fd);
  puts("PASS pre-public-read: sticky cancellation; no fabricated result");
}

/* This samples the actual target thread's kernel syscall state. The hook alone
 * is deliberately insufficient: require SYS_read with exact fd/address/count.
 * A five-second deadline is failure, never a synthesized successful wake. */
static void await_kernel_read(int tid, int fd) {
  char path[128];
  assert(snprintf(path, sizeof(path), "/proc/self/task/%d/syscall", tid) > 0);
  uint64_t deadline = monotonic_ns() + 5000000000;
  for (;;) {
    FILE *file = fopen(path, "re");
    assert(file != NULL);
    char line[512];
    assert(fgets(line, sizeof(line), file) != NULL);
    assert(fclose(file) == 0);
    long number;
    unsigned long arg0, arg1, arg2;
    if (sscanf(line, "%ld %lx %lx %lx", &number, &arg0, &arg1, &arg2) == 4 &&
        number == SYS_read && arg0 == (unsigned long)fd && arg1 == 1 &&
        arg2 == 0) {
      char stat_path[128];
      assert(snprintf(stat_path, sizeof(stat_path), "/proc/self/task/%d/stat", tid) > 0);
      FILE *stat_file = fopen(stat_path, "re");
      assert(stat_file != NULL);
      char stat_line[4096];
      assert(fgets(stat_line, sizeof(stat_line), stat_file) != NULL);
      assert(fclose(stat_file) == 0);
      char *comm_end = strrchr(stat_line, ')');
      assert(comm_end != NULL);
      if (comm_end[1] == ' ' && comm_end[2] == 'S') {
        printf("KERNEL_READ tid=%d fd=%d syscall=%s", tid, fd, line);
        printf("KERNEL_THREAD_STAT %s", stat_line);
        assert(snprintf(stat_path, sizeof(stat_path), "/proc/self/task/%d/wchan", tid) > 0);
        print_proc_file(stat_path);
        return;
      }
    }
    assert(monotonic_ns() < deadline);
    struct timespec pause = {.tv_nsec = 1000000};
    nanosleep(&pause, NULL);
  }
}

static void inside_kernel(void) {
  reset();
  int fd = inotify_fd(false);
  int flags = fcntl(fd, F_GETFL);
  int guest_alias = dup(fd);
  assert(guest_alias >= 0);
  struct rvk_read *op = prepare(fd);
  assert(rvk_read_start(op) == 0);
  await_event(RVK_READ_TEST_BEFORE_READ);
  await_kernel_read(atomic_load(&event_tid[RVK_READ_TEST_BEFORE_READ]), fd);
  assert(snapshot(op).outcome == RVK_READ_PENDING);
  assert(rvk_read_finish(op) == EBUSY);

  /* Retain the actual operation fd while a different guest-facing alias is
   * closed/reused. A reused alias must not redirect this prepared invocation. */
  assert(close(guest_alias) == 0);
  int replacement = null_fd();
  if (replacement != guest_alias) {
    assert(dup2(replacement, guest_alias) == guest_alias);
    assert(close(replacement) == 0);
  }
  assert(fcntl(fd, F_GETFL) == flags);
  assert(rvk_read_request_cancel(op) == 0);
  assert(outcome(op).outcome == RVK_READ_CANCELED);
  assert(fcntl(fd, F_GETFL) == flags);
  finish_and_destroy(op, fd);
  char byte;
  assert(read(guest_alias, &byte, 1) == 0);
  assert(close(guest_alias) == 0);
  puts("PASS inside-kernel: exact staging args, unchanged flags, alias reuse");
}

static void returned_before_disable(void) {
  reset();
  gate(RVK_READ_TEST_AFTER_READ);
  int fd = inotify_fd(true);
  int flags = fcntl(fd, F_GETFL);
  struct rvk_read *op = prepare(fd);
  assert(rvk_read_start(op) == 0);
  await_event(RVK_READ_TEST_AFTER_READ);
  assert(snapshot(op).outcome == RVK_READ_PENDING);
  assert(rvk_read_request_cancel(op) == 0);
  release(RVK_READ_TEST_AFTER_READ);
  struct rvk_read_snapshot state = outcome(op);
  assert(state.outcome == RVK_READ_RETURNED);
  assert(state.result == -1 && state.read_errno == EAGAIN && state.terminal);
  assert(fcntl(fd, F_GETFL) == flags);
  finish_and_destroy(op, fd);
  puts("PASS public-return-before-disable: real EAGAIN retained on late cancel");
}

static void delayed_sender(void) {
  reset();
  gate(RVK_READ_TEST_BEFORE_READ);
  gate(RVK_READ_TEST_BEFORE_CANCEL);
  gate(RVK_READ_TEST_AFTER_CANCEL);
  gate(RVK_READ_TEST_AFTER_JOIN);
  int fd = null_fd();
  struct rvk_read *op = prepare(fd);
  assert(rvk_read_start(op) == 0);
  await_event(RVK_READ_TEST_BEFORE_READ);
  struct call sender = {.op = op};
  pthread_t sender_thread = launch(cancel_call, &sender);
  await_event(RVK_READ_TEST_BEFORE_CANCEL);
  assert(snapshot(op).senders == 1);
  release(RVK_READ_TEST_BEFORE_READ);
  assert(outcome(op).outcome == RVK_READ_RETURNED);
  struct call finisher = {.op = op};
  pthread_t finish_thread = launch(finish_call, &finisher);
  await_event(RVK_READ_TEST_DISARMED);
  struct rvk_read_snapshot state = snapshot(op);
  assert(state.state == RVK_READ_DISARMED && state.senders == 1);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_JOIN]) == 0);
  assert(rvk_read_request_cancel(op) == 0);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_CANCEL]) == 1);

  release(RVK_READ_TEST_BEFORE_CANCEL);
  await_event(RVK_READ_TEST_AFTER_CANCEL);
  state = snapshot(op);
  assert(state.state == RVK_READ_DISARMED && state.senders == 1);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_JOIN]) == 0);
  release(RVK_READ_TEST_AFTER_CANCEL);
  joined(sender_thread, &sender, 0);
  await_event(RVK_READ_TEST_AFTER_JOIN);
  assert(snapshot(op).state == RVK_READ_JOINING);
  assert(rvk_read_request_cancel(op) == 0);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_CANCEL]) == 1);
  release(RVK_READ_TEST_AFTER_JOIN);
  joined(finish_thread, &finisher, 0);
  assert(rvk_read_request_cancel(op) == 0);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_CANCEL]) == 1);
  assert(snapshot(op).outcome == RVK_READ_RETURNED);
  finish_and_destroy(op, fd);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_JOIN]) == 1);
  puts("PASS delayed-sender: drain before join; fresh sends refused through retirement");
}

struct watched_directory {
  char path[4096];
  int dir;
};

static struct watched_directory watch_directory(int fd) {
  struct watched_directory watch;
  const char *tmp = getenv("TMPDIR");
  assert(tmp != NULL && tmp[0] == '/');
  int length = snprintf(watch.path, sizeof(watch.path), "%s/terminal-read-XXXXXX", tmp);
  assert(length > 0 && (size_t)length < sizeof(watch.path));
  assert(mkdtemp(watch.path) == watch.path);
  watch.dir = open(watch.path, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
  assert(watch.dir >= 0);
  assert(inotify_add_watch(fd, watch.path, IN_CREATE) >= 0);
  return watch;
}

static void queue_event(struct watched_directory *watch) {
  int created = openat(watch->dir, "event", O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
  assert(created >= 0);
  assert(close(created) == 0);
}

static void remove_watch(struct watched_directory *watch) {
  assert(unlinkat(watch->dir, "event", 0) == 0);
  assert(close(watch->dir) == 0);
  assert(rmdir(watch->path) == 0);
}

static void queued_event(void) {
  reset();
  int fd = inotify_fd(false);
  struct watched_directory watch = watch_directory(fd);
  queue_event(&watch);
  struct rvk_read *op = prepare(fd);
  assert(rvk_read_start(op) == 0);
  struct rvk_read_snapshot state = outcome(op);
  assert(state.outcome == RVK_READ_RETURNED);
  assert(state.result == -1 && state.read_errno == EINVAL);
  assert(!state.terminal && state.error_number == 0);
  assert(rvk_read_finish(op) == 0);
  _Alignas(struct inotify_event) char events[4096];
  ssize_t count = read(fd, events, sizeof(events));
  assert(count >= (ssize_t)sizeof(struct inotify_event));
  struct inotify_event *event = (void *)events;
  assert((event->mask & IN_CREATE) != 0);
  assert((size_t)count >= sizeof(*event) + event->len);
  assert(event->len > 0 && strcmp(event->name, "event") == 0);
  finish_and_destroy(op, fd);
  remove_watch(&watch);
  puts("PASS queued-inotify: actual EINVAL and retained event");
}

static void creation_error(void) {
  reset();
  int fd = null_fd();
  struct rvk_read *op = prepare(fd);
  rvk_read_test_fail(op, RVK_READ_ERROR_CREATE, EAGAIN);
  assert(rvk_read_start(op) == EAGAIN);
  struct rvk_read_snapshot state = snapshot(op);
  assert(state.error_phase == RVK_READ_ERROR_CREATE && state.error_number == EAGAIN);
  assert(state.outcome == RVK_READ_NOT_STARTED && !state.handle_published);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_READ]) == 0);
  finish_and_destroy(op, fd);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_JOIN]) == 0);
  puts("PASS create-error: typed no-thread failure, no fallback read");
}

static void cancellation_error(bool fail_join) {
  reset();
  int fd = inotify_fd(false);
  struct watched_directory watch = watch_directory(fd);
  struct rvk_read *op = prepare(fd);
  assert(rvk_read_start(op) == 0);
  await_event(RVK_READ_TEST_BEFORE_READ);
  await_kernel_read(atomic_load(&event_tid[RVK_READ_TEST_BEFORE_READ]), fd);
  rvk_read_test_fail(op, RVK_READ_ERROR_CANCEL, ESRCH);
  assert(rvk_read_request_cancel(op) == ESRCH);
  struct rvk_read_snapshot state = snapshot(op);
  assert(state.outcome == RVK_READ_PENDING && state.senders == 0);
  assert(state.error_phase == RVK_READ_ERROR_CANCEL && state.error_number == ESRCH);
  assert(rvk_read_finish(op) == EBUSY);
  assert(rvk_read_destroy(op) == EBUSY);
  queue_event(&watch); /* Real endpoint completion, never fabricated progress. */
  state = outcome(op);
  assert(state.outcome == RVK_READ_RETURNED);
  assert(state.result == -1 && state.read_errno == EINVAL);
  assert(state.error_phase == RVK_READ_ERROR_CANCEL && state.error_number == ESRCH);
  if (fail_join) {
    rvk_read_test_fail(op, RVK_READ_ERROR_JOIN, EDEADLK);
    assert(rvk_read_finish(op) == EDEADLK);
    state = snapshot(op);
    assert(state.state == RVK_READ_JOIN_FAILED && state.handle_published);
    assert(state.error_phase == RVK_READ_ERROR_CANCEL && state.error_number == ESRCH);
    assert(rvk_read_finish(op) == EDEADLK);
    assert(rvk_read_destroy(op) == EBUSY);
    assert(rvk_read_request_cancel(op) == 0);
    assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_JOIN]) == 1);
    assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_CANCEL]) == 1);
    assert(fcntl(fd, F_GETFD) >= 0);
    remove_watch(&watch);
    /* Intentionally retain op, fd, and unjoined thread until process exit.
     * This is a failed retirement, not a synthetic join or safe destroy. */
    puts("PASS injected-join-error: first failure and ownership retained until process exit");
  } else {
    assert(rvk_read_finish(op) == 0);
    state = snapshot(op);
    assert(state.state == RVK_READ_JOINED);
    assert(state.error_phase == RVK_READ_ERROR_CANCEL && state.error_number == ESRCH);
    finish_and_destroy(op, fd);
    remove_watch(&watch);
    puts("PASS injected-cancel-error: real event completion; first error retained");
  }
}

static void wake_epoch(void) {
  reset();
  int fd = null_fd();
  struct rvk_read *op = prepare(fd);
  uint64_t epoch = rvk_read_epoch(op);
  assert(rvk_read_wake(op) == 0);
  assert(rvk_read_wait(op, epoch) == 0); /* Wake before wait cannot be lost. */
  assert(snapshot(op).outcome == RVK_READ_PENDING);
  assert(!snapshot(op).terminal);
  assert(rvk_read_finish(op) == 0);
  finish_and_destroy(op, fd);
  puts("PASS wake-before-wait: no lost wake or invented terminal outcome");
}

static void context_error(const char *operation, const char *path, int error) {
  fflush(stdout);
  fprintf(stderr, "CONTEXT_ERROR operation=%s path=%s errno=%d (%s)\n",
          operation, path, error, strerror(error));
  fflush(stderr);
  abort();
}

static void context_file(const char *path, char *buffer, size_t capacity) {
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) {
    context_error("open", path, errno);
  }
  size_t used = 0;
  for (;;) {
    if (used == capacity - 1) {
      context_error("read-buffer-limit", path, EOVERFLOW);
    }
    ssize_t count = read(fd, buffer + used, capacity - 1 - used);
    if (count < 0) {
      context_error("read", path, errno);
    }
    if (count == 0) {
      break;
    }
    used += (size_t)count;
  }
  buffer[used] = '\0';
  if (close(fd) != 0) {
    context_error("close", path, errno);
  }
}

static void status_field(const char *status, const char *field, char *value,
                         size_t capacity) {
  size_t length = strlen(field);
  for (const char *line = status; *line != '\0';) {
    const char *end = strchr(line, '\n');
    if (end == NULL) {
      end = line + strlen(line);
    }
    if ((size_t)(end - line) > length && strncmp(line, field, length) == 0 &&
        line[length] == ':') {
      size_t size = (size_t)(end - line) - length - 1;
      if (size >= capacity) {
        context_error("status-field-limit", field, EOVERFLOW);
      }
      memcpy(value, line + length + 1, size);
      value[size] = '\0';
      return;
    }
    line = *end == '\0' ? end : end + 1;
  }
  context_error("missing-status-field", field, ENODATA);
}

static uint64_t thread_start(int tid, const char *role) {
  char path[128], data[4096];
  assert(snprintf(path, sizeof(path), "/proc/self/task/%d/stat", tid) > 0);
  context_file(path, data, sizeof(data));
  printf("CONTEXT_STAT role=%s %s", role, data);
  char *comm_end = strrchr(data, ')');
  assert(comm_end != NULL && comm_end[1] == ' ');
  char *remaining;
  char *token = strtok_r(comm_end + 2, " ", &remaining);
  for (unsigned field = 3; field < 22; ++field) {
    assert(token != NULL);
    token = strtok_r(NULL, " ", &remaining);
  }
  assert(token != NULL);
  errno = 0;
  char *end;
  unsigned long long start = strtoull(token, &end, 10);
  assert(errno == 0 && end != token && *end == '\0');
  return (uint64_t)start;
}

static void compare_link(int creator, int helper, const char *suffix) {
  char paths[2][256], targets[2][4096];
  struct stat identity[2];
  int tids[2] = {creator, helper};
  for (unsigned i = 0; i < 2; ++i) {
    int length = snprintf(paths[i], sizeof(paths[i]), "/proc/self/task/%d/%s",
                          tids[i], suffix);
    assert(length > 0 && (size_t)length < sizeof(paths[i]));
    ssize_t count = readlink(paths[i], targets[i], sizeof(targets[i]) - 1);
    if (count < 0) {
      context_error("readlink", paths[i], errno);
    }
    if ((size_t)count == sizeof(targets[i]) - 1) {
      context_error("readlink-buffer-limit", paths[i], EOVERFLOW);
    }
    targets[i][count] = '\0';
    if (stat(paths[i], &identity[i]) != 0) {
      context_error("stat", paths[i], errno);
    }
    printf("CONTEXT_LINK tid=%d field=%s target=%s dev=%llu ino=%llu\n",
           tids[i], suffix, targets[i], (unsigned long long)identity[i].st_dev,
           (unsigned long long)identity[i].st_ino);
  }
  assert(fflush(stdout) == 0);
  assert(strcmp(targets[0], targets[1]) == 0);
  assert(identity[0].st_dev == identity[1].st_dev);
  assert(identity[0].st_ino == identity[1].st_ino);
}

static void inherited_context(void) {
  reset();
  gate(RVK_READ_TEST_BEFORE_ENABLE);
  atomic_store(&capture_context, true);
  int creator = (int)syscall(SYS_gettid);
  sigset_t original_mask, test_mask;
  assert(pthread_sigmask(SIG_SETMASK, NULL, &original_mask) == 0);
  test_mask = original_mask;
  assert(sigaddset(&test_mask, SIGUSR1) == 0);
  assert(pthread_sigmask(SIG_SETMASK, &test_mask, NULL) == 0);
  stack_t original_stack, creator_stack;
  assert(sigaltstack(NULL, &original_stack) == 0);
  assert((original_stack.ss_flags & SS_ONSTACK) == 0);
  size_t stack_size = (size_t)SIGSTKSZ;
  void *stack_memory = malloc(stack_size);
  assert(stack_memory != NULL);
  stack_t test_stack = {.ss_sp = stack_memory, .ss_size = stack_size, .ss_flags = 0};
  assert(sigaltstack(&test_stack, NULL) == 0);
  assert(sigaltstack(NULL, &creator_stack) == 0);

  int fd = null_fd();
  struct rvk_read *op = prepare(fd);
  assert(rvk_read_start(op) == 0);
  await_event(RVK_READ_TEST_BEFORE_ENABLE);
  int helper = atomic_load(&event_tid[RVK_READ_TEST_BEFORE_ENABLE]);
  assert(helper != creator && helper > 0);
  assert(atomic_load(&event_tid[RVK_READ_TEST_BEFORE_CREATE]) == creator);
  uint64_t creator_start = thread_start(creator, "creator");
  uint64_t helper_start = thread_start(helper, "helper");
  printf("CONTEXT_IDENTITY creator=%d/%llu helper=%d/%llu\n", creator,
         (unsigned long long)creator_start, helper, (unsigned long long)helper_start);
  if (child_context.mask_error != 0) {
    context_error("pthread_sigmask", "helper", child_context.mask_error);
  }
  if (child_context.altstack_result != 0) {
    context_error("sigaltstack", "helper", child_context.altstack_errno);
  }
  for (int signal = 1; signal < NSIG; ++signal) {
    assert(sigismember(&test_mask, signal) == sigismember(&child_context.mask, signal));
  }
  assert(sigismember(&child_context.mask, SIGUSR1) == 1);
  printf("CONTEXT_ALTSTACK creator_flags=%d creator_sp=%p creator_size=%zu "
         "helper_flags=%d helper_sp=%p helper_size=%zu\n",
         creator_stack.ss_flags, creator_stack.ss_sp, creator_stack.ss_size,
         child_context.altstack.ss_flags, child_context.altstack.ss_sp,
         child_context.altstack.ss_size);
  assert(fflush(stdout) == 0);
  assert((creator_stack.ss_flags & SS_DISABLE) == 0);
  assert((child_context.altstack.ss_flags & SS_DISABLE) != 0);
  assert((child_context.altstack.ss_flags & SS_ONSTACK) == 0);

  char creator_status[32768], helper_status[32768], path[128];
  assert(snprintf(path, sizeof(path), "/proc/self/task/%d/status", creator) > 0);
  context_file(path, creator_status, sizeof(creator_status));
  assert(snprintf(path, sizeof(path), "/proc/self/task/%d/status", helper) > 0);
  context_file(path, helper_status, sizeof(helper_status));
  const char *fields[] = {"Uid", "Gid", "Groups", "CapInh", "CapPrm", "CapEff",
                          "CapBnd", "CapAmb", "NoNewPrivs", "Seccomp",
                          "Seccomp_filters", "SigBlk", "Umask"};
  for (unsigned i = 0; i < sizeof(fields) / sizeof(fields[0]); ++i) {
    char left[16384], right[16384];
    status_field(creator_status, fields[i], left, sizeof(left));
    status_field(helper_status, fields[i], right, sizeof(right));
    printf("CONTEXT_STATUS field=%s creator=%s helper=%s\n", fields[i], left, right);
    assert(fflush(stdout) == 0);
    assert(strcmp(left, right) == 0);
  }
  /* Pending signals are recorded, never asserted inherited by the new thread. */
  const char *pending[] = {"SigPnd", "ShdPnd"};
  for (unsigned i = 0; i < sizeof(pending) / sizeof(pending[0]); ++i) {
    char left[256], right[256];
    status_field(creator_status, pending[i], left, sizeof(left));
    status_field(helper_status, pending[i], right, sizeof(right));
    printf("CONTEXT_PENDING field=%s creator=%s helper=%s\n", pending[i], left, right);
  }
  const char *links[] = {"ns/user", "ns/mnt", "ns/pid", "ns/pid_for_children",
                         "ns/net", "ns/uts", "ns/ipc", "ns/cgroup", "ns/time",
                         "ns/time_for_children", "cwd", "root"};
  for (unsigned i = 0; i < sizeof(links) / sizeof(links[0]); ++i) {
    compare_link(creator, helper, links[i]);
  }
  char creator_lsm[4096], helper_lsm[4096];
  assert(snprintf(path, sizeof(path), "/proc/self/task/%d/attr/current", creator) > 0);
  context_file(path, creator_lsm, sizeof(creator_lsm));
  assert(snprintf(path, sizeof(path), "/proc/self/task/%d/attr/current", helper) > 0);
  context_file(path, helper_lsm, sizeof(helper_lsm));
  printf("CONTEXT_LSM creator=%s helper=%s\n", creator_lsm, helper_lsm);
  assert(fflush(stdout) == 0);
  assert(strcmp(creator_lsm, helper_lsm) == 0);

  Dl_info binding;
  assert(dladdr((void *)read, &binding) != 0);
  assert(binding.dli_fname != NULL && binding.dli_fbase != NULL);
  printf("CONTEXT_READ_BINDING address=%p dso=%s base=%p symbol=%s symbol_address=%p\n",
         (void *)read, binding.dli_fname, binding.dli_fbase,
         binding.dli_sname != NULL ? binding.dli_sname : "<unknown>", binding.dli_saddr);
  print_proc_file("/proc/self/maps");
  assert(snapshot(op).outcome == RVK_READ_PENDING);
  assert(atomic_load(&reached[RVK_READ_TEST_BEFORE_READ]) == 0);
  release(RVK_READ_TEST_BEFORE_ENABLE);
  struct rvk_read_snapshot state = outcome(op);
  assert(state.outcome == RVK_READ_RETURNED && state.result == 0 && !state.terminal);
  finish_and_destroy(op, fd);
  assert(sigaltstack(&original_stack, NULL) == 0);
  free(stack_memory);
  assert(pthread_sigmask(SIG_SETMASK, &original_mask, NULL) == 0);
  atomic_store(&capture_context, false);
  puts("PASS inherited-context: matching credentials/namespaces/mask; new thread has disabled altstack");
}

int main(void) {
  printf("terminal_read_protocol pid=%ld owner_tid=%ld\n", (long)getpid(), syscall(SYS_gettid));
  char executable[4096];
  ssize_t length = readlink("/proc/self/exe", executable, sizeof(executable) - 1);
  assert(length >= 0 && (size_t)length < sizeof(executable) - 1);
  executable[length] = '\0';
  printf("EXECUTABLE %s\n", executable);
  print_proc_file("/proc/self/stat");
  print_proc_file("/proc/self/maps");
  before_start();
  before_create();
  before_publication();
  early_completion();
  normal_completion();
  before_read();
  inside_kernel();
  returned_before_disable();
  delayed_sender();
  queued_event();
  creation_error();
  cancellation_error(false);
  wake_epoch();
  assert(fflush(stdout) == 0);
  inherited_context();
  assert(fflush(stdout) == 0);
  /* Isolate the deliberately unretired join-error ownership. Do not reclaim
   * its operation to make leak checking or a retirement assertion pass. */
  pid_t child = fork();
  assert(child >= 0);
  if (child == 0) {
    cancellation_error(true);
    assert(fflush(stdout) == 0);
    _exit(0);
  }
  int status;
  assert(waitpid(child, &status, 0) == child);
  assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
  puts("PASS all C protocol controls; injected retirement failure contained by process exit");
  return 0;
}
