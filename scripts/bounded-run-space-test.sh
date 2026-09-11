#!/usr/bin/env bash
# shellcheck disable=SC2016
# Tests for bounded-run-space, covering BOTH paths that matter:
#
#   * the BOUNDED path, on a throwaway loopback btrfs with quotas enabled, and
#   * the HONEST-DEGRADATION paths for a btrfs mount without quotas and a
#     non-btrfs filesystem.
#
# WHY THE DEGRADATION CASES CARRY MORE WEIGHT HERE THAN THE HAPPY PATH. Until
# quotas are enabled, EVERY real call takes the unbounded path. A silent fallback
# would mean callers believe their runs are capped when nothing caps them, which is
# worse than not having the feature: it converts a known gap into a false guarantee.
# So cases 5-9 assert that the fallback is loud, exits 10, and leaves a marker.
#
# WHY A READBACK CASE EXISTS (case 3). `btrfs qgroup limit` reports failure by exit
# code, but the property we need is not "the command succeeded", it is "the limit is
# live on the qgroup". Those are different claims, and only the second one bounds
# anything. Case 3 asserts we report BOUNDED only when the readback agrees.
#
# The loopback is 256 MiB and is torn down on exit. The test never fills /data.
#
# Run: scripts/bounded-run-space-test.sh
set -uo pipefail

BRS="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/bounded-run-space"
PASS=0
declare -a FAILURES=()
fail() { FAILURES+=("$1${2:+  -- $2}"); }
ok()   { PASS=$(( PASS + 1 )); }
check() { if [ "$1" = "yes" ]; then ok; else fail "$2" "${3:-}"; fi; }
yn()  { if eval "$1"; then echo yes; else echo no; fi; }

[ -x "$BRS" ] || { echo "FAIL  bounded-run-space not executable at $BRS"; exit 1; }

# Refuse rather than report a vacuous pass: without sudo+btrfs the bounded half of
# this suite cannot run at all, and a suite that silently tests only the easy half
# is exactly the false assurance this tool exists to avoid.
sudo -n true >/dev/null 2>&1 || { echo "FAIL  passwordless sudo unavailable; refusing to test only the degraded half"; exit 1; }
command -v mkfs.btrfs >/dev/null 2>&1 || { echo "FAIL  mkfs.btrfs unavailable"; exit 1; }

# Guard this hard. An empty WORK sends every `sudo` line below to the HOST ROOT --
# `truncate /img`, `mount /mnt`. That is not hypothetical: it happened while writing
# these tests, creating a 256 MB /img and mounting it over /mnt.
WORK=$(mktemp -d /var/tmp/brs-test.XXXXXX) || { echo "FAIL  could not create a work directory"; exit 1; }
case "$WORK" in /var/tmp/brs-test.*) ;; *) echo "FAIL  refusing to run with WORK='$WORK'"; exit 1 ;; esac
cleanup() {
    sudo -n umount "$WORK/mnt" 2>/dev/null || true
    sudo -n rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

sudo -n truncate -s 256M "$WORK/img"
sudo -n mkfs.btrfs -q -f "$WORK/img" >/dev/null 2>&1
sudo -n mkdir -p "$WORK/mnt"
sudo -n mount -o loop,compress=zstd:3 "$WORK/img" "$WORK/mnt" || { echo "FAIL  could not mount loopback"; exit 1; }
sudo -n chmod 0777 "$WORK/mnt"

# --- 1. HONEST DEGRADATION on btrfs before quotas are enabled -------------------
out0="$("$BRS" create --limit 4M --tag no-quota --base "$WORK/mnt" 2>"$WORK/no-quota.err")"; rc0=$?
check "$(yn '[ "$rc0" -eq 10 ]')" "quota-off create exits 10, NOT 0" "exit=$rc0"
check "$(yn 'grep -q "UNBOUNDED" "$WORK/no-quota.err"')" "quota-off create says UNBOUNDED" "$(cat "$WORK/no-quota.err")"
check "$(yn '[ -f "$out0/.UNBOUNDED" ]')" "quota-off create leaves an ownership and warning marker" ""
"$BRS" report "$out0" >/dev/null 2>"$WORK/no-quota-report.err"; r0=$?
check "$(yn '[ "$r0" -eq 10 ]')" "quota-off report remains UNBOUNDED" "exit=$r0"
"$BRS" destroy "$out0" >/dev/null 2>"$WORK/no-quota-destroy.err"; d0=$?
check "$(yn '[ "$d0" -eq 0 ] && [ ! -e "$out0" ]')" "quota-off space is still safely destroyable" "exit=$d0"

sudo -n btrfs quota enable "$WORK/mnt" || { echo "FAIL  could not enable quotas on the loopback"; exit 1; }

# --- 2-4. BOUNDED path on a quota-enabled filesystem ----------------------------
out="$("$BRS" create --limit 4M --tag t1 --base "$WORK/mnt" 2>"$WORK/t1.err")"; rc=$?
check "$(yn '[ "$rc" -eq 0 ]')" "bounded create exits 0 when quotas are on" "exit=$rc $(cat "$WORK/t1.err")"
check "$(yn '[ -d "$out" ]')" "bounded create returns a usable directory" "got: $out"
check "$(yn '[ ! -f "$out/.UNBOUNDED" ]')" "bounded create leaves NO unbounded marker" ""
check "$(yn '[ -f "$out/.BOUNDED_RUN_SPACE" ]')" "bounded create records the ownership marker destroy requires" ""
# The readback claim: the live qgroup must actually carry the limit we asked for.
id="$(sudo -n btrfs subvolume show "$out" 2>/dev/null | awk '/Subvolume ID/{print $3; exit}')"
live="$(sudo -n btrfs qgroup show -re --raw "$WORK/mnt" 2>/dev/null | awk -v q="0/$id" '$1==q{print $4}')"
check "$(yn '[ "$live" = "4000000" ]')" "the limit is LIVE on the qgroup, not merely requested" "readback=$live"

"$BRS" report "$out" >/dev/null 2>"$WORK/under.err"; under_rc=$?
check "$(yn '[ "$under_rc" -eq 0 ]')" "report says an under-limit space is still complete" "exit=$under_rc"

# Changing the live limit after create must invalidate the recorded guarantee.
sudo -n btrfs qgroup limit 8M "0/$id" "$WORK/mnt"
"$BRS" report "$out" >/dev/null 2>"$WORK/changed-limit.err"; changed_rc=$?
check "$(yn '[ "$changed_rc" -eq 10 ]')" "report refuses a live limit that differs from the recorded limit" "exit=$changed_rc"
check "$(yn 'grep -q "live qgroup limit" "$WORK/changed-limit.err"')" "limit drift is named as UNBOUNDED" "$(cat "$WORK/changed-limit.err")"
sudo -n btrfs qgroup limit 4000000 "0/$id" "$WORK/mnt"
"$BRS" report "$out" >/dev/null 2>"$WORK/restored-limit.err"; restored_rc=$?
check "$(yn '[ "$restored_rc" -eq 0 ]')" "report accepts the restored recorded limit" "exit=$restored_rc"

# --- 5. the bound actually stops a writer with EDQUOT, and report says so -------
write_result="$(python3 - "$out/flood.log" <<'PY'
import sys
chunk = (b"INFO detcore::scheduler: [scheduler] >>>>>>>\n\n COMMIT turn 1, dettid 3\n" * 200)
w = 0
try:
    with open(sys.argv[1], "wb") as f:
        while w < 500_000_000:
            f.write(chunk); f.flush(); w += len(chunk)
except OSError as exc:
    print(f"errno={exc.errno} logical={w}")
else:
    print(f"errno=none logical={w}")
PY
)"
logical=$(stat -c %s "$out/flood.log" 2>/dev/null || echo 0)
check "$(yn '[ "${logical:-0}" -gt 0 ]')" "the flood wrote something before being stopped" "logical=$logical"
check "$(yn '[[ "$write_result" == errno=122* ]]')" "the writer was stopped by EDQUOT (122), not ENOSPC" "$write_result"
rep="$("$BRS" report "$out" 2>"$WORK/t4.err")"; rrc=$?
check "$(yn '[ "$rrc" -eq 11 ]')" "report exits 11 once the allowance is reached" "exit=$rrc out=$rep"
check "$(yn 'grep -q "ALLOWANCE THRESHOLD REACHED" "$WORK/t4.err"')" "report SAYS the contents may be incomplete" "$(cat "$WORK/t4.err")"
# Compression is why the allowance is a disk number: far more logical bytes get through.
check "$(yn '[ "${logical:-0}" -gt 4000000 ]')" \
      "compressed accounting let MORE than the raw limit through (disk-bytes semantics)" "logical=$logical vs limit=4000000"

# --- 6. destroy reports reclaim and removes the subvolume ------------------------
"$BRS" destroy "$out" 2>"$WORK/t5.err"; drc=$?
check "$(yn '[ "$drc" -eq 0 ]')" "destroy exits 0" "exit=$drc"
check "$(yn '[ ! -d "$out" ]')" "destroy actually removes the subvolume" ""
check "$(yn 'grep -q "reclaimed about" "$WORK/t5.err"')" "destroy reports what came back" "$(cat "$WORK/t5.err")"

# --- 7. non-btrfs base degrades loudly, does not crash ---------------------------
nb=$(mktemp -d /dev/shm/brs-nonbtrfs.XXXXXX 2>/dev/null)
if [ -n "$nb" ] && [ "$(stat -fc %T "$nb")" != "btrfs" ]; then
    out3="$("$BRS" create --limit 16M --tag t9 --base "$nb" 2>"$WORK/t9.err")"; rc3=$?
    check "$(yn '[ "$rc3" -eq 10 ]')" "non-btrfs base exits 10" "exit=$rc3"
    check "$(yn 'grep -q "is not btrfs" "$WORK/t9.err"')" "non-btrfs base names that as the reason" "$(cat "$WORK/t9.err")"
    check "$(yn '[ -d "$out3" ]')" "non-btrfs base still yields a usable directory" ""
else
    fail "non-btrfs base case could not run" "no non-btrfs tmpfs available at /dev/shm"
fi

# --- 8. destroy refuses absent and copied identity markers -----------------------
victim="$WORK/brs-unmarked-victim"
mkdir "$victim"; printf 'keep\n' > "$victim/important"
"$BRS" destroy "$victim" >/dev/null 2>"$WORK/refuse.err"; refuse_rc=$?
check "$(yn '[ "$refuse_rc" -eq 12 ]')" "destroy refuses an unmarked lookalike directory" "exit=$refuse_rc"
check "$(yn '[ -f "$victim/important" ]')" "refused destroy leaves the unowned contents intact" ""
if [ -n "${out3:-}" ] && [ -f "$out3/.UNBOUNDED" ]; then
    cp "$out3/.UNBOUNDED" "$victim/.UNBOUNDED"
    "$BRS" destroy "$victim" >/dev/null 2>"$WORK/copied-marker.err"; copied_rc=$?
    check "$(yn '[ "$copied_rc" -eq 12 ]')" "destroy refuses a marker copied from another space" "exit=$copied_rc"
    check "$(yn '[ -f "$victim/important" ]')" "copied-marker refusal preserves the unowned contents" ""
    "$BRS" destroy "$out3" >/dev/null 2>&1
    rmdir "$nb" 2>/dev/null || true
fi

# --- 9. usage errors -------------------------------------------------------------
"$BRS" create --limit banana --base /tmp >/dev/null 2>&1
check "$(yn '[ "$?" -eq 2 ]')" "a malformed --limit is a usage error (exit 2), never a silent default" ""
"$BRS" create --base /tmp >/dev/null 2>&1
check "$(yn '[ "$?" -eq 2 ]')" "a missing --limit is a usage error, never an unbounded default" ""

# --- 10. END TO END through the current safehermit integration ------------------
SH="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../bin" && pwd)/safehermit"
mkdir -p "$WORK/mnt/safehermit"
SAFEHERMIT_LOG_ROOT="$WORK/mnt/safehermit" "$SH" --sh-report "$WORK/safehermit.report" \
    --sh-no-deadline --sh-disk-limit 4M /bin/sh -c 'printf "safehermit-e2e\\n"; printf "stderr-e2e\\n" >&2' \
    >"$WORK/safehermit.out" 2>"$WORK/safehermit.err"
e2erc=$?
check "$(yn '[ "$e2erc" -eq 0 ]')" "e2e: safehermit propagates the child exit status" "exit=$e2erc"
check "$(yn 'grep -q "safehermit-e2e" "$WORK/safehermit.out"')" "e2e: safehermit preserves child stdout" ""
check "$(yn 'grep -q "bound.disk=APPLIED:4M for log_dir only" "$WORK/safehermit.report"')" \
      "e2e: safehermit reports the applied disk bound and its exact scope" "$(cat "$WORK/safehermit.report")"
safe_dir="$(sed -n 's/^safehermit: log_dir=//p' "$WORK/safehermit.report")"
safe_report="$("$BRS" report "$safe_dir" 2>"$WORK/safe-report.err")"; safe_rc=$?
check "$(yn '[ "$safe_rc" -eq 0 ]')" "e2e: safehermit's retained log directory has a live under-limit qgroup" "exit=$safe_rc $safe_report"
"$BRS" destroy "$safe_dir" >/dev/null 2>"$WORK/safe-destroy.err"; safe_destroy_rc=$?
check "$(yn '[ "$safe_destroy_rc" -eq 0 ] && [ ! -e "$safe_dir" ]')" "e2e: safehermit's bounded log directory can be reclaimed" "exit=$safe_destroy_rc"

total=$(( PASS + ${#FAILURES[@]} ))
if [ ${#FAILURES[@]} -gt 0 ]; then
    echo "FAIL  ${#FAILURES[@]} of $total checks failed:"
    for f in "${FAILURES[@]}"; do echo "  - $f"; done
    exit 1
fi
echo "ok  $PASS/$total checks passed  (bounded and quota-off paths on a loopback; non-btrfs degradation; safehermit wiring)"
