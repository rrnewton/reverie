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

int main(void) {
  const virtual_identity_t identities[] = {
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

  return 0;
}
