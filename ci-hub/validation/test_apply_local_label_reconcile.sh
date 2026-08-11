#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(cd -- "$script_dir/../.." && pwd)
hub=$root/ci-hub/ci-hub
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

stale_pr=101
absent_pr=102
stale_head=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
absent_head=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

mkdir -p "$tmp/bin"
cat >"$tmp/bin/with-proxy" <<'EOF'
#!/usr/bin/env bash
exec "$@"
EOF
cat >"$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$FAKE_GH_LOG"

if [[ ${1:-} == pr && ${2:-} == list ]]; then
    printf '%s\n%s\n' "$STALE_PR" "$ABSENT_PR"
    exit 0
fi

if [[ ${1:-} == pr && ${2:-} == view ]]; then
    pr=${3:-}
    case "$pr" in
        "$STALE_PR")
            jq -cn --arg head "$STALE_HEAD" '{
              headRefOid: $head,
              labels: [{name: "locally-validated"}]
            }'
            ;;
        "$ABSENT_PR")
            jq -cn --arg head "$ABSENT_HEAD" '{headRefOid: $head, labels: []}'
            ;;
        *) exit 91 ;;
    esac
    exit 0
fi

if [[ ${1:-} == pr && ${2:-} == edit ]]; then
    printf '%s\n' "$*" >>"$FAKE_MUTATION_LOG"
    exit 0
fi

printf 'unsupported fake gh invocation: %s\n' "$*" >&2
exit 92
EOF
chmod +x "$tmp/bin/with-proxy" "$tmp/bin/gh"

run_reconcile() {
    PATH="$tmp/bin:$PATH" \
    CI_HUB_TOOL_COST_ACTIVE=1 \
    FAKE_GH_LOG="$tmp/gh.log" \
    FAKE_MUTATION_LOG="$tmp/mutations.log" \
    STALE_PR="$stale_pr" ABSENT_PR="$absent_pr" \
    STALE_HEAD="$stale_head" ABSENT_HEAD="$absent_head" \
    "$hub" apply-local-label --all-open --repo rrnewton/hermit --json "$@"
}

: >"$tmp/gh.log"
: >"$tmp/mutations.log"
run_reconcile >"$tmp/actual.out"
sed -n '/^{/,$p' "$tmp/actual.out" >"$tmp/actual.json"
jq -e '.removed == 1 and .applied == 0' "$tmp/actual.json" >/dev/null ||
    fail "actual reconcile did not report one removal and zero applications"
jq -e --argjson pr "$stale_pr" \
    '.actions[] | select(.pr == $pr) | .action == "removed"' \
    "$tmp/actual.json" >/dev/null || fail "stale labeled head was not removed"
jq -e --argjson pr "$absent_pr" \
    '.actions[] | select(.pr == $pr) | .action == "skip"' \
    "$tmp/actual.json" >/dev/null || fail "unlabeled unvalidated head was not left absent"
[[ $(wc -l <"$tmp/mutations.log") -eq 1 ]] ||
    fail "reconcile performed more than the one required mutation"
grep -Fxq \
    "pr edit $stale_pr --repo rrnewton/hermit --remove-label locally-validated" \
    "$tmp/mutations.log" || fail "reconcile did not remove the exact stale label"

: >"$tmp/gh.log"
: >"$tmp/mutations.log"
run_reconcile --dry-run >"$tmp/dry-run.out"
sed -n '/^{/,$p' "$tmp/dry-run.out" >"$tmp/dry-run.json"
jq -e '.removed == 1 and .applied == 0' "$tmp/dry-run.json" >/dev/null ||
    fail "dry-run did not report the intended removal"
jq -e --argjson pr "$stale_pr" \
    '.actions[] | select(.pr == $pr) | .action == "would-remove"' \
    "$tmp/dry-run.json" >/dev/null || fail "dry-run omitted the stale-label action"
[[ ! -s $tmp/mutations.log ]] || fail "dry-run mutated a GitHub label"

printf 'PASS: apply-local-label reconciles stale labels without removing absent labels\n'
