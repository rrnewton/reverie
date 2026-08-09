#!/bin/bash
# Launch one timed side under a transient systemd --user service with the SAME
# environment ci-hub validate-run's start_unit.py builds, so both sides and the
# sanctioned producer path see identical toolchain/link/runtime state.
set -u
label=$1; shift
root=/home/newton/work/dev-hermit
lu=$root/ignored/lu-parity/usr/lib64
lurt=/home/newton/fbsource/fbcode/third-party-buck/platform010/build/libunwind/lib
systemctl --user reset-failed "soltiming-$label.service" 2>/dev/null || true
systemd-run --user --unit="soltiming-$label" \
  --description="sol-timing A/B $label" \
  --setenv=HOME="$HOME" \
  --setenv=PATH="$PATH" \
  --setenv=PKG_CONFIG_PATH="$lu/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" \
  --setenv=LIBRARY_PATH="$lu${LIBRARY_PATH:+:$LIBRARY_PATH}" \
  --setenv=LD_LIBRARY_PATH="$lurt${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  --setenv=CI_HUB_VALIDATE_PRODUCER=systemd-user-v1 \
  --setenv=CI_DAG_JOBS=16 \
  ${EXTRA_SETENV:+--setenv=$EXTRA_SETENV} \
  -- "$root/ignored/soltiming/run.sh" "$label" "$@"
