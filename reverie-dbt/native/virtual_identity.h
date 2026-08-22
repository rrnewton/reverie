/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#ifndef REVERIE_DBT_NATIVE_VIRTUAL_IDENTITY_H
#define REVERIE_DBT_NATIVE_VIRTUAL_IDENTITY_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct {
  int32_t host;
  int32_t virtual_id;
} virtual_identity_t;

static inline bool lookup_virtual_identity_entries(
    const virtual_identity_t *identities, size_t count, int32_t host,
    int32_t *virtual_id) {
  size_t i;

  if (host <= 0)
    return false;

  for (i = 0; i < count; ++i) {
    if (identities[i].host == host) {
      *virtual_id = identities[i].virtual_id;
      return true;
    }
  }

  return false;
}

static inline int32_t virtual_identity_for_host_entries(
    const virtual_identity_t *identities, size_t count, int32_t host) {
  int32_t result = host;

  if (host <= 0)
    return host;

  (void)lookup_virtual_identity_entries(identities, count, host, &result);
  return result;
}

static inline bool update_virtual_identity_entries(
    virtual_identity_t *identities, size_t count, int32_t host,
    int32_t virtual_id) {
  size_t i;

  for (i = 0; i < count; ++i) {
    if (identities[i].host == host ||
        identities[i].virtual_id == virtual_id) {
      identities[i] = (virtual_identity_t){host, virtual_id};
      return true;
    }
  }

  return false;
}

static inline bool clone_identity_mapping_ready(uint64_t pending_thread_clones,
                                                bool mapping_found) {
  return pending_thread_clones == 0 && mapping_found;
}

static inline int32_t host_identity_for_guest_entries(
    const virtual_identity_t *identities, size_t count, int32_t identity) {
  size_t i;

  if (identity <= 0)
    return identity;

  /* Tool and guest syscall arguments are guest-visible identities.  Resolve
   * only that namespace: accepting a numerically equal host ID here makes the
   * result depend on host PID allocation and is ambiguous at collisions. */
  for (i = 0; i < count; ++i) {
    if (identities[i].virtual_id == identity)
      return identities[i].host;
  }

  return -1;
}

#endif
