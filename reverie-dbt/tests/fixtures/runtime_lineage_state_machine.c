/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#include "runtime_lineage.h"

#include <assert.h>
#include <sched.h>
#include <stdint.h>

int main(void) {
  bool fork_child_native = reverie_dbt_child_requires_native_path(false, 0);
  bool clone_vm_child_native =
      reverie_dbt_child_requires_native_path(false, CLONE_VM);
  bool vfork_child_native =
      reverie_dbt_child_requires_native_path(false, CLONE_VM | CLONE_VFORK);
  bool native_descendant =
      reverie_dbt_child_requires_native_path(clone_vm_child_native, 0);
  bool thread_native =
      reverie_dbt_child_requires_native_path(false, CLONE_VM | CLONE_THREAD);

  assert(!fork_child_native);
  assert(clone_vm_child_native);
  assert(vfork_child_native);
  assert(native_descendant);
  assert(!thread_native);

  assert(reverie_dbt_process_owns_runtime(false, false, false));
  assert(reverie_dbt_process_owns_runtime(true, true, fork_child_native));
  assert(!reverie_dbt_process_owns_runtime(true, true, clone_vm_child_native));
  assert(!reverie_dbt_process_owns_runtime(true, true, vfork_child_native));
  assert(!reverie_dbt_process_owns_runtime(true, true, native_descendant));
  assert(!reverie_dbt_process_owns_runtime(true, false, false));

  uint64_t shared_exit_latch = 0;
  assert(!reverie_dbt_claim_runtime_thread_exit(false, &shared_exit_latch));
  assert(shared_exit_latch == 0);
  assert(reverie_dbt_claim_runtime_thread_exit(true, &shared_exit_latch));
  assert(shared_exit_latch == 1);
  assert(!reverie_dbt_claim_runtime_thread_exit(true, &shared_exit_latch));

  return 0;
}
