/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#ifndef REVERIE_DBT_RUNTIME_LINEAGE_H
#define REVERIE_DBT_RUNTIME_LINEAGE_H

#include <sched.h>
#include <stdbool.h>
#include <stdint.h>

static inline bool
reverie_dbt_child_requires_native_path(bool parent_requires_native_path,
                                       uint64_t clone_flags) {
  bool creates_shared_vm_process =
      (clone_flags & (CLONE_VM | CLONE_THREAD)) == CLONE_VM;
  return parent_requires_native_path || creates_shared_vm_process;
}

static inline bool reverie_dbt_process_owns_runtime(bool copied,
                                                    bool external_global,
                                                    bool requires_native_path) {
  return !copied || (external_global && !requires_native_path);
}

static inline bool
reverie_dbt_claim_runtime_thread_exit(bool owns_runtime,
                                      uint64_t *runtime_thread_exit_called) {
  if (!owns_runtime || *runtime_thread_exit_called != 0)
    return false;
  *runtime_thread_exit_called = 1;
  return true;
}

#endif
