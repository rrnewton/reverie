/*  Copyright © 2026 Software Reliability Group, Imperial College London
 *
 *  This file is part of SaBRe.
 *
 *  SPDX-License-Identifier: GPL-3.0-or-later
 */

/*
 * REQUIRES: rdtsc
 * RUN: %{cc} %s -o %t
 * RUN: %t > %t.native
 * RUN: %{sbr} %{sbr-id} -- %t > %t.sabre
 * RUN: diff %t.native %t.sabre
 */

#include <stdint.h>
#include <stdio.h>

static void cpuid(uint32_t leaf, uint32_t subleaf, uint32_t *eax,
                  uint32_t *ebx, uint32_t *ecx, uint32_t *edx) {
  asm volatile("cpuid"
               : "=a"(*eax), "=b"(*ebx), "=c"(*ecx), "=d"(*edx)
               : "a"(leaf), "c"(subleaf));
}

int main(void) {
  uint32_t eax, ebx, ecx, edx;
  cpuid(0, 0, &eax, &ebx, &ecx, &edx);
  printf("0:%08x:%08x:%08x:%08x\n", eax, ebx, ecx, edx);
  cpuid(1, 0, &eax, &ebx, &ecx, &edx);
  /* Leaf-1 EBX contains the host logical-processor ID and can legitimately
   * differ when the two test processes are scheduled on different CPUs. */
  printf("1:%08x:%08x:%08x\n", eax, ecx, edx);
  return 0;
}
