#!/usr/bin/env bash
# Acceptance tests for bin/hermit-repeat.
#
# WHY THESE PARTICULAR ASSERTIONS. Every test below exists because the thing it
# checks was WRONG at some point during development and reading the code did not
# reveal it:
#
#   * T4/T5 -- the runner's `--keep-going` lets in-flight steps finish but STILL
#     STOPS LAUNCHING NEW ONES. At `-j 1` the first diverging node ended the
#     sweep: 4 of 20 nodes ran, 16 never started, and the report called all 16
#     "failed". Sixteen fabricated failures. A node now exits 0 to the runner
#     when it produced a MEASUREMENT (clean or diverged), and a node that never
#     started is its own category.
#   * T6 -- hermit writes `verdict:"no_result"` both when the comparison ran and
#     selected nothing AND when run 1 died before there was a second run to
#     compare. LiteInst on `sh -c 'seq | wc -l'` does the latter. Filing a hard
#     backend failure as a measurement gap is exactly the "value that reads as
#     information and carries none" this project keeps rediscovering.
#   * T7 -- the record-index cross-check compared our derived turn to hermit's.
#     When hermit publishes no turn (`--no-sequentialize-threads` emits no
#     COMMITs) both were None, `None == None` was true, and the check printed
#     "OK" having verified nothing -- then the syscall ordinal it guards was
#     printed as established fact. A vacuous pass is worse than no check.
#   * T8 -- "no syscall had completed yet" and "we could not tell" must not share
#     a blank column.
#
# The end-to-end tests use synthetic node artefacts rather than real hermit runs
# so this is fast and hermetic; T1-T3 cover the real generation path.
set -uo pipefail
HR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/hermit-repeat"
pass=0; fail=0
ok(){ printf '  PASS  %s\n' "$1"; pass=$((pass+1)); }
no(){ printf '  FAIL  %s -- %s\n' "$1" "$2"; fail=$((fail+1)); }
[ -x "$HR" ] || { echo "no hermit-repeat at $HR"; exit 2; }

t=$(mktemp -d); trap 'rm -rf "$t"' EXIT

# T1 the log-coordinate parsing agrees with detcore's record enumeration.
"$HR" --selftest >"$t/1.out" 2>&1 \
  && ok "selftest: log-coordinate parsing matches detcore's record enumeration" \
  || no "selftest" "$(tail -3 "$t/1.out")"

# T2 refuses with no guest command rather than running something surprising.
"$HR" -n 2 >/dev/null 2>"$t/2.err"; rc=$?
[ $rc -eq 2 ] && grep -qi "no guest command" "$t/2.err" \
  && ok "no guest command is a usage error (exit 2), not a default run" \
  || no "empty guest" "rc=$rc, stderr: $(head -1 "$t/2.err")"

# T3 GENERATION: N disconnected nodes, no edges, and every node goes through
# bin/safehermit. If a generated command ever bypasses the wrapper, the whole
# point of building this on safehermit is gone, so assert it directly.
"$HR" -n 4 --out "$t/gen" --dry-run -- /bin/echo hi >"$t/3.out" 2>&1
python3 - "$t/gen/dag.json" >"$t/3.chk" 2>&1 <<'PY'
import json, sys, pathlib
d = json.load(open(sys.argv[1]))
steps = d["steps"]
assert len(steps) == 4, f"expected 4 steps, got {len(steps)}"
assert all(not s.get("deps") for s in steps), "nodes must be DISCONNECTED (no deps)"
assert len({s["job"] for s in steps}) == 4, "node ids must be distinct"
assert all(s["hint"]["preferred_inner_jobs"] == 1 for s in steps), "per-node width must be 1"
root = pathlib.Path(sys.argv[1]).parent
for s in steps:
    body = (root / "runs" / s["job"] / "node.sh").read_text()
    assert "/bin/safehermit " in body, f"{s['job']} does not go through bin/safehermit"
    assert "--verify" in body and "--verify-strict" in body, f"{s['job']} is not a verify pair"
print("ok")
PY
grep -q '^ok$' "$t/3.chk" \
  && ok "generates N disconnected, distinct, width-1 nodes, each boxed by safehermit" \
  || no "dag generation" "$(head -3 "$t/3.chk")"

# ---- synthetic-artefact tests -------------------------------------------------
# Build a run directory by hand and run only the collector, so the classification
# rules are tested without needing a guest that misbehaves on demand.
mkfixture() {   # mkfixture <dir> <name>
    mkdir -p "$1/runs/$2/logs"
}
collect() {     # collect <outdir> ; prints "<name> <category>" per run
    python3 - "$HR" "$1" <<'PY'
from importlib.machinery import SourceFileLoader
import importlib.util, sys, pathlib
loader = SourceFileLoader("hr", sys.argv[1])
spec = importlib.util.spec_from_loader("hr", loader)
mod = importlib.util.module_from_spec(spec); sys.modules["hr"] = mod; loader.exec_module(mod)
root = pathlib.Path(sys.argv[2]) / "runs"
for d in sorted(root.iterdir()):
    r = mod.RunResult(index=0, name=d.name, dir=str(d))
    mod.collect_one(r, "/bin/true", keep=True)
    print(d.name, r.category, "|", r.why)
PY
}

# T4 A NODE THAT NEVER STARTED IS NOT A FAILURE. No stamps, no rc, no verdict.
mkfixture "$t/c1" nostart
collect "$t/c1" >"$t/4.out" 2>&1
grep -q "^nostart not_run" "$t/4.out" \
  && ok "a node that never launched is 'not_run', not 'failed'" \
  || no "not_run" "got: $(cat "$t/4.out")"

# T5 A node that started and died with no verdict IS a failure.
mkfixture "$t/c2" died
date +%s.%N > "$t/c2/runs/died/started"; printf 1 > "$t/c2/runs/died/rc"
collect "$t/c2" >"$t/5.out" 2>&1
grep -q "^died failed" "$t/5.out" \
  && ok "a node that started and produced no verdict is 'failed'" \
  || no "failed" "got: $(cat "$t/5.out")"

# T6 verdict=no_result with comparison=null is a FAILURE (run 1 died), while
# verdict=no_result WITH a comparison is a genuine no-result. Same field, two
# meanings; they must not share a bucket.
mkfixture "$t/c3" crashed; mkfixture "$t/c3" empty
date +%s.%N > "$t/c3/runs/crashed/started"; printf 1 > "$t/c3/runs/crashed/rc"
echo '{"verdict":"no_result","comparison":null,"verified":false}' > "$t/c3/runs/crashed/verify.json"
date +%s.%N > "$t/c3/runs/empty/started"; printf 0 > "$t/c3/runs/empty/rc"
echo '{"verdict":"no_result","comparison":{"strictness":"canonical"},"verified":false}' > "$t/c3/runs/empty/verify.json"
collect "$t/c3" >"$t/6.out" 2>&1
grep -q "^crashed failed" "$t/6.out" && grep -q "^empty no_result" "$t/6.out" \
  && ok "no_result WITHOUT a comparison is a failure; WITH one it is a no-result" \
  || no "no_result split" "got: $(cat "$t/6.out")"

# T7 THE CROSS-CHECK MUST NOT PASS VACUOUSLY. A diverged run whose logs are
# absent cannot be cross-checked, so it must not come back verified.
mkfixture "$t/c4" div
date +%s.%N > "$t/c4/runs/div/started"; printf 1 > "$t/c4/runs/div/rc"
echo '{"verdict":"diverged","comparison":{},"verified":false,"first_divergent_scheduler_turn":9,"first_divergent_virtual_nanoseconds":5}' > "$t/c4/runs/div/verify.json"
python3 - "$HR" "$t/c4/runs/div" <<'PY' >"$t/7.out" 2>&1
from importlib.machinery import SourceFileLoader
import importlib.util, sys
loader = SourceFileLoader("hr", sys.argv[1])
spec = importlib.util.spec_from_loader("hr", loader)
mod = importlib.util.module_from_spec(spec); sys.modules["hr"] = mod; loader.exec_module(mod)
r = mod.RunResult(index=0, name="div", dir=sys.argv[2])
mod.collect_one(r, "/bin/true", keep=True)
print("category", r.category)
print("verified", r.coord_verified)
print("check", r.coord_cross_check)
print("syscall", r.syscall_ordinal, "|", r.syscall_note)
PY
grep -q "^category diverged" "$t/7.out" && grep -q "^verified False" "$t/7.out" \
  && grep -q "^syscall None" "$t/7.out" \
  && ok "a divergence with no readable log pair is diverged but NOT verified, and no syscall number is invented" \
  || no "vacuous cross-check" "got: $(cat "$t/7.out")"

# T8 the turn/virtual-time coordinates still come through from hermit's own JSON
# even when the record index cannot be recovered -- a missing unit must not take
# the available ones with it.
grep -qE "^category diverged" "$t/7.out" && python3 - "$HR" "$t/c4/runs/div" <<'PY' >"$t/8.out" 2>&1
from importlib.machinery import SourceFileLoader
import importlib.util, sys
loader = SourceFileLoader("hr", sys.argv[1])
spec = importlib.util.spec_from_loader("hr", loader)
mod = importlib.util.module_from_spec(spec); sys.modules["hr"] = mod; loader.exec_module(mod)
r = mod.RunResult(index=0, name="div", dir=sys.argv[2])
mod.collect_one(r, "/bin/true", keep=True)
print("turn", r.sched_turn, "ns", r.virtual_ns, "record", r.info_record)
PY
grep -q "^turn 9 ns 5 record None" "$t/8.out" \
  && ok "an unavailable unit does not suppress the units that ARE available" \
  || no "unit independence" "got: $(cat "$t/8.out")"

# T9 measured peak overlap is derived from the stamps, not from -j.
python3 - "$HR" <<'PY' >"$t/9.out" 2>&1
from importlib.machinery import SourceFileLoader
import importlib.util, sys
loader = SourceFileLoader("hr", sys.argv[1])
spec = importlib.util.spec_from_loader("hr", loader)
mod = importlib.util.module_from_spec(spec); sys.modules["hr"] = mod; loader.exec_module(mod)
mk = lambda n, s, e: mod.RunResult(index=0, name=n, dir="", started_at=s, ended_at=e)
print("overlapping", mod.measured_peak_overlap([mk("a", 0, 10), mk("b", 1, 9), mk("c", 2, 8)]))
print("serial", mod.measured_peak_overlap([mk("a", 0, 1), mk("b", 2, 3), mk("c", 4, 5)]))
print("nostamps", mod.measured_peak_overlap([mod.RunResult(index=0, name="a", dir="")]))
PY
grep -q "^overlapping 3" "$t/9.out" && grep -q "^serial 1" "$t/9.out" \
  && grep -q "^nostamps None" "$t/9.out" \
  && ok "peak overlap is measured from stamps (3 concurrent, 1 serial, None unknown)" \
  || no "peak overlap" "got: $(cat "$t/9.out")"

# T10 PREFLIGHT REFUSES IN ~ONE RUN, NOT N. An owner asked for -n 200 against a
# build with no SaBRe backend; all 200 failed identically and the run cost 200
# node-times to learn a build was missing a backend. One real run of the real
# guest answers that first. Asserts BOTH that it refuses and that it is fast --
# a preflight that took the whole sweep to refuse would be pointless.
s=$(date +%s)
"$HR" -n 200 --deadline 60 --out "$t/pf" -- /no/such/guest >"$t/10.out" 2>&1; rc=$?
e=$(( $(date +%s) - s ))
if [ "$e" -ge 60 ]; then
    no "preflight speed" "took ${e}s; it did not short-circuit the sweep"
else
    [ $rc -eq 2 ] && grep -q "REFUSED" "$t/10.out" \
      && grep -q "CONFIGURATION ERROR" "$t/10.out" \
      && ok "preflight refuses a bad configuration in ${e}s with exit 2, not after 200 runs" \
      || no "preflight refusal" "rc=$rc in ${e}s; $(grep -c REFUSED "$t/10.out") REFUSED lines, $(grep -c 'CONFIGURATION ERROR' "$t/10.out") config-error lines"
fi

# T11 --no-preflight still runs, and THE LAST LINE IS THE VERDICT. This is the
# defect the owner reported: the no-divergence-point section was correct but the
# report ended on a tip about run modes, so a reader of the tail learned nothing.
"$HR" -n 3 --deadline 60 --no-preflight --out "$t/allfail" -- /no/such/guest >"$t/11.out" 2>&1; rc=$?
last=$(grep '^hermit-repeat: ' "$t/11.out" | tail -5 | head -1)
[ $rc -eq 3 ] && printf '%s' "$last" | grep -q "ALL 3 RUNS FAILED" \
  && ok "an all-failed sweep exits 3 and its final lines say ALL FAILED with the cause" \
  || no "terminal verdict" "rc=$rc, final verdict line was: $last"

# T12 the reported cause is the line that NAMES the problem, not the wrapped
# errno continuation under it.
grep -q "Program /no/such/guest does not exist" "$t/11.out" \
  && ! grep -qE '^hermit-repeat:\s+> No such file' "$t/11.out" \
  && ok "the grouped cause is hermit's Error: line, not its '> errno' continuation" \
  || no "cause selection" "got: $(grep -A2 'single cause' "$t/11.out" | tail -1)"

# T13 the verdict text is generated in ONE place, so the report's last section
# and the process's final stderr lines cannot disagree.
python3 - "$HR" <<'PY' >"$t/13.out" 2>&1
from importlib.machinery import SourceFileLoader
import importlib.util, sys
loader = SourceFileLoader("hr", sys.argv[1])
spec = importlib.util.spec_from_loader("hr", loader)
mod = importlib.util.module_from_spec(spec); sys.modules["hr"] = mod; loader.exec_module(mod)
def mk(name, cat, tail=(), why="w"):
    r = mod.RunResult(index=0, name=name, dir="", category=cat, why=why)
    r.error_tail = list(tail); return r
meta = {"mode": "concurrent"}
same = [mk(f"r{i}", "failed", ["Error: backend `sabre` is unavailable: SaBRe support was not included in this build"]) for i in range(200)]
v = mod.verdict_lines(meta, same)
print("homogeneous_head:", v[0])
print("homogeneous_config:", "CONFIGURATION ERROR" in " ".join(v))
mixed = [mk("a", "failed", ["Error: one thing"]), mk("b", "failed", ["Error: another thing"])]
print("mixed_head:", mod.verdict_lines(meta, mixed)[0])
print("mixed_not_config:", "CONFIGURATION ERROR" not in " ".join(mod.verdict_lines(meta, mixed)))
allclean = [mk(f"c{i}", "clean") for i in range(20)]
print("clean_head:", mod.verdict_lines(meta, allclean)[0])
mix2 = [mk("d", "diverged"), mk("e", "clean"), mk("f", "failed", ["Error: x"])]
print("partial_head:", mod.verdict_lines(meta, mix2)[0])
PY
grep -q "^homogeneous_head: VERDICT: ALL 200 RUNS FAILED" "$t/13.out" \
  && grep -q "^homogeneous_config: True" "$t/13.out" \
  && grep -q "^mixed_not_config: True" "$t/13.out" \
  && grep -q "^clean_head: VERDICT: all 20 runs clean" "$t/13.out" \
  && grep -q "^partial_head: VERDICT: 1 diverged, 1 clean, and 1 of 3 produced NO" "$t/13.out" \
  && ok "verdict: 200 identical failures are ONE config error; mixed causes are not labelled config" \
  || no "verdict lines" "got: $(cat "$t/13.out")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
