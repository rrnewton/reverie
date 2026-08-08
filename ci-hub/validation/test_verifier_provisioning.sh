#!/usr/bin/env bash
# INERT both-sided bracket of the merge gate's VERIFIER PROVISIONING step.
#
# The gate does not run verify_receipt.sh from a checkout -- it fetches it into
# $RUNNER_TEMP and runs it there. The verifier is NOT self-contained: it reads
# its qualifying-receipt predicate and its producer-definition registration from
# sibling files. A provisioning step that fetches only the script therefore
# leaves those unresolvable, and the verifier exits 2 on EVERY call.
#
# That is fail-closed, so it is not a fake green -- but it silently turns every
# legitimate receipt into a refusal, and it presents as mass evidence loss
# rather than as a deploy defect. It was live: the deployed pin predates the
# shared-predicate refactor, so advancing the pin to any parent commit at or
# after 19a219f would have armed it.
#
# This bracket EXTRACTS the real step from merge-gate.yml and runs it against a
# STUBBED `gh` serving fixture content. Nothing dials GitHub, dispatches a
# workflow, or touches a PR (#243).
#
#   NEGATIVE  a provisioning step that omits either registry leaves the verifier
#             unable to resolve it -> exit 2, and the bracket FAILS.
#   POSITIVE  the real step provisions all three artifacts and the verifier
#             reaches a genuine verdict (exit 0/1, never 2).
#
# Exit 3 = UNAVAILABLE (no gate definition to read), matching the posture of
# test_gate_definition_binding.sh: decline to have an opinion rather than
# reporting a false red where the hermit checkout does not exist.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(cd -- "$script_dir/../.." && pwd)
gate_yaml=${GATE_YAML:-$root/hermit/.github/workflows/merge-gate.yml}
verifier_src=$script_dir/verify_receipt.sh
step_name='Fetch the trusted receipt verifier'

if [ ! -r "$gate_yaml" ]; then
  echo "UNAVAILABLE: no gate definition to bracket at $gate_yaml" >&2
  echo "             point GATE_YAML at a merge-gate.yml, or initialise the" >&2
  echo "             hermit checkout; refusing to report PASS or FAIL." >&2
  exit 3
fi

tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

# Extract every copy of the step. There is deliberately more than one job
# carrying it, and a fix applied to only one of them still leaves the
# authoritative merge-gate job broken -- so all copies are bracketed.
python3 - "$gate_yaml" "$step_name" "$tmp" <<'PY'
import sys, pathlib
path, want, out = sys.argv[1], sys.argv[2], pathlib.Path(sys.argv[3])
lines = open(path).read().splitlines()
found = 0
for i, line in enumerate(lines):
    if line.strip() not in (f"- name: {want}", f'- name: "{want}"'):
        continue
    run_at = None
    for j in range(i + 1, len(lines)):
        s = lines[j].strip()
        if s.startswith("- name:"):
            break
        if s in ("run: |", "run: |-"):
            run_at = j
            break
    if run_at is None:
        continue
    # The step's `env:` block is part of its contract -- PARENT_REF lives there,
    # and dropping it would make the extracted script fail on an unbound
    # variable and look like a provisioning defect that is really a harness bug.
    prelude = []
    for j in range(i + 1, run_at):
        if lines[j].strip() == "env:":
            env_indent = len(lines[j]) - len(lines[j].lstrip())
            for k in range(j + 1, run_at):
                s = lines[k].strip()
                if not s:
                    continue
                if (len(lines[k]) - len(lines[k].lstrip())) <= env_indent:
                    break
                if ":" not in s:
                    continue
                name, _, value = s.partition(":")
                value = value.strip()
                # A ${{ ... }} expression cannot be evaluated off-runner; give
                # it an inert stub so the wiring under test still executes.
                if value.startswith("${{"):
                    value = "stub"
                prelude.append(f"export {name.strip()}={value}")
            break
    indent = len(lines[run_at]) - len(lines[run_at].lstrip())
    body = []
    for line2 in lines[run_at + 1:]:
        if line2.strip() and (len(line2) - len(line2.lstrip())) <= indent:
            break
        body.append(line2[indent + 2:] if len(line2) > indent + 2 else "")
    found += 1
    (out / f"step{found}.sh").write_text("\n".join(prelude + body) + "\n")
print(found)
PY
copies=$(ls "$tmp"/step*.sh 2>/dev/null | wc -l)
if [ "$copies" -eq 0 ]; then
  echo "FAIL: no '$step_name' step body extracted from $gate_yaml" >&2
  exit 1
fi
echo "extracted $copies copy/copies of '$step_name'"

neg_refused=0; neg_total=0; pos_accepted=0; pos_total=0; fail=0

# Serve fixture content for whatever repo path the step asks for. Provisioning
# fetches return base64 like `--jq .content`; exact-target producer resolution
# returns the registered blob identity like `--jq .sha`. This keeps the
# positive leg on an actual registered commit instead of passing merely because
# an unrelated fake SHA is correctly refused.
make_stub() { # make_stub <bin-dir>
  mkdir -p "$1"
  cat >"$1/gh" <<'STUB'
#!/usr/bin/env bash
# args: api <repos/.../contents/<path>?ref=...> --jq .content
url=""
for a in "$@"; do case "$a" in repos/*) url=$a ;; esac; done
rel=${url#*/contents/}; rel=${rel%%\?*}
case " $* " in
  *" --jq .sha "*)
    registry="$FIXTURE_ROOT/ci-hub/validate/producer-definition.json"
    blob=$(jq -r --arg path "$rel" '.registered[$path] // empty' "$registry")
    case "$blob" in
      [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*)
        [ "${#blob}" -eq 40 ] || exit 1
        printf '%s\n' "$blob"
        exit 0
        ;;
    esac
    echo "stub gh: no registered producer blob for $rel" >&2
    exit 1
    ;;
esac
src="$FIXTURE_ROOT/$rel"
[ -f "$src" ] || { echo "stub gh: no fixture for $rel" >&2; exit 1; }
base64 -w0 <"$src"; echo
STUB
  chmod +x "$1/gh"
}

# Build a fixture parent tree whose digests match whatever the step expects, by
# reading the expected digests out of the step itself. The point of the bracket
# is the PROVISIONING WIRING, not the digest values.
run_step() { # run_step <step.sh> <label> <expect_verifier_rc_not_2>
  local step=$1 label=$2 mode=$3
  local box="$tmp/run" ; rm -rf "$box"; mkdir -p "$box/bin" "$box/temp" "$box/fixture"
  make_stub "$box/bin"

  mkdir -p "$box/fixture/ci-hub/validation" "$box/fixture/ci-hub/validate"
  cp "$verifier_src" "$box/fixture/ci-hub/validation/verify_receipt.sh"
  cp "$root/ci-hub/validate/qualifying-receipt.json" "$box/fixture/ci-hub/validate/"
  cp "$root/ci-hub/validate/producer-definition.json" "$box/fixture/ci-hub/validate/"

  # Rewrite the step's expected digests to match the fixture content, so this
  # bracket does not fail merely because the pin is older than the checkout.
  local patched="$box/step.sh"; cp "$step" "$patched"
  local relpath digest
  while read -r relpath; do
    digest=$(sha256sum "$box/fixture/$relpath" | awk '{print $1}')
    python3 - "$patched" "$relpath" "$digest" <<'PY'
import re, sys
p, rel, dig = sys.argv[1], sys.argv[2], sys.argv[3]
t = open(p).read()
# replace the 64-hex token that follows this path in the step body
t = re.sub(re.escape(rel) + r'(["\s\\]+\S*["\s\\]*)([0-9a-f]{64})',
           lambda m: rel + m.group(1) + dig, t)
open(p, 'w').write(t)
PY
  done <<'PATHS'
ci-hub/validation/verify_receipt.sh
ci-hub/validate/qualifying-receipt.json
ci-hub/validate/producer-definition.json
PATHS
  # The single-artifact (old) form has no path-adjacent digest for the JSONs;
  # its lone digest still needs updating to the fixture verifier.
  local vdigest; vdigest=$(sha256sum "$box/fixture/ci-hub/validation/verify_receipt.sh" | awk '{print $1}')
  python3 - "$patched" "$vdigest" <<'PY'
import re, sys
p, dig = sys.argv[1], sys.argv[2]
t = open(p).read()
t = re.sub(r"'[0-9a-f]{64}  '", "'" + dig + "  '", t)
open(p, 'w').write(t)
PY

  local env_file="$box/github_env"; : >"$env_file"
  local rc=0 out
  out=$(PATH="$box/bin:$PATH" FIXTURE_ROOT="$box/fixture" RUNNER_TEMP="$box/temp" \
        GITHUB_ENV="$env_file" GH_TOKEN=stub bash "$patched" 2>&1) || rc=$?
  if [ "$rc" -ne 0 ]; then
    printf 'BAD  %-4s provisioning step itself failed rc=%s: %s\n' "$mode" "$rc" "$label" >&2
    printf '     %s\n' "$out" >&2
    fail=1; return 0
  fi

  # Now run the provisioned verifier exactly as the gate does: with only the
  # env the step exported, from the temp dir, with NO checkout beside it.
  local verifier predicate producer target_sha
  verifier=$(sed -n 's/^RECEIPT_VERIFIER=//p' "$env_file" | tail -1)
  predicate=$(sed -n 's/^QUALIFYING_RECEIPT_PREDICATE=//p' "$env_file" | tail -1)
  producer=$(sed -n 's/^PRODUCER_DEFINITION_REGISTRY=//p' "$env_file" | tail -1)
  target_sha=$(jq -r '.registered_at' \
    "$box/fixture/ci-hub/validate/producer-definition.json")
  printf '[[]]\n' >"$box/comments.json"
  local vrc=0
  env -i PATH="$PATH" HOME="$box" \
      ${predicate:+QUALIFYING_RECEIPT_PREDICATE="$predicate"} \
      ${producer:+PRODUCER_DEFINITION_REGISTRY="$producer"} \
      bash "$verifier" --repo rrnewton/hermit \
        --sha "$target_sha" \
        --comments "$box/comments.json" >/dev/null 2>&1 || vrc=$?

  # exit 2 == the verifier could not resolve a registry == deploy defect.
  local status
  if [ "$mode" = POS ]; then
    pos_total=$((pos_total + 1))
    if [ "$vrc" -ne 2 ]; then status=OK; pos_accepted=$((pos_accepted + 1)); else status=BAD; fail=1; fi
  else
    neg_total=$((neg_total + 1))
    if [ "$vrc" -eq 2 ]; then status=OK; neg_refused=$((neg_refused + 1)); else status=BAD; fail=1; fi
  fi
  printf '%-4s %-4s verifier rc=%-2s %s\n' "$status" "$mode" "$vrc" "$label"
  return 0
}

echo
echo "== POSITIVE leg: the real step provisions every registry the verifier needs =="
n=0
for step in "$tmp"/step*.sh; do
  n=$((n + 1))
  run_step "$step" "real merge-gate.yml provisioning step, copy $n of $copies" POS
done

echo
echo "== NEGATIVE leg: a step that omits a registry leaves the verifier unable to resolve it =="
# The pre-fix shape: fetch ONLY the verifier. Reconstructed from the REAL step
# by deleting its registry provisioning, so each negative is the actual wiring
# minus one piece rather than a hand-written straw man. Deletion works on
# LOGICAL lines (backslash continuations joined first): a `fetch` call spans two
# physical lines, and removing only the first would orphan the digest argument
# and produce a shell error that looks like a detection but is a harness bug.
strip_lines() { # strip_lines <in> <out> <pattern>...
  python3 - "$@" <<'PY'
import sys, re
src, dst, *pats = sys.argv[1:]
raw = open(src).read().splitlines()
logical, buf = [], ""
for line in raw:
    buf = buf + "\n" + line if buf else line
    if line.rstrip().endswith("\\"):
        continue
    logical.append(buf); buf = ""
if buf:
    logical.append(buf)
kept = [l for l in logical if not any(re.search(p, l) for p in pats)]
removed = len(logical) - len(kept)
if removed == 0:
    sys.exit(f"strip_lines matched NOTHING for {pats} -- negative would be a no-op")
open(dst, "w").write("\n".join(kept) + "\n")
PY
}

strip_lines "$tmp/step1.sh" "$tmp/neg-none.sh" \
  'qualifying-receipt\.json' 'producer-definition\.json' \
  'QUALIFYING_RECEIPT_PREDICATE' 'PRODUCER_DEFINITION_REGISTRY' \
  '^\s*predicate=' '^\s*producer='
run_step "$tmp/neg-none.sh" "SCRIPT ONLY: neither registry provisioned (the shipped defect)" NEG

strip_lines "$tmp/step1.sh" "$tmp/neg-producer.sh" \
  'producer-definition\.json' 'PRODUCER_DEFINITION_REGISTRY' '^\s*producer='
run_step "$tmp/neg-producer.sh" "PARTIAL: predicate provisioned, producer registry omitted" NEG

strip_lines "$tmp/step1.sh" "$tmp/neg-predicate.sh" \
  'qualifying-receipt\.json' 'QUALIFYING_RECEIPT_PREDICATE' '^\s*predicate='
run_step "$tmp/neg-predicate.sh" "PARTIAL: producer registry provisioned, predicate omitted" NEG

echo
printf 'NEGATIVE (exit-2) detections: %d/%d   POSITIVE (resolvable) : %d/%d\n' \
  "$neg_refused" "$neg_total" "$pos_accepted" "$pos_total"
if [ "$fail" -ne 0 ] || [ "$neg_refused" -ne "$neg_total" ] ||
   [ "$pos_accepted" -ne "$pos_total" ] || [ "$pos_total" -lt 1 ]; then
  echo "FAIL: verifier-provisioning bracket" >&2
  exit 1
fi
echo "PASS: every copy of the gate's provisioning step gives the verifier a resolvable predicate and producer registry; omitting either is detected"
