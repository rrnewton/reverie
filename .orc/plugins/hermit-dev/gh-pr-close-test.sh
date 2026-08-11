#!/usr/bin/env bash
# Offline, bidirectional bracket for gh-pr-close.  The fake gh implementation
# cannot reach GitHub; it resolves planted PR records and validates/logs the
# exact final close command.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
wrapper="$here/gh-pr-close"
test_tmp="$(mktemp -d "${TMPDIR:-/tmp}/gh-pr-close-test.XXXXXX")"
trap 'rm -rf -- "$test_tmp"' EXIT

fake_log="$test_tmp/gh.log"
fake_gh="$test_tmp/gh"

cat >"$fake_gh" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail

: "${FAKE_GH_LOG:?}"

if [[ "${1:-}" == "pr" && "${2:-}" == "view" ]]; then
  (($# == 9)) || { echo "fake gh: unexpected pr view argc=$#" >&2; exit 92; }
  [[ "$4" == "--repo" && "$6" == "--json" && "$7" == "number,state" && "$8" == "--jq" ]] || {
    echo "fake gh: unexpected pr view shape: $*" >&2
    exit 92
  }
  [[ "$5" == "rrnewton/hermit" ]] || {
    echo "fake gh: successor was not resolved in the source repo: $5" >&2
    exit 92
  }
  printf 'VIEW %s %s\n' "$3" "$5" >>"$FAKE_GH_LOG"
  case "$3" in
    4242) printf '4242\tOPEN\n' ;;
    4343) printf '4343\tCLOSED\n' ;;
    4444) printf '4444\tMERGED\n' ;;
    4545) printf '4546\tOPEN\n' ;;
    4646) echo "fake gh: planted API failure" >&2; exit 42 ;;
    *) echo "fake gh: no pull request $3" >&2; exit 1 ;;
  esac
  exit 0
fi

if [[ "${1:-}" == "pr" && "${2:-}" == "close" ]]; then
  printf 'CLOSE\n' >>"$FAKE_GH_LOG"
  (($# == 7)) || { echo "fake gh: unexpected pr close argc=$#" >&2; exit 91; }
  [[ "$3" == "${EXPECT_CLOSE_PR:?}" ]] || { echo "fake gh: wrong source PR: $3" >&2; exit 91; }
  [[ "$4" == "--repo" && "$5" == "${EXPECT_CLOSE_REPO:?}" ]] || {
    echo "fake gh: wrong close repo/shape: $*" >&2
    exit 91
  }
  [[ "$6" == "--comment" && "$7" == "${EXPECT_CLOSE_BODY:?}" ]] || {
    echo "fake gh: closing body changed" >&2
    exit 91
  }
  exit 0
fi

echo "fake gh: unsupported command: $*" >&2
exit 90
FAKE
chmod +x "$fake_gh"

export GH_BIN="$fake_gh"
export FAKE_GH_LOG="$fake_log"

pass=0
fail=0

reset_case() {
  : >"$fake_log"
  unset EXPECT_CLOSE_PR EXPECT_CLOSE_REPO EXPECT_CLOSE_BODY || true
}

close_count() {
  local count
  count="$(grep -c '^CLOSE$' "$fake_log" || true)"
  echo "$count"
}

view_count() {
  local count
  count="$(grep -c '^VIEW ' "$fake_log" || true)"
  echo "$count"
}

run_wrapper() {
  case "${1:-}" in
    -R|--repo|--repo=*|-R?*)
      "$wrapper" "$@" >"$test_tmp/out" 2>"$test_tmp/err"
      ;;
    *)
      "$wrapper" --repo rrnewton/hermit "$@" >"$test_tmp/out" 2>"$test_tmp/err"
      ;;
  esac
}

record_pass() {
  echo "ok   [$1] $2"
  pass=$((pass + 1))
}

record_fail() {
  echo "FAIL [$1] $2"
  fail=$((fail + 1))
}

expect_refused() {
  local name="$1"
  local want="$2"
  shift 2
  reset_case
  local rc=0
  run_wrapper "$@" || rc=$?
  if ((rc != 2)); then
    record_fail "$name" "expected exit 2, got $rc"
    sed 's/^/    /' "$test_tmp/err"
    return
  fi
  if ! grep -qF "$want" "$test_tmp/err"; then
    record_fail "$name" "refusal did not cite '$want'"
    sed 's/^/    /' "$test_tmp/err"
    return
  fi
  if [[ "$(close_count)" != "0" ]]; then
    record_fail "$name" "refusal invoked pr close"
    sed 's/^/    /' "$fake_log"
    return
  fi
  record_pass "$name" "refused before close; cited '$want'"
}

expect_allowed_dry() {
  local name="$1"
  local want="$2"
  local expected_views="$3"
  shift 3
  reset_case
  if ! run_wrapper "$@" --dry-run; then
    record_fail "$name" "qualifying dry run was refused"
    sed 's/^/    /' "$test_tmp/err"
    return
  fi
  if ! grep -qF "$want" "$test_tmp/out"; then
    record_fail "$name" "verdict did not contain '$want'"
    sed 's/^/    /' "$test_tmp/out"
    return
  fi
  if [[ "$(close_count)" != "0" ]]; then
    record_fail "$name" "dry run invoked pr close"
    return
  fi
  if [[ "$(view_count)" != "$expected_views" ]]; then
    record_fail "$name" "expected $expected_views successor lookup(s), got $(view_count)"
    sed 's/^/    /' "$fake_log"
    return
  fi
  record_pass "$name" "allowed dry run; $expected_views bound lookup(s), zero close calls"
}

expect_fake_close() {
  local name="$1"
  local source_pr="$2"
  local body="$3"
  local expected_views="$4"
  shift 4
  reset_case
  export EXPECT_CLOSE_PR="$source_pr"
  export EXPECT_CLOSE_REPO="rrnewton/hermit"
  export EXPECT_CLOSE_BODY="$body"
  if ! run_wrapper "$source_pr" "$@"; then
    record_fail "$name" "qualifying close did not reach the exact fake command"
    sed 's/^/    /' "$test_tmp/err"
    return
  fi
  if [[ "$(close_count)" != "1" ]]; then
    record_fail "$name" "expected exactly one fake pr close call"
    sed 's/^/    /' "$fake_log"
    return
  fi
  if [[ "$(view_count)" != "$expected_views" ]]; then
    record_fail "$name" "expected $expected_views successor lookup(s), got $(view_count)"
    sed 's/^/    /' "$fake_log"
    return
  fi
  record_pass "$name" "issued $expected_views bound lookup(s) and one byte-checked fake close"
}

# Original refusal bracket.
expect_refused rule1-bare-close "(rule 1)" 1635 --comment "Closing this."
expect_refused rule1-silent-close "(rule 1)" 1635 --comment "   "
expect_refused rule2-procedural "(rule 2)" 1635 \
  --comment "Closing: this PR is procedurally deficient."
expect_refused rule2-generic-quality "(rule 2)" 1635 \
  --comment "Closing, the change does not meet our standard."
expect_refused rule3-task-instead "(rule 3)" 1635 \
  --comment "Closing; I filed a task in the taskgraph to track the rework."
expect_refused rule3-tg-note "(rule 3)" 1635 \
  --comment "Superseded conceptually; see tg note follow-up-work for the plan."
expect_refused successor-unresolvable "does not resolve" 1635 \
  --comment "SUPERSEDED-BY: #9999"
expect_refused successor-is-self "same PR" 1635 \
  --comment "SUPERSEDED-BY: #1635"
expect_refused upstream-repo "upstream" \
  --repo facebookexperimental/hermit 1635 --comment "SUPERSEDED-BY: #4242"

# Current-policy deltas and ambiguity controls.
expect_refused successor-missing-hash "(rule 1)" 1635 \
  --comment "SUPERSEDED-BY: 4242"
expect_refused successor-malformed-suffix "(rule 1)" 1635 \
  --comment "SUPERSEDED-BY: #4242oops"
expect_refused successor-closed "not OPEN" 1635 \
  --comment "SUPERSEDED-BY: #4343"
expect_refused successor-merged "not OPEN" 1635 \
  --comment "SUPERSEDED-BY: #4444"
expect_refused successor-number-mismatch "did not bind" 1635 \
  --comment "SUPERSEDED-BY: #4545"
expect_refused successor-api-failure "does not resolve" 1635 \
  --comment "SUPERSEDED-BY: #4646"
expect_refused dispositions-both "multiple or conflicting" 1635 \
  --comment "SUPERSEDED-BY: #4242 OWNER-AUTHORIZED-CLOSE: owner said close"
expect_refused successors-conflict "multiple or conflicting" 1635 \
  --comment "SUPERSEDED-BY: #4242; SUPERSEDED-BY: #4343"
expect_refused successors-duplicate "multiple or conflicting" 1635 \
  --comment "SUPERSEDED-BY: #4242; again SUPERSEDED-BY: #4242"
expect_refused owner-placeholder "placeholder" 1635 \
  --comment "OWNER-AUTHORIZED-CLOSE: <quote the owner instruction>"
expect_refused dual-comment-input "exactly one" 1635 \
  --comment "SUPERSEDED-BY: #4242" --comment-file /dev/null
expect_refused delete-branch "branch deletion" 1635 \
  --comment "SUPERSEDED-BY: #4242" --delete-branch
expect_refused unknown-flag "unsupported argument" 1635 \
  --comment "SUPERSEDED-BY: #4242" --mystery

# Original positive bracket.  A valid disposition outranks incidental prose.
expect_allowed_dry named-successor "superseded by open PR #4242" 1 1635 \
  --comment "Replaced by a complete implementation. SUPERSEDED-BY: #4242"
expect_allowed_dry owner-authorized "owner authorization" 0 1635 \
  --comment "OWNER-AUTHORIZED-CLOSE: owner asked for this to be abandoned on 2026-08-07."
expect_allowed_dry reverie-owner-authorized "owner authorization" 0 \
  --repo rrnewton/reverie 1635 \
  --comment "OWNER-AUTHORIZED-CLOSE: owner requested closure in this fork."
expect_allowed_dry successor-beats-prose "superseded by open PR #4242" 1 1635 \
  --comment "Procedurally deficient here; tracked in a task too. SUPERSEDED-BY: #4242"
printf 'Rewritten cleanly.\nSUPERSEDED-BY: #4242\n' >"$test_tmp/comment.txt"
expect_allowed_dry comment-file-path "superseded by open PR #4242" 1 1635 \
  --comment-file "$test_tmp/comment.txt"

# Exercise the real mutation branch against the offline fake.  The fake checks
# the exact source, repo, body, argument count, and absence of delete flags.
successor_body=$'Replacement carries the complete work.\nSUPERSEDED-BY: #4242'
expect_fake_close live-successor-shape 1635 "$successor_body" 1 \
  --comment "$successor_body"
owner_body=$'OWNER-AUTHORIZED-CLOSE: owner requested closure.\n'
printf '%s' "$owner_body" >"$test_tmp/owner-comment.txt"
expect_fake_close live-owner-file-shape 1635 "$owner_body" 0 \
  --comment-file "$test_tmp/owner-comment.txt"

echo
echo "gh-pr-close-test: $pass passed, $fail failed"
echo "  fake view calls in final case: $(view_count)"
echo "  fake close calls in final case: $(close_count)"
((fail == 0)) || exit 1
