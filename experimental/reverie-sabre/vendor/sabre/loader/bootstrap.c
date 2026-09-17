/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#include "bootstrap.h"

#include <elf.h>
#include <err.h>
#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>

static bool requested;
static bool image_bound;
static bool state_taken;

/* A protocol violation is not a getrandom errno that guest libc may ignore
 * or handle by trying another entropy path. This can run before client TLS
 * and libc initialization, so use only raw syscalls and constant storage.
 */
__attribute__((noreturn)) static void invalid_random_phase(void) {
#ifdef __x86_64__
  static const char message[] =
      "SaBRe: getrandom outside initialized loader bootstrap phase\n";
  long ignored;
  __asm__ volatile("syscall"
                   : "=a"(ignored)
                   : "0"(SYS_write), "D"(2), "S"(message),
                     "d"(sizeof(message) - 1)
                   : "rcx", "r11", "memory");
  __asm__ volatile("syscall"
                   : "=a"(ignored)
                   : "0"(SYS_exit_group), "D"(EXIT_FAILURE)
                   : "rcx", "r11", "memory");
  __builtin_unreachable();
#else
  abort();
#endif
}

void sbr_bootstrap_configure(void) {
  const char *value = getenv(SBR_BOOTSTRAP_ENV);
  if (value == NULL)
    return;
  if (strcmp(value, "1") != 0)
    errx(EXIT_FAILURE, "invalid " SBR_BOOTSTRAP_ENV);
#ifndef __x86_64__
  errx(EXIT_FAILURE, "loader bootstrap requires x86-64");
#endif
  requested = true;
}

bool sbr_bootstrap_enabled(void) { return requested; }

__attribute__((noinline, visibility("default"))) long
sbr_bootstrap_request_v1(unsigned long operation, unsigned long arg1,
                         unsigned long arg2, unsigned long arg3,
                         unsigned long arg4) {
#ifdef __x86_64__
  register unsigned long r10 __asm__("r10") = arg2;
  register unsigned long r8 __asm__("r8") = arg3;
  register unsigned long r9 __asm__("r9") = arg4;
  long result;
  __asm__ volatile(".global sbr_bootstrap_syscall_v1\n"
                   "sbr_bootstrap_syscall_v1:\n"
                   "syscall"
                   : "=a"(result)
                   : "0"(SYS_prctl), "D"(SBR_BOOTSTRAP_OPTION), "S"(operation),
                     "d"(arg1), "r"(r10), "r"(r8), "r"(r9)
                   : "rcx", "r11", "memory");
  return result;
#else
  (void)operation;
  (void)arg1;
  (void)arg2;
  (void)arg3;
  (void)arg4;
  return -ENOSYS;
#endif
}

void sbr_bootstrap_image(void *stack, void *entry) {
  if (!requested)
    return;
  if (image_bound || state_taken)
    errx(EXIT_FAILURE, "duplicate loader bootstrap image");

  /* unsetenv before load's find_auxv would shorten the environment without
   * moving auxv and make the original stack unparsable. Consume the private
   * option only once the final guest stack exists, moving the complete auxv
   * together with the remaining environment. The guest stack pointer and its
   * alignment, argv, and all other environment strings are unchanged.
   */
  uintptr_t *words = stack;
  uintptr_t *env = words + 1 + words[0] + 1;
  uintptr_t *option = NULL;
  uintptr_t *end = env;
  for (; *end != 0; ++end) {
    const char *value = (const char *)*end;
    if (strncmp(value, SBR_BOOTSTRAP_ENV "=", sizeof(SBR_BOOTSTRAP_ENV)) == 0) {
      if (option != NULL || strcmp(value, SBR_BOOTSTRAP_ENV "=1") != 0)
        errx(EXIT_FAILURE, "ambiguous private loader bootstrap environment");
      option = end;
    }
  }
  if (option == NULL)
    errx(EXIT_FAILURE,
         "private loader bootstrap environment missing from stack");
  ++end;
  while (end[0] != AT_NULL)
    end += 2;
  end += 2;
  memmove(option, option + 1, (char *)end - (char *)(option + 1));

  long result =
      sbr_bootstrap_request_v1(SBR_BOOTSTRAP_IMAGE, (unsigned long)stack,
                               (unsigned long)entry, SBR_BOOTSTRAP_VERSION, 0);
  if (result != 0)
    errx(EXIT_FAILURE, "loader bootstrap image refused: %ld", result);
  image_bound = true;
}

long sbr_bootstrap_getrandom(long buffer, long length, long flags,
                             void *wrapper_sp) {
  if (!requested)
    return -EPROTO;
  if (!image_bound || state_taken)
    invalid_random_phase();
  return sbr_bootstrap_request_v1(SBR_BOOTSTRAP_GETRANDOM, buffer, length,
                                  flags, (unsigned long)wrapper_sp);
}

long sbr_bootstrap_take_state(void *buffer, size_t capacity) {
  if (!requested || !image_bound || state_taken)
    return -EPROTO;
  if (buffer == NULL || capacity == 0 || capacity > SBR_BOOTSTRAP_MAX_STATE)
    return -EINVAL;
  long result =
      sbr_bootstrap_request_v1(SBR_BOOTSTRAP_TAKE_STATE, (unsigned long)buffer,
                               capacity, SBR_BOOTSTRAP_VERSION, 0);
  if (result > 0 && (unsigned long)result <= capacity) {
    state_taken = true;
    return result;
  }
  /* A rejected or short-capacity request must not retire the supervisor's
   * state. An invalid successful response is never treated as an empty state.
   */
  return result < 0 ? result : -EPROTO;
}
