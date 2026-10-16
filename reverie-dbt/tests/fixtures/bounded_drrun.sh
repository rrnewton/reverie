#!/bin/sh
#
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

# Runs "$REVERIE_DBT_TEST_DRRUN" with the given arguments, bounded at 150 s.
# The run gets its own process group and the whole group is killed at the
# bound: a stuck fork child would otherwise keep the caller's output pipes
# open. The watchdog's own sleep must not outlive a run that ends in time, or
# the runner waits for it as a member of this group.
#
# This file is committed rather than written by the tests: exec'ing a file the
# test process has just written races with other test threads' forks, which can
# hold the write descriptor open (ETXTBSY).
setsid "$REVERIE_DBT_TEST_DRRUN" "$@" &
run=$!
(sleep 150 & s=$!; trap 'kill $s; exit 0' TERM; wait $s; kill -KILL -- -$run) \
  >/dev/null 2>&1 </dev/null &
watchdog=$!
wait $run
status=$?
kill $watchdog 2>/dev/null
exit $status
