#!/usr/bin/env bash
# INERT both-sided bracket of the landing eligibility predicate.
#
# `local-validation-eligibility.sh` exposes the local-receipt predicate used by
# the normal Hermit exact-head VALIDATE/LEDGER authority. The GitHub
# `locally-validated` label is a CACHE of that result; the LEDGER is the truth
# (#231/#243). This test exercises the local predicate DIRECTLY against synthetic
# ledger fixtures in a temp dir, then audits the production combiner and lander:
# no PR is read, no label is written, no merge is attempted, and no authorization
# artifact is ever planted on live state.
#
# Both legs are required, and the second is the one usually skipped:
#   NEGATIVE  an unbacked / stale-SHA / tampered / known-failing record is
#             REFUSED (proves the guard is not one that never fires), and
#   POSITIVE  a genuinely ledger-backed exact head is still ADMITTED (proves the
#             guard is not one that refuses everything).
# Counts for both sides are printed at the end.
#
# The exit-code contract under test (see lib/validate_status.rs Verdict):
#   0 ELIGIBILITY=VALIDATED      2 ELIGIBILITY=ERROR / usage
#   3 ELIGIBILITY=KNOWN_FAILED   4 ELIGIBILITY=NOT_VALIDATED
# The lander must land on 0 only; every other code is a refusal.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(cd -- "$script_dir/../.." && pwd)
helper=$script_dir/local-validation-eligibility.sh
lander=$script_dir/land-pr.sh
review_applicability=$script_dir/review_rebase.py
publisher=$script_dir/publish_comment.py
authority=$script_dir/exact-head-validation-authority.sh
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
ledger=$tmp/ledger.jsonl
canonical_root=$tmp/canonical

# HEAD is a real Hermit commit: the authoritative consumer reads commits from
# Git, so a nonexistent synthetic SHA can never be a positive fixture.  The
# ledger and labels remain inert temp-dir plants; only commit identity comes
# from the pinned Hermit object store.  No registered producer map exists
# (https://github.com/rrnewton/dev-hermit/issues/414).
producer_repo=$root/hermit
HEAD_SHA=ce1aca3aba3a4bb1db244e1ca2ab6903ea4bb7a8
HEAD_TREE=$(git -C "$producer_repo" rev-parse "$HEAD_SHA^{tree}")
# OTHER and THIRD stand for stale receipts. The canonical row store now proves
# that every named commit exists in row.cwd, so use real ancestors rather than
# syntactically valid but nonexistent fixture hashes.
OTHER_SHA=$(git -C "$producer_repo" rev-parse "$HEAD_SHA^")
THIRD_SHA=$(git -C "$producer_repo" rev-parse "$HEAD_SHA^^")

neg_refused=0
neg_total=0
pos_admitted=0
pos_total=0
fail=0
check_index=0
# Sub-tallies for the label-independence pair reported in the legacy summary
# line that ci-hub/tests/test_operational_bounds.py asserts on.
unbacked_refused=0
backed_admitted=0
LAST_STATUS=

# A fully qualifying schema-5 clean full-coverage PASS row for $1.
pass_row() {
  jq -cn --arg c "$1" --arg cwd "$producer_repo" --arg tree "$HEAD_TREE" '{
    schema_version: 5, repo: "hermit", commit: $c,
    tree: $tree, cwd: $cwd,
    commit_anchored: true, tree_dirty: false,
    profile: "full", selection_mode: "full", result: "pass", raw_result: "pass",
    exit_code: 0, checks: 1, failures: 0, gates_run: 1, gates_expected: 1,
    gates: [{name: "fixture", result: "pass", exit_code: 0}],
    executed_tests: 42, passed_tests: 42, filtered_tests: 0,
    producer: "hermit-validate-sh",
    admission: "ci-hub-validate-lock",
    concurrent_validates: 0,
    concurrency_proof: "validate_lock_owner_ancestry",
    base_sha: ("1" * 40),
    base_tree: ("2" * 40),
    reverie_base_sha: ("3" * 40),
    reverie_base_tree: ("4" * 40),
    coverage: {planned_test_nodes: 1, executed_test_nodes: 1,
               zero_executed_nodes: [], absent_nodes: []},
    started_at: "2026-08-03T23:59:59Z", finished_at: "2026-08-04T00:00:00Z",
    host: "fixture", slot: "fixture-slot", log_file: "/durable/fixture.log",
    real_seconds: 600
  }'
}

# A DURABLE failing row for $1: clean+full+anchored, complete five-gate run, a
# named red gate with bound origin, solo and non-flaky. This is the only shape
# that reaches KNOWN_FAILED (rc 3) rather than a re-measurement request.
fail_row() {
  jq -cn --arg c "$1" --arg cwd "$producer_repo" --arg tree "$HEAD_TREE" '{
    schema_version: 5, repo: "hermit", commit: $c, tree: $tree, cwd: $cwd,
    commit_anchored: true, tree_dirty: false,
    profile: "full", selection_mode: "full", result: "fail",
    executed_tests: 100, filtered_tests: 0, failures: 1,
    gates_run: 5, gates_expected: 5,
    gates: [{name: "detcore", result: "fail", exit_code: 101, real_seconds: 42.0,
             failure_origin: "outer_gate", failed_substeps: []}],
    dag_jobs: 4, concurrent_validates: 0, known_flaky_failure: false,
    exit_code: 1, finished_at: "2026-08-04T00:00:00Z", host: "fixture",
    real_seconds: 600
  }'
}

# pass_row for HEAD_SHA with one field tampered, per the jq expression in $1.
tampered_row() { pass_row "$HEAD_SHA" | jq -c "$1"; }

# check <side> <expected_rc> <expected_verdict|-> <name> -- <helper args...>
# Runs the predicate against whatever is currently in $ledger (unless the caller
# exported CI_HUB_VALIDATE_STATUS_BIN) and records the outcome on the right side.
check() {
  local side=$1 expected_rc=$2 expected_verdict=$3 name=$4; shift 5
  local output rc verdict status=OK check_root
  check_index=$((check_index + 1))
  check_root=$canonical_root/check-$check_index
  mkdir -p -- "$check_root"
  # Every bracket gets its own canonical root. The event writer stores live
  # rows outside ledger/, so deleting only that published path let a preceding
  # same-SHA negative contaminate the following positive control.
  ROOT="$root" CANONICAL_ROOT="$check_root" RAW_LEDGER="$ledger" python3 -c '
import json, os, sys
from pathlib import Path
sys.path.insert(0, str(Path(os.environ["ROOT"]) / "ci-hub" / "ledger"))
import validate_rows
source = Path(os.environ["RAW_LEDGER"])
if source.is_file():
    for line in source.read_text().splitlines():
        if line.strip():
            validate_rows.record(json.loads(line), root=Path(os.environ["CANONICAL_ROOT"]))
'
  set +e
  output=$(HERMIT_VALIDATE_STOP_TEST_MODE=1 \
    CI_HUB_VALIDATE_LEDGER_TEST_ROOT="$check_root" \
    "$helper" "$@" 2>&1)
  rc=$?
  set -e
  verdict=$(grep '^ELIGIBILITY=' <<<"$output" | tail -1 || true)
  verdict=${verdict:-<none>}
  [ "$rc" -eq "$expected_rc" ] || status=BAD
  if [ "$expected_verdict" != "-" ] && [ "$verdict" != "$expected_verdict" ]; then
    status=BAD
  fi
  # A refusal that leaks VALIDATED anywhere in its output is not a refusal.
  if [ "$side" = NEG ] && { [ "$rc" -eq 0 ] || grep -q 'ELIGIBILITY=VALIDATED' <<<"$output"; }; then
    status=BAD
  fi
  if [ "$side" = NEG ]; then
    neg_total=$((neg_total + 1))
    [ "$status" = OK ] && neg_refused=$((neg_refused + 1))
  else
    pos_total=$((pos_total + 1))
    [ "$status" = OK ] && pos_admitted=$((pos_admitted + 1))
  fi
  LAST_STATUS=$status
  printf '%-4s %-4s rc=%-2s %-26s %s\n' "$status" "$side" "$rc" "$verdict" "$name"
  if [ "$status" = BAD ]; then
    fail=1
    printf '     expected rc=%s verdict=%s; full output:\n%s\n' \
      "$expected_rc" "$expected_verdict" "$output" >&2
  fi
}

echo "== NEGATIVE leg: an unbacked, stale, tampered, or failing record is REFUSED =="

# --- unbacked exact head: nothing at all in the ledger --------------------
: >"$ledger"
check NEG 4 ELIGIBILITY=NOT_VALIDATED "unbacked head, no label" -- \
  "$HEAD_SHA" ""
[ "$LAST_STATUS" = OK ] && unbacked_refused=$((unbacked_refused + 1))
check NEG 4 ELIGIBILITY=NOT_VALIDATED "unbacked head, locally-validated PLANTED" -- \
  "$HEAD_SHA" "locally-validated"
[ "$LAST_STATUS" = OK ] && unbacked_refused=$((unbacked_refused + 1))

# --- STALE-SHA: a real, fully qualifying PASS for a DIFFERENT commit -------
# This is the #231/#243 hazard in its sharpest form: the receipt is genuine and
# green, it is simply not for the head being landed (a rebase moved the SHA).
pass_row "$OTHER_SHA" >"$ledger"
check NEG 4 ELIGIBILITY=NOT_VALIDATED "STALE-SHA pass for other commit, no label" -- \
  "$HEAD_SHA" ""
check NEG 4 ELIGIBILITY=NOT_VALIDATED "STALE-SHA pass + locally-validated PLANTED" -- \
  "$HEAD_SHA" "locally-validated"
# ...and the stale receipt is not laundered by piling on more foreign greens.
{ pass_row "$OTHER_SHA"; pass_row "$THIRD_SHA"; } >"$ledger"
check NEG 4 ELIGIBILITY=NOT_VALIDATED "STALE-SHA: two foreign passes, none for head" -- \
  "$HEAD_SHA" "locally-validated"

# --- TAMPERED: a record FOR the head with one qualifying condition broken ---
# Each is a well-shaped record a careless or hostile producer could write.
while IFS='|' read -r label expr; do
  [ -n "$label" ] || continue
  tampered_row "$expr" >"$ledger"
  check NEG 4 ELIGIBILITY=NOT_VALIDATED "TAMPERED $label" -- \
    "$HEAD_SHA" "locally-validated"
done <<'CASES'
tree_dirty=true|.tree_dirty = true
commit_anchored=false|.commit_anchored = false
profile=fast|.profile = "fast"
selection_mode=affected|.selection_mode = "affected"
result=fail (bare)|.result = "fail"
executed_tests=0|.executed_tests = 0
executed_tests absent|del(.executed_tests) | del(.passed_tests)
passed_tests absent|del(.passed_tests)
passed_tests null|.passed_tests = null
passed_tests mismatched|.passed_tests = 41
coverage absent|del(.coverage)
coverage.zero_executed_nodes|.coverage.zero_executed_nodes = ["detcore"]
coverage.absent_nodes|.coverage.absent_nodes = ["detcore"]
coverage.planned_test_nodes=0|.coverage.planned_test_nodes = 0
failures=1|.failures = 1
base_sha absent|del(.base_sha)
schema 3, counts stripped|.schema_version = 3 | del(.executed_tests) | del(.passed_tests) | del(.filtered_tests)
CASES

# --- KNOWN-FAILING: a durable clean full-coverage FAIL for the exact head ---
fail_row "$HEAD_SHA" >"$ledger"
check NEG 3 ELIGIBILITY=KNOWN_FAILED "durable FAIL for head + label PLANTED" -- \
  "$HEAD_SHA" "locally-validated"

# A qualifying PASS does not erase a durable failure for the same exact SHA.
# Keep this separate from the positive foreign-row bracket below: the two
# controls exercise failure latching and exact-head selection respectively.
{ fail_row "$HEAD_SHA"; pass_row "$HEAD_SHA"; } >"$ledger"
check NEG 3 ELIGIBILITY=KNOWN_FAILED "same-SHA failure remains latched beside pass" -- \
  "$HEAD_SHA" "locally-validated"

# --- malformed input: refused before any ledger is consulted ---------------
pass_row "$HEAD_SHA" >"$ledger"   # a real green is present; input must still be rejected
check NEG 2 - "malformed sha: uppercase" -- "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" ""
check NEG 2 - "malformed sha: 39 hex" -- "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" ""
check NEG 2 - "malformed sha: 41 hex" -- "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" ""
check NEG 2 - "malformed sha: non-hex" -- "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz" ""
check NEG 2 - "malformed sha: empty" -- "" ""
check NEG 2 - "usage: no arguments" --
check NEG 2 - "usage: too many arguments" -- "$HEAD_SHA" "" extra

# --- fail-closed when the authority itself cannot be consulted -------------
# The verdict comes from ci-hub validate-status; if that cannot be run or
# answers outside the contract, the predicate must ERROR, never admit.
printf '#!/bin/sh\nexit 1\n' >"$tmp/rc1"; chmod +x "$tmp/rc1"
printf '#!/bin/sh\nexit 5\n' >"$tmp/rc5"; chmod +x "$tmp/rc5"
CI_HUB_VALIDATE_STATUS_BIN=$tmp/does-not-exist \
  check NEG 2 - "fail-closed: authority binary missing" -- "$HEAD_SHA" "locally-validated"
CI_HUB_VALIDATE_STATUS_BIN=$tmp/rc1 \
  check NEG 2 - "fail-closed: authority rc=1" -- "$HEAD_SHA" "locally-validated"
CI_HUB_VALIDATE_STATUS_BIN=$tmp/rc5 \
  check NEG 2 - "fail-closed: authority rc=5 (off-contract)" -- "$HEAD_SHA" "locally-validated"

echo
echo "== POSITIVE leg: a genuinely ledger-backed exact head is still ADMITTED =="

pass_row "$HEAD_SHA" >"$ledger"
check POS 0 ELIGIBILITY=VALIDATED "backed head, NO label (ledger is the authority)" -- \
  "$HEAD_SHA" ""
[ "$LAST_STATUS" = OK ] && backed_admitted=$((backed_admitted + 1))
check POS 0 ELIGIBILITY=VALIDATED "backed head, label present" -- \
  "$HEAD_SHA" "locally-validated"
[ "$LAST_STATUS" = OK ] && backed_admitted=$((backed_admitted + 1))
check POS 0 ELIGIBILITY=VALIDATED "backed head, unrelated labels only" -- \
  "$HEAD_SHA" "mechanism:landing,post-facto-human-review"
# The exact-head green must survive being surrounded by foreign PASS and FAIL
# records. Keep the durable failure on a distinct SHA: this bracket tests
# exact-head selection, not the separate same-SHA failure-latching policy.
{ pass_row "$OTHER_SHA"; fail_row "$THIRD_SHA"; pass_row "$HEAD_SHA"; } >"$ledger"
check POS 0 ELIGIBILITY=VALIDATED "backed head amid foreign pass/fail rows" -- \
  "$HEAD_SHA" "locally-validated"

echo
echo "== CONSUMER AUDIT: the lander defers to this predicate, and nothing else merges =="

audit_fail=0
audit() { # audit <description> <ok|bad>
  if [ "$2" = ok ]; then printf 'OK   AUDIT %s\n' "$1"
  else printf 'BAD  AUDIT %s\n' "$1"; audit_fail=1; fi
}

# 1. All mutation boundaries invoke the ONE landability consumer: before JIT,
# after JIT, after push, and immediately before merge. The consumer, not this
# shell entrypoint, selects hard- or soft-green under hermit-validation-authority.
mapfile -t authority_lines < <(grep -n 'validation_landability "\$' "$lander" || true)
# Four logical boundaries produce five textual calls because the final boundary
# has mutually exclusive Hermit receipt-comment and Reverie local-row arms.
if [ "${#authority_lines[@]}" -eq 5 ] &&
   [ "$(grep -c 'VRC=\$?' "$lander")" -ge 2 ] &&
   grep -Fq 'receipt_rc=$?' "$lander" &&
   grep -Fq 'rebase_wrapper.py" authorize' "$lander"; then
  audit "land-pr.sh: 4 logical validation-landability boundaries include the post-JIT recheck" ok
else
  audit "land-pr.sh: expected 5 repository-routed call sites, found ${#authority_lines[@]}" bad
fi
if grep -Fq '[ "$plain_rebase" -eq 1 ] && [ "$HEAD" != "$ORIG" ]' "$lander" &&
   grep -Fq 'rebase_wrapper.py" record' "$lander"; then
  audit "rewritten plain rebases record provenance; no-op rebases preserve the prior row" ok
else
  audit "plain rebase provenance recording can overwrite a no-op soft-green row" bad
fi

# The combiner independently dereferences semantic local and hosted status. It
# must not reintroduce the legacy label-shaped helper as an authority.
if grep -Fq 'local-validation-eligibility.sh' "$authority"; then
  audit "exact-head authority calls the legacy label helper" bad
elif grep -Fq 'validate-status' "$authority" && grep -Fq 'hosted-status' "$authority"; then
  audit "exact-head authority dereferences both semantic status verifiers" ok
else
  audit "exact-head authority lost a semantic status verifier" bad
fi

if grep -Fq 'validation_landability "$HEAD" "$current_base"' "$lander" &&
   grep -Fq -- '--current-reverie-base "$current_reverie_base"' "$lander" &&
   grep -Fq -- '--reverie-checkout "$reverie_checkout"' "$lander" &&
   grep -Fq -- '--repo-checkout "$WT"' "$lander"; then
  audit "final authority call carries both freshly resolved bases into the shared verifier" ok
else
  audit "final authority call lost merge-boundary base evidence" bad
fi

if grep -Fq 'health/approval_binding.py"' "$lander" &&
   grep -Fq -- '--repo "$R" --pr "$PR" --json' "$lander" &&
   grep -Fq 'review_rebase.py"' "$lander" &&
   grep -Fq -- '--current-base "$current_base" --current-head "$HEAD"' "$lander" &&
   grep -Fq 'review applicability REFUSED' "$lander" &&
   ! grep -Fq 'row.get("source")' "$review_applicability" &&
   ! grep -Fq 'row.get("label")' "$review_applicability" &&
   grep -Fq 'state == BOUND_CURRENT' "$review_applicability" &&
   grep -Fq 'approval-binding report contains no accepted review approval' "$review_applicability"; then
  audit "final boundary uses canonical binding state, ignores label cache metadata, and refuses an empty review set" ok
else
  audit "final boundary lost canonical binding state, cache independence, or empty-set refusal" bad
fi

# The just-in-time producer moved into `attempt_jit_exact_head_validation`, so
# its head is the function's `$candidate` parameter rather than `$ORIG`. Same
# producer, same current-base precondition, same post-run recheck -- the two
# renamed literals below are the parameter, not a relaxed check. The added
# clauses are the reason it was factored: it is now reachable from BOTH the
# pre-check and the no-op-rebase discharge, and one copy cannot drift from the
# other.
if grep -Fq 'bin/pr-head-ci"' "$lander" &&
   grep -Fq -- '--expected-head "$candidate"' "$lander" &&
   grep -Fq -- '--checkout "$WT" --agent "$AGENT" --execute' "$lander" &&
   grep -Fq 'merge-base --is-ancestor "$eligibility_base" "$candidate"' "$lander" &&
   grep -Fq 'post-JIT validation landability' "$lander" &&
   grep -Fq 'attempt_jit_exact_head_validation "$ORIG"' "$lander" &&
   grep -Fq 'attempt_jit_exact_head_validation "$HEAD"' "$lander"; then
  audit "missing exact-head authority invokes centralized JIT only on a current-base head, then rechecks landability, from both the pre-check and the no-op-rebase discharge" ok
else
  audit "landing frontier has no exact-head centralized JIT producer or skips its post-run authority recheck" bad
fi

if ! grep -Fq 'cherry_pick_context.py"' "$lander" &&
   ! grep -Fq 'cherry-pick context contract REFUSED' "$lander" &&
   grep -Fq 'review_rebase.py"' "$lander"; then
  audit "final boundary relies on approval bindings plus range-diff, not cherry-pick digest ceremony" ok
else
  audit "cherry-pick digest ceremony still blocks a reviewed patch series" bad
fi

merge_prefix='gh pr '
merge_suffix='merge "$PR"'
if [ "$(grep -Fc "${merge_prefix}${merge_suffix}" "$lander")" -eq 1 ] &&
   ! grep -Fq 'for mtries' "$lander"; then
  audit "one merge attempt consumes each freshly evaluated boundary verdict" ok
else
  audit "a merge retry can outlive its freshly evaluated boundary verdict" bad
fi

# 2. The lander refuses on each non-zero code rather than only on rc 3/4.
for pat in \
  'validation landability refused PR head' \
  'has no accepted validation landability' \
  'validation landability REFUSED'; do
  if grep -Fq "$pat" "$lander"; then
    audit "land-pr.sh abandons on: $pat" ok
  else
    audit "land-pr.sh lost its abandon path for: $pat" bad
  fi
done

# 3. The lander never types the cache label itself; only the ledger-guarded
#    applier may materialize it.
if grep -Eq 'gh pr edit .*--add-label locally-validated' "$lander"; then
  audit "land-pr.sh directly types locally-validated" bad
else
  audit "land-pr.sh does not type the cache label directly" ok
fi
if grep -Fq 'apply-local-label' "$lander"; then
  audit "land-pr.sh derives the label via ledger-guarded apply-local-label" ok
else
  audit "land-pr.sh no longer routes the label through apply-local-label" bad
fi
if grep -Fq 'ledger-guarded apply-local-label failed' "$lander" ||
   grep -Fq 'locally-validated stripped immediately' "$lander"; then
  audit "derived locally-validated cache can still veto exact-head authority" bad
else
  audit "locally-validated cache reconciliation is non-blocking" ok
fi

# 3a. Owner checks remain while accumulated ceremony is absent.
if grep -Eq 'gate[_-]definition[_-]rotation|--gate-rotation|GATE_ROTATION' "$lander"; then
  audit "mandatory gate rotation remains in the reviewer landing path" bad
else
  audit "merge-gate.yml changes use the ordinary exact-review + receipt path" ok
fi
if grep -Fq 'authority_mode=hosted-only' "$lander" ||
   grep -Fq 'producer-changing PR requires --no-rebase' "$lander"; then
  audit "validate-producer changes still enter the Actions-off hosted dead end" bad
else
  audit "validate-producer changes use local-only authority like other PR heads" ok
fi
if ! grep -Eq 'land_and_arm\.py|inherit-obligations|resolve-obligation' "$lander"; then
  audit "lander invokes no removed post-land verification commands" ok
else
  audit "lander still invokes removed post-land verification commands" bad
fi
if grep -Fq -- '--checkout "$WT"' "$lander" &&
   grep -Fq 'DG_AGENT_NAME' "$lander" && grep -Fq '"reviewer"' "$publisher"; then
  audit "approving reviewer can land from their own identified slot" ok
else
  audit "lander still requires a separate activator identity or checkout" bad
fi

# 3b. The predicate's authority is only as strong as the environment it runs in:
#     CI_HUB_VALIDATE_STATUS_BIN replaces the authority binary. That override
#     makes one fail-closed bracket possible, but a real landing must not inherit
#     it. The ledger fixture instead uses the adapter's stop-test-only root, which
#     the real producer path can never enter. The lander clears the binary override
#     before anything reads it; assert the clear is still there and still
#     ahead of the first read.
# `|| true` is load-bearing: under `set -euo pipefail` a non-matching grep would
# abort the script here, and a guard that crashes is not a guard that fired.
unset_line=$( { grep -n '^unset CI_HUB_VALIDATE_STATUS_BIN$' "$lander" || true; } | head -1 | cut -d: -f1)
first_read=$( { grep -n 'exact-head-validation-authority\.sh"\|scan-finalize\.sh' "$lander" || true; } | head -1 | cut -d: -f1)
if [ -n "$unset_line" ] && [ -n "$first_read" ] && [ "$unset_line" -lt "$first_read" ]; then
  audit "land-pr.sh clears the validate binary override (line $unset_line) before first read (line $first_read)" ok
else
  audit "land-pr.sh does not clear CI_HUB_VALIDATE_STATUS_BIN before use (unset=${unset_line:-none} first-read=${first_read:-none})" bad
fi

# 4. COORDINATOR DECISION: no merge path outside land-pr.sh. A bare label can
#    still authorize at the merge gate, so the hole is closed structurally by
#    keeping exactly one merge call site behind this predicate. Any other
#    executable merge invocation in the parent must be reported, not used.
# A file-level allowlist cannot tell an INVOCATION from a quoted MENTION, so it
# had to blanket-exempt whole assertion files -- which would equally have hidden
# a real merge call added to one of them later. Classify per LINE instead, and
# allowlist only the one file permitted to actually merge.
#
#   invocation : `gh pr merge` reached as a command, i.e. not immediately
#                preceded by a quote, or reached indirectly through `eval`.
#   mention    : `gh pr merge` inside a quoted literal (an assertion about the
#                lander, or an error string), which authorizes nothing.
#
# Bracketed by `merge_path_classifier_fixtures` below: every fixture shape is
# asserted in BOTH directions before this audit is trusted.
allowed_merge_files="ci-hub/landing/land-pr.sh"
merge_invocation_re='^[^#]*(^|[^'"'"'"])gh +pr +merge'
# Indirect invocation: the command is reached through an interpreter, so the
# quote in front of it means nothing. This is the hole a bare quote-heuristic
# would leave, and it is bracketed by the `system(...)`/`eval` fixtures below.
merge_eval_re='^[^#]*(eval|system|popen|subprocess|check_output|check_call|sh +-c|bash +-c)[^#]*gh +pr +merge'
if merge_files=$(git -C "$root" grep -lI -E '^[^#]*gh +pr +merge' -- \
      '*.sh' '*.py' '*.rs' '*.bash' ':!hermit' ':!reverie' ':!liteinst2' ':!agent-utils' 2>/dev/null); then
  unexpected=""; mentions=0
  while read -r f; do
    [ -n "$f" ] || continue
    case " $allowed_merge_files " in *" $f "*) continue ;; esac
    if git -C "$root" grep -qI -E "$merge_invocation_re" -- "$f" 2>/dev/null ||
       git -C "$root" grep -qI -E "$merge_eval_re" -- "$f" 2>/dev/null; then
      unexpected="$unexpected $f"
    else
      mentions=$((mentions + 1))
    fi
  done <<<"$merge_files"
  if [ -z "$unexpected" ]; then
    audit "sole executable merge path is land-pr.sh ($mentions quoted mention-only file(s) inspected, 0 invocations)" ok
  else
    audit "REPORT: merge invocation outside land-pr.sh:$unexpected" bad
  fi
else
  # A guard that cannot run is not a guard that passed.
  audit "merge-path audit could not run (git grep failed from $root)" bad
fi

# Bracket the classifier itself, both ways, against inert fixture text. These
# strings are never executed and live only in this process: a fixture that
# merges nothing cannot authorize the merge whose refusal it tests.
merge_path_classifier_fixtures() {
  local pos=0 neg=0 bad=0 line
  # The fixture command is ASSEMBLED AT RUNTIME so this harness never contains
  # the literal token unquoted -- otherwise the audit above would flag its own
  # test data and the guard would fail on itself.
  local m="gh pr"" merge"
  local -a invocations=(
    "out=\$(with-proxy $m \"\$PR\" -R \"\$R\" --rebase)"
    "$m 123 --admin"
    "  eval \"$m \$PR --rebase\""
    "system(\"$m\")"
    "subprocess.check_call(\"$m 7\", shell=True)"
  )
  local -a mentions=(
    "    assert lander.count('$m \"\$PR\"') == 1"
    "    assert '--match-head-commit' in lander and \"$m\" in lander"
    "    merge = script.index('$m \"\$PR\"')"
    "abandon \"$m --rebase refused after the fresh final-boundary check\""
    "# $m is documented here in a comment"
  )
  for line in "${invocations[@]}"; do
    if printf '%s\n' "$line" | grep -qE "$merge_invocation_re" ||
       printf '%s\n' "$line" | grep -qE "$merge_eval_re"; then
      pos=$((pos + 1))
    else
      bad=$((bad + 1)); echo "  MISSED invocation: $line" >&2
    fi
  done
  for line in "${mentions[@]}"; do
    if printf '%s\n' "$line" | grep -qE "$merge_invocation_re" ||
       printf '%s\n' "$line" | grep -qE "$merge_eval_re"; then
      bad=$((bad + 1)); echo "  FALSE POSITIVE on mention: $line" >&2
    else
      neg=$((neg + 1))
    fi
  done
  if [ "$bad" -eq 0 ]; then
    audit "merge-path classifier bracketed: ${pos}/${#invocations[@]} invocations flagged, ${neg}/${#mentions[@]} quoted mentions cleared" ok
  else
    audit "merge-path classifier MISCLASSIFIED $bad fixture(s)" bad
  fi
}
merge_path_classifier_fixtures

echo
# Legacy label-independence summary, kept verbatim so existing consumers keep
# working; the counts below are the full both-sided tally.
printf 'unbacked label rejected %d/2; validated head admitted %d/2\n' \
  "$unbacked_refused" "$backed_admitted"
printf 'NEGATIVE refusals: %d/%d   POSITIVE admissions: %d/%d\n' \
  "$neg_refused" "$neg_total" "$pos_admitted" "$pos_total"
if [ "$unbacked_refused" -ne 2 ] || [ "$backed_admitted" -ne 2 ]; then
  fail=1
fi
if [ "$fail" -ne 0 ] || [ "$audit_fail" -ne 0 ] ||
   [ "$neg_refused" -ne "$neg_total" ] || [ "$pos_admitted" -ne "$pos_total" ]; then
  echo "FAIL: local-validation eligibility bracket" >&2
  exit 1
fi
echo "PASS: local eligibility refuses every unbacked/stale/tampered/failing head and still admits a ledger-backed one; lander uses the exact-head OR authority; no merge path outside land-pr.sh"
