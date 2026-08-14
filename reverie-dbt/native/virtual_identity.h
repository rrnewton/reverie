/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#ifndef REVERIE_DBT_NATIVE_VIRTUAL_IDENTITY_H
#define REVERIE_DBT_NATIVE_VIRTUAL_IDENTITY_H

#include <stddef.h>
#include <stdint.h>

typedef struct {
  int32_t host;
  int32_t virtual_id;
} virtual_identity_t;

static inline int32_t host_identity_for_guest_entries(
    const virtual_identity_t *identities, size_t count, int32_t identity) {
  size_t i;

  if (identity <= 0)
    return identity;

  /* A guest virtual ID wins even when an earlier entry has the same numeric
   * host ID. */
  for (i = 0; i < count; ++i) {
    if (identities[i].virtual_id == identity)
      return identities[i].host;
  }

  for (i = 0; i < count; ++i) {
    if (identities[i].host == identity)
      return identities[i].host;
  }

  return -1;
}

#endif
