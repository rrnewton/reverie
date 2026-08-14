/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#include <stdint.h>
#include <stdio.h>

#include "virtual_identity.h"

static int expect_translation(const virtual_identity_t *identities,
                              size_t count, int32_t guest, int32_t expected) {
  int32_t actual = host_identity_for_guest_entries(identities, count, guest);
  if (actual == expected)
    return 0;

  fprintf(stderr, "guest identity %d resolved to %d, expected %d\n", guest,
          actual, expected);
  return 1;
}

static int expect_lookup(const virtual_identity_t *identities, size_t count,
                         int32_t host, int32_t expected) {
  int32_t actual = 0;
  if (!lookup_virtual_identity_entries(identities, count, host, &actual)) {
    fprintf(stderr, "host identity %d was not found\n", host);
    return 1;
  }
  if (actual == expected)
    return 0;

  fprintf(stderr, "host identity %d resolved to %d, expected %d\n", host,
          actual, expected);
  return 1;
}

int main(void) {
  virtual_identity_t identities[] = {
      {.host = 4, .virtual_id = 3},
      {.host = 100, .virtual_id = 4},
  };
  const size_t count = sizeof(identities) / sizeof(identities[0]);

  if (expect_translation(identities, count, 3, 4) != 0)
    return 1;
  if (expect_translation(identities, count, 4, 100) != 0)
    return 2;
  if (expect_translation(identities, count, 100, 100) != 0)
    return 3;
  if (expect_translation(identities, count, 200, -1) != 0)
    return 4;
  if (expect_translation(identities, count, 0, 0) != 0)
    return 5;
  if (expect_translation(identities, count, -1, -1) != 0)
    return 6;
  if (expect_lookup(identities, count, 100, 4) != 0)
    return 7;

  int32_t unchanged = 23;
  if (lookup_virtual_identity_entries(identities, count, 200, &unchanged))
    return 8;
  if (unchanged != 23) {
    fprintf(stderr, "missing host identity changed output to %d, expected 23\n",
            unchanged);
    return 9;
  }

  int32_t refreshed = 4;
  if (!update_virtual_identity_entries(identities, count, 100, 9))
    return 10;
  if (!lookup_virtual_identity_entries(identities, count, 100, &refreshed))
    return 11;
  if (refreshed != 9) {
    fprintf(stderr, "refreshed host identity remained %d, expected 9\n",
            refreshed);
    return 12;
  }

  return 0;
}
