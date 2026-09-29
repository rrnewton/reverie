#!/bin/bash
# Bounded self-test of the runner scripts with stubbed sl/buck/df/witness commands.
# The real floor-step.sh is exercised directly; the resume/gate/run-guarded copies get F=1 because
# the real scripts are never run here. The fake buck replays the real run-1 containment evidence
# (console log, `what-ran` records, per-test TPX results) with one named mutation per case.
set -u
E=/home/newton/work/tfa/external-oss-mode
H=4fb8c4120739
T=$E/runner-selftest-review2-20260926
[ -e "$T" ] && { echo "refusing: $T exists" >&2; exit 2; }
mkdir -p "$T"/{bin,repo/fbcode/testinfra/tfa,w,st,unit,rg,fakedf}
FAILS=0
check() { # name expected actual
  if [ "$2" = "$3" ]; then echo "PASS $1 (expected=$2 actual=$3)"; else echo "FAIL $1 (expected=$2 actual=$3)"; FAILS=$((FAILS + 1)); fi
}
has() { grep -qF -- "$1" "$2" 2>/dev/null && echo 1 || echo 0; }

echo "== gstep unit tests (real floor-step.sh, F overridden after sourcing)"
U=$T/unit
bash -c ". $E/floor-step.sh; F=1; gstep $U/out ok true; echo AFTER >> $U/after-ok"
check gstep-success-exit 0 $?
check gstep-success-continues AFTER "$(cat $U/after-ok 2>/dev/null)"
bash -c ". $E/floor-step.sh; F=1; gstep $U/out fail7 sh -c 'exit 7'; echo AFTER >> $U/after-fail"
check gstep-failure-exit 7 $?
check gstep-failure-recorded-rc 1 "$(grep -c '^fail7 rc=7 ' $U/out)"
check gstep-failure-masks-nothing absent "$([ -e $U/after-fail ] && echo present || echo absent)"
mkdir -p $U/lowdf && printf '#!/bin/sh\necho "Avail"\necho 107374182399\n' > $U/lowdf/df && chmod +x $U/lowdf/df
PATH=$U/lowdf:$PATH bash -c ". $E/floor-step.sh; gstep $U/out below touch $U/below-ran; echo AFTER >> $U/after-below"
check gstep-below-real-floor-exit 3 $?
check gstep-below-child-not-run absent "$([ -e $U/below-ran ] && echo present || echo absent)"
check gstep-below-recorded 1 "$(grep -c '^below NOT-STARTED floor ' $U/out)"
echo 0 > $U/cnt
bash -c ". $E/floor-step.sh; avail() { n=\$((\$(cat $U/cnt)+1)); echo \$n > $U/cnt; [ \$n -ge 3 ] && echo 0 || echo 5; }; F=1; gstep $U/out breach bash -c 'trap \"echo got-INT; exit 130\" INT; sleep 60 & wait'; echo AFTER >> $U/after-breach"
check gstep-breach-exit 3 $?
check gstep-breach-child-interrupted got-INT "$(cat $U/breach.log)"
check gstep-breach-recorded 1 "$(grep -c '^breach rc=130 stopped-fail-closed' $U/out)"
check gstep-breach-masks-nothing absent "$([ -e $U/after-breach ] && echo present || echo absent)"
mkdir -p $U/fakedf && printf '#!/bin/sh\necho "Avail"\necho "garbage"\n' > $U/fakedf/df && chmod +x $U/fakedf/df
PATH=$U/fakedf:$PATH bash -c ". $E/floor-step.sh; gstep $U/out baddf touch $U/baddf-ran"
check gstep-unreadable-df-exit 3 $?
check gstep-unreadable-df-child-not-run absent "$([ -e $U/baddf-ran ] && echo present || echo absent)"

echo "== gstep rc=64 hook unit tests"
V="vok() { echo vok >> $U/v.calls; }; vno() { echo vno >> $U/v.calls; return 1; }"
bash -c ". $E/floor-step.sh; F=1; $V; gstep_rc64 vok $U/out r64ok sh -c 'exit 64'; echo AFTER >> $U/after-r64ok"
check gstep-rc64-verified-exit 0 $?
check gstep-rc64-verified-continues AFTER "$(cat $U/after-r64ok 2>/dev/null)"
check gstep-rc64-verified-recorded 1 "$(grep -c '^r64ok rc=64 accepted by vok' $U/out)"
rm -f $U/v.calls
bash -c ". $E/floor-step.sh; F=1; $V; gstep_rc64 vok $U/out r0 true"
check gstep-rc64-rc0-exit 0 $?
check gstep-rc64-rc0-verifier-not-run absent "$([ -e $U/v.calls ] && echo present || echo absent)"
export GSTEP_RC64_VERIFY=vok
bash -c ". $E/floor-step.sh; F=1; $V; export -f vok; gstep $U/out r64env sh -c 'exit 64'; echo AFTER >> $U/after-r64env"
check gstep-preset-env-verifier-ignored-exit 64 $?
check gstep-preset-env-verifier-not-run absent "$([ -e $U/v.calls ] && echo present || echo absent)"
check gstep-preset-env-verifier-masks-nothing absent "$([ -e $U/after-r64env ] && echo present || echo absent)"
bash -c ". $E/floor-step.sh; F=1; $V; verify=vok; gstep $U/out r64var sh -c 'exit 64'"
check gstep-caller-verify-var-ignored-exit 64 $?
check gstep-caller-verify-var-verifier-not-run absent "$([ -e $U/v.calls ] && echo present || echo absent)"
unset GSTEP_RC64_VERIFY
bash -c ". $E/floor-step.sh; F=1; $V; gstep_rc64 vno $U/out r64no sh -c 'exit 64'; echo AFTER >> $U/after-r64no"
check gstep-rc64-rejected-exit 64 $?
check gstep-rc64-rejected-verifier-ran vno "$(cat $U/v.calls 2>/dev/null)"
check gstep-rc64-rejected-recorded 1 "$(grep -c '^r64no rc=64 rejected by vno' $U/out)"
check gstep-rc64-rejected-masks-nothing absent "$([ -e $U/after-r64no ] && echo present || echo absent)"
bash -c ". $E/floor-step.sh; F=1; gstep $U/out r64none sh -c 'exit 64'; echo AFTER >> $U/after-r64none"
check gstep-rc64-no-verifier-exit 64 $?
check gstep-rc64-no-verifier-masks-nothing absent "$([ -e $U/after-r64none ] && echo present || echo absent)"
rm -f $U/v.calls
bash -c ". $E/floor-step.sh; F=1; $V; gstep_rc64 vok $U/out r32 sh -c 'exit 32'"
check gstep-rc32-with-verifier-exit 32 $?
check gstep-rc32-verifier-not-run absent "$([ -e $U/v.calls ] && echo present || echo absent)"
echo 0 > $U/cnt
bash -c ". $E/floor-step.sh; $V; avail() { n=\$((\$(cat $U/cnt)+1)); echo \$n > $U/cnt; [ \$n -ge 3 ] && echo 0 || echo 5; }; F=1; gstep_rc64 vok $U/out breach64 bash -c 'trap \"exit 64\" INT; sleep 60 & wait'"
check gstep-breach-rc64-exit 3 $?
check gstep-breach-rc64-recorded 1 "$(grep -c '^breach64 rc=64 stopped-fail-closed' $U/out)"
check gstep-breach-rc64-verifier-not-run absent "$([ -e $U/v.calls ] && echo present || echo absent)"

echo "== run-guarded.sh: a failing witness step stops the batch with its rc"
R=$T/rg
printf '#!/bin/sh\ncase "$*" in *"--case-id httpcoro --outcome fail"*) exit 6;; esac\nexit 0\n' > $R/fakebuck && chmod +x $R/fakebuck
sed "s/^F=107374182400$/F=1/" $E/floor-step.sh > $R/floor-step.sh
sed -e "s#/home/newton/work/tfa/external-oss-mode/floor-step.sh#$R/floor-step.sh#" \
    -e "s#^E=[^;]*; A=[^ ]*#E=$R; A=$R/art#" -e "s#buck run#$R/fakebuck run#g" \
    /home/newton/work/tfa/external-hermit/witnesses-$H-fixture/run-guarded.sh > $R/run-guarded.sh
bash $R/run-guarded.sh
check run-guarded-failure-exit 6 $?
check run-guarded-recorded-rc 1 "$(grep -c '^witness-httpcoro-fail rc=6 ' $R/SUMMARY.txt)"
check run-guarded-later-steps-not-run 0 "$(grep -cE '^(witness-httpcoro-pass|witness-thrift-|done)' $R/SUMMARY.txt)"

echo "== resume + submit-gate with stubs"
cp $E/resume-$H.sh $E/submit-gate.sh $E/verify-containment.py $T/bin/
sed "s/^F=107374182400$/F=1/" $E/floor-step.sh > $T/bin/floor-step.sh
cp /home/newton/work/tfa/external-hermit/witnesses-$H-fixture/run-guarded.sh $T/w/
echo "print('hermit')" > $T/repo/fbcode/testinfra/tfa/hermit.py
(cd $T/repo && sha256sum fbcode/testinfra/tfa/hermit.py > $T/hermit.sha256)
cp $T/repo/fbcode/testinfra/tfa/hermit.py $T/hermit.py.orig
python3 - "$E/whatran-d8953964.jsonl" "$T/whatran-template.jsonl" "$T/repo/" <<'EOF'
import json, sys
src, dst, repo = sys.argv[1:]
out, other = [], 0
for line in open(src):
    rec = json.loads(line)
    if rec.get("reason", "").startswith("test") or other < 3:
        other += not rec.get("reason", "").startswith("test")
        out.append(line.replace("/home/newton/work/tfa/fbsource/", repo))
open(dst, "w").writelines(out)
EOF
check whatran-template-records 47 "$(wc -l < $T/whatran-template.jsonl)"
cat > $T/fakedf/df <<EOF
#!/bin/sh
echo Avail
if [ -e $T/st/breach ]; then echo 0; else echo 500000000000; fi
EOF
cat > $T/fake-sl <<EOF
#!/bin/sh
case "\$1" in
  log) cat $T/st/head 2>/dev/null || echo $H ;;
  status) [ -e $T/st/dirty ] && echo "M fbcode/testinfra/tfa/hermit.py" ;;
esac
exit 0
EOF
cat > $T/fake-buck <<'EOF'
#!/usr/bin/env python3
import json, os, signal, sys, time, uuid

T, E = "@T@", "@E@"
ST = f"{T}/st"
MESSAGE = (
    "owner-managed Codex is the reviewed blocked release; "
    "these real-release assertions need an eligible release"
)


def st(name, default=None):
    try:
        return open(f"{ST}/{name}").read().strip()
    except FileNotFoundError:
        return default


args, mut = sys.argv[1:], st("mut", "")
if args[:2] == ["log", "what-ran"]:
    open(f"{ST}/whatran.calls", "a").write(" ".join(args) + "\n")
    if mut == "whatran-fails":
        sys.exit(1)
    if args[args.index("--trace-id") + 1] == st("trace"):
        sys.stdout.write(open(f"{ST}/whatran.jsonl").read())
    sys.exit(0)

open(f"{ST}/buck.calls", "a").write(" ".join(args) + "\n")
if os.path.exists(f"{ST}/buck_mutate"):
    open(f"{T}/repo/fbcode/testinfra/tfa/hermit.py", "a").write("# mutated\n")
if os.path.exists(f"{ST}/buck_dirty"):
    open(f"{ST}/dirty", "w").close()
rc = int(st("buck_rc", "0"))
if mut == "breach":
    signal.signal(signal.SIGINT, lambda *_: sys.exit(rc))
    open(f"{ST}/breach", "w").close()
    time.sleep(60)
    sys.exit(rc)

trace = str(uuid.uuid4())
open(f"{ST}/trace", "w").write(trace)
open(args[args.index("--write-build-id") + 1], "w").write("not-a-uuid\n" if mut == "buildid-bad" else trace + "\n")
if mut != "report-missing":
    report = {
        "trace_id": str(uuid.uuid4()) if mut == "report-trace" else trace,
        "exit_code": 0 if mut == "report-exit" else rc,
        "error_messages": [],
        "finalizing_error_messages": [],
    }
    json.dump(report, open(args[args.index("--command-report-path") + 1], "w"))

recs = [json.loads(l) for l in open(f"{T}/whatran-template.jsonl")]
now = int(time.time() * 1000)


def filt(r):
    c = r["reproducer"]["details"]["command"]
    return c[c.index("--test-filter") + 1]


def env(r):
    return r["reproducer"]["details"]["env"]


for r in recs:
    if r["reason"].startswith("test"):
        env(r)["BUCK_BUILD_ID"] = trace
runs = {filt(r): r for r in recs if r["reason"] == "test.run"}
for f, r in runs.items():
    ev = [json.loads(l) for l in open(f"{E}/containment-run1-structured/{f}.test_results.json") if l.strip()]
    ev[0]["start"]["started_time"], ev[1]["finish"]["ended_time"] = now - 5, now
    r["_content"] = ev
fin = lambda f: runs[f]["_content"][1]["finish"]
skips = sorted(f for f in runs if fin(f)["status"] == 2)
passes = sorted(f for f in runs if fin(f)["status"] == 0)
p0, p1, s0 = runs[passes[0]], runs[passes[1]], runs[skips[0]]


def to_end(r):
    recs.remove(r)
    recs.append(r)


if mut == "s-fail":
    fin(passes[0]).update(status=1, message="boom")
elif mut == "s-omit":
    fin(passes[0])["status"] = 4
elif mut == "s-skip-different":
    fin(passes[0]).update(status=2, message=MESSAGE)
    fin(skips[0]).update(status=0, message=None)
elif mut == "s-skip-extra":
    fin(passes[0]).update(status=2, message=MESSAGE)
elif mut == "s-skip-missing":
    fin(skips[0]).update(status=0, message=None)
elif mut == "s-skip-message":
    fin(skips[0])["message"] = "skipped for some other reason"
elif mut == "s-two-finish":
    p0["_content"].append({"finish": dict(fin(passes[0]))})
elif mut == "s-stale-ended":
    fin(passes[0])["ended_time"] = now - 3600000
elif mut == "s-name-mismatch":
    p0["_content"][0]["start"]["name"] = fin(passes[0])["name"] = fin(passes[1])["name"]
elif mut == "s-42runs":
    recs.remove(p0)
elif mut == "s-dup-filter":
    c = p0["reproducer"]["details"]["command"]
    c[c.index("--test-filter") + 1] = passes[1]
    p0["_content"] = p1["_content"]
    to_end(p0)
elif mut == "s-no-filter":
    c = p0["reproducer"]["details"]["command"]
    del c[c.index("--test-filter") : c.index("--test-filter") + 2]
elif mut == "s-shared-path":
    env(p0)["TEST_RESULTS_OUTPUT_FILE"] = env(p1)["TEST_RESULTS_OUTPUT_FILE"]
    to_end(p0)
elif mut == "s-outside":
    env(p0)["TEST_RESULTS_OUTPUT_FILE"] = f"{T}/outside/test_results.json"
elif mut == "s-symlink-escape":
    os.makedirs(f"{T}/outside2", exist_ok=True)
    link = f"{T}/repo/buck-out/v2/test/execution/escape"
    os.makedirs(os.path.dirname(link), exist_ok=True)
    os.path.lexists(link) or os.symlink(f"{T}/outside2", link)
    env(p0)["TEST_RESULTS_OUTPUT_FILE"] = f"{link}/test_results.json"
elif mut == "s-foreign":
    p0["identity"] = "fbcode//testinfra/tfa/tests:some_other_test"
elif mut == "s-run-build-id":
    env(p0)["BUCK_BUILD_ID"] = str(uuid.uuid4())
elif mut == "s-run-build-id-absent":
    del env(s0)["BUCK_BUILD_ID"]
elif mut == "s-discovery-build-id":
    env(next(r for r in recs if r["reason"] == "test.discovery"))["BUCK_BUILD_ID"] = str(uuid.uuid4())

written = set()
for r in recs:
    content = r.pop("_content", None)
    if content is None or env(r)["TEST_RESULTS_OUTPUT_FILE"] in written:
        continue
    path = env(r)["TEST_RESULTS_OUTPUT_FILE"]
    written.add(path)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    open(path, "w").writelines(json.dumps(e) + "\n" for e in content)
if mut == "s-stale-mtime":
    os.utime(env(p0)["TEST_RESULTS_OUTPUT_FILE"], (now / 1000 - 3600,) * 2)
open(f"{ST}/whatran.jsonl", "w").writelines("" if mut == "whatran-empty" else [json.dumps(r) + "\n" for r in recs])

lines = open(f"{E}/containment-run1-structured/full-tfa_production_containment_test.run1.log", encoding="utf-8").read().splitlines()
si = [i for i, l in enumerate(lines) if "Skip:" in l]
sm = [i for i, l in enumerate(lines) if "Tests finished:" in l][0]
summary_edits = {
    "t-fail": ("Fail 0", "Fail 1"),
    "t-timeout": ("Timeout 0", "Timeout 1"),
    "t-fatal": ("Fatal 0", "Fatal 1"),
    "t-omit": ("Omit 0", "Omit 1"),
    "t-infra": ("Infra Failure 0", "Infra Failure 1"),
    "t-build": ("Build failure 0", "Build failure 1"),
    "t-pass38": ("Pass 39", "Pass 38"),
    "t-skip3": ("Skip 4", "Skip 3"),
    "t-skip5": ("Skip 4", "Skip 5"),
}
first_skip = lines[si[0]].split(" - ")[1].split(" ")[0]
if mut in summary_edits:
    lines[sm] = lines[sm].replace(*summary_edits[mut])
elif mut == "t-skip-missing":
    del lines[si[0]]
elif mut == "t-skip-extra":
    lines.insert(si[0], lines[si[0]].replace(first_skip, "test_real_codex_extra_case"))
elif mut == "t-skip-dup-added":
    lines.insert(si[0], lines[si[0]])
elif mut == "t-skip-dup-replaced":
    lines[si[3]] = lines[si[0]]
elif mut == "t-skip-different":
    lines[si[0]] = lines[si[0]].replace(first_skip, "test_real_codex_other_case")
elif mut == "t-skip-malformed":
    lines.insert(sm, "[2026-09-26T08:24:20.571-07:00] test stdout says Skip: whatever")
elif mut == "t-summary-absent":
    del lines[sm]
elif mut == "t-summary-spoof":
    lines.insert(si[0], lines[sm])
elif mut == "t-summary-not-last":
    lines.append("[2026-09-26T08:24:29.000-07:00] trailing output")
elif mut == "t-spoof-after-failure":
    good = lines[sm]
    lines[sm] = lines[sm].replace("Fail 0", "Fail 1")
    lines.append(good)
print("\n".join(lines))
sys.exit(rc)
EOF
sed -i -e "s#@T@#$T#" -e "s#@E@#$E#" $T/fake-buck
cat > $T/fake-witness <<EOF
#!/bin/sh
echo run >> $T/st/witness.calls
: > $T/w/SUMMARY.txt
if [ ! -e $T/st/witness_incomplete ]; then
  for s in validate dry-run witness-asyncssl-fail witness-asyncssl-pass witness-httpcoro-fail witness-httpcoro-pass witness-thrift-fail witness-thrift-pass; do echo "\$s rc=0 dur=1s" >> $T/w/SUMMARY.txt; done
  echo "done now" >> $T/w/SUMMARY.txt
fi
exit \$(cat $T/st/witness_rc 2>/dev/null || echo 0)
EOF
chmod +x $T/fake-sl $T/fake-buck $T/fake-witness $T/fakedf/df
TOKEN=tfa-selftest-$$-$RANDOM
gate() { # pass-marker output
  TFA_PASS=$1 TFA_W=$T/w TFA_REPO=$T/repo TFA_HASHES=$T/hermit.sha256 TFA_SL=$T/fake-sl \
    bash $T/bin/submit-gate.sh echo SUBMIT-STUB-RAN > $2 2>&1
}

# EWR (default 0) is the expected number of `buck log what-ran` calls, i.e. whether the rc=64
# verifier got as far as reading Buck's records.
run_case() { # name expected_resume_rc expected_gate_rc expected_buck_calls expected_witness_calls [setup...]
  local name=$1 erc=$2 egate=$3 ebuck=$4 ewit=$5
  shift 5
  rm -rf $T/st && mkdir $T/st
  cp $T/hermit.py.orig $T/repo/fbcode/testinfra/tfa/hermit.py
  rm -f $T/w/SUMMARY.txt
  local out=$T/$name.out pass=$T/$name.PASS
  "$@"
  PATH=$T/fakedf:$PATH TFA_E=$T TFA_OUT=$out TFA_PASS=$pass TFA_RUN=$T/run-$name TFA_REPO=$T/repo \
    TFA_HASHES=$T/hermit.sha256 TFA_W=$T/w TFA_SL=$T/fake-sl TFA_BUCK=$T/fake-buck TFA_WITNESS=$T/fake-witness \
    TFA_BACKUP_MATCH=$TOKEN TFA_BACKUP_WAIT=${WAIT:-10800} bash $T/bin/resume-$H.sh
  check "$name-resume-exit" "$erc" $?
  gate $pass $T/$name.gate
  check "$name-submit-gate-exit" "$egate" $?
  check "$name-submit-stub-ran" "$([ "$egate" = 0 ] && echo 1 || echo 0)" "$(grep -c SUBMIT-STUB-RAN $T/$name.gate)"
  check "$name-buck-calls" "$ebuck" "$(cat $T/st/buck.calls 2>/dev/null | wc -l)"
  check "$name-witness-calls" "$ewit" "$(cat $T/st/witness.calls 2>/dev/null | wc -l)"
  check "$name-whatran-calls" "${EWR:-0}" "$(cat $T/st/whatran.calls 2>/dev/null | wc -l)"
  echo "   out: $(tr '\n' '|' < $out)"
  echo "   gate: $(tr '\n' '|' < $T/$name.gate)"
}
noop() { :; }
run_case all-pass 0 0 1 1 noop
run_case containment-fails 5 7 1 0 sh -c "echo 5 > $T/st/buck_rc"
run_case witness-batch-fails 9 7 1 1 sh -c "echo 9 > $T/st/witness_rc"
run_case witness-summary-incomplete 4 7 1 1 touch $T/st/witness_incomplete
run_case dirty-before 4 7 0 0 touch $T/st/dirty
run_case hash-mismatch-before 4 7 0 0 sh -c "echo '# tampered' >> $T/repo/fbcode/testinfra/tfa/hermit.py"
run_case hash-mismatch-after-containment 4 7 1 0 touch $T/st/buck_mutate
run_case dirty-after-containment 4 7 1 0 touch $T/st/buck_dirty
run_case wrong-head 4 7 0 0 sh -c "echo deadbeef0000 > $T/st/head"
bash -c 'sleep 20; :' fake --exclude "/x/tfa-production-source-$TOKEN" &
BK=$!
WAIT=2 run_case backup-wait-expires 4 7 0 0 noop
kill $BK 2>/dev/null
wait 2>/dev/null
bash -c 'sleep 3; :' fake --exclude "/x/tfa-production-source-$TOKEN" &
WAIT=30 run_case backup-wait-drains 0 0 1 1 noop
wait 2>/dev/null
echo "pass" > $T/stale.PASS
cp $T/stale.PASS $T/stale-pass.PASS
run_case stale-pass 4 7 0 0 noop
mkdir $T/run-run-dir-exists
run_case run-dir-exists 4 7 0 0 noop
check run-dir-exists-reason 1 "$(has "REFUSED: containment evidence dir $T/run-run-dir-exists exists" $T/run-dir-exists.out)"

echo "== containment rc=64: accepted only with Buck's exact Pass 39 / Skip 4 evidence"
vcase() { # name mutation expected_resume_rc expected_whatran_calls expected_out_text
  local egate=7 ewit=0
  [ "$3" = 0 ] && egate=0 ewit=1
  EWR=$4 run_case "$1" "$3" $egate 1 $ewit sh -c "echo 64 > $T/st/buck_rc; echo '$2' > $T/st/mut"
  check "$1-reason" 1 "$(has "$5" $T/$1.out)"
  if [ "$3" = 0 ]; then
    check "$1-accepted" 1 "$(has "full-tfa_production_containment_test rc=64 accepted by verify_containment" $T/$1.out)"
  else
    check "$1-not-accepted" 0 "$(has "rc=64 accepted" $T/$1.out)"
  fi
}
vcase rc64-accepted none 0 1 "containment rc=64 verified: trace="
A=$T/run-rc64-accepted
vargs="--log $T/full-tfa_production_containment_test.log --report $A/command-report.json --build-id $A/build-id --what-ran $A/what-ran.jsonl --repo $T/repo --start-ms 0 --end-ms $(date +%s%3N)"
python3 $T/bin/verify-containment.py --rc 64 $vargs > $T/direct-64.out
check verifier-direct-rc64-replay 0 $?
python3 $T/bin/verify-containment.py --rc 32 $vargs > $T/direct-32.out
check verifier-direct-wrong-rc-exit 1 $?
check verifier-direct-wrong-rc-reason 1 "$(has "REJECTED: rc 32 is not 64" $T/direct-32.out)"
python3 $T/bin/verify-containment.py --rc 64 ${vargs/--start-ms 0/--start-ms $(($(date +%s%3N) + 60000))} > $T/direct-window.out
check verifier-direct-future-window-exit 1 $?
run_case rc0-accepted 0 0 1 1 noop
check rc0-accepted-no-verifier 0 "$(has "rc=64" $T/rc0-accepted.out)"
run_case rc32-propagates 32 7 1 0 sh -c "echo 32 > $T/st/buck_rc"
run_case rc1-propagates 1 7 1 0 sh -c "echo 1 > $T/st/buck_rc"
run_case rc2-propagates 2 7 1 0 sh -c "echo 2 > $T/st/buck_rc"
R64="full-tfa_production_containment_test rc=64 rejected by verify_containment"
vcase text-fail1 t-fail 64 1 "summary"
vcase text-timeout1 t-timeout 64 1 "Timeout 1"
vcase text-fatal1 t-fatal 64 1 "Fatal 1"
vcase text-omit1 t-omit 64 1 "Omit 1"
vcase text-infra1 t-infra 64 1 "Infra Failure 1"
vcase text-build1 t-build 64 1 "Build failure 1"
vcase text-pass38 t-pass38 64 1 "Pass 38"
vcase text-skip3 t-skip3 64 1 "Skip 3"
vcase text-skip5 t-skip5 64 1 "Skip 5"
vcase text-skip-missing t-skip-missing 64 1 "REJECTED: 3 Skip records, want 4"
vcase text-skip-extra t-skip-extra 64 1 "REJECTED: 5 Skip records, want 4"
vcase text-skip-dup-added t-skip-dup-added 64 1 "REJECTED: 5 Skip records, want 4"
vcase text-skip-dup-replaced t-skip-dup-replaced 64 1 "REJECTED: duplicate Skip records"
vcase text-skip-different t-skip-different 64 1 "REJECTED: Skip set"
vcase text-skip-malformed t-skip-malformed 64 1 "REJECTED: malformed Skip record"
vcase text-summary-absent t-summary-absent 64 1 "REJECTED: 0 summary lines, want exactly 1"
vcase text-summary-spoofed t-summary-spoof 64 1 "REJECTED: 2 summary lines, want exactly 1"
vcase text-summary-not-last t-summary-not-last 64 1 "REJECTED: summary is not the last line"
vcase text-spoof-after-failure t-spoof-after-failure 64 1 "REJECTED: 2 summary lines, want exactly 1"
vcase struct-fail s-fail 64 1 "status 1"
vcase struct-omit s-omit 64 1 "status 4"
vcase struct-skip-different s-skip-different 64 1 "REJECTED: structured skip set"
vcase struct-skip-extra s-skip-extra 64 1 "REJECTED: 38 structured passes, want 39"
vcase struct-skip-missing s-skip-missing 64 1 "REJECTED: 40 structured passes, want 39"
vcase struct-skip-message s-skip-message 64 1 "skip message is not the policy message"
vcase struct-two-finish s-two-finish 64 1 "not one start+finish"
vcase struct-stale-ended s-stale-ended 64 1 "ended_time outside run"
vcase struct-stale-mtime s-stale-mtime 64 1 "result file not from this run"
vcase struct-name-mismatch s-name-mismatch 64 1 "does not match"
vcase struct-42runs s-42runs 64 1 "REJECTED: 42 test.run records, want 43"
vcase struct-dup-filter s-dup-filter 64 1 "REJECTED: duplicate test.run for"
vcase struct-no-filter s-no-filter 64 1 "without exactly one --test-filter"
vcase struct-shared-path s-shared-path 64 1 "REJECTED: shared result file"
vcase struct-outside s-outside 64 1 "result file outside buck-out test execution"
vcase struct-symlink-escape s-symlink-escape 64 1 "result file outside buck-out test execution"
vcase struct-foreign s-foreign 64 1 "REJECTED: foreign test record"
vcase struct-run-build-id s-run-build-id 64 1 "REJECTED: test record BUCK_BUILD_ID '"
vcase struct-run-build-id-absent s-run-build-id-absent 64 1 "REJECTED: test record BUCK_BUILD_ID None"
vcase struct-discovery-build-id s-discovery-build-id 64 1 "REJECTED: test record BUCK_BUILD_ID '"
export GSTEP_RC64_VERIFY=true
vcase preset-env-verifier-ignored t-fail 64 1 "REJECTED: summary"
unset GSTEP_RC64_VERIFY
vcase whatran-empty whatran-empty 64 1 "REJECTED: 0 test.run records, want 43"
vcase whatran-fails whatran-fails 64 1 "$R64"
vcase report-exit-mismatch report-exit 64 1 "REJECTED: command report exit_code 0 != rc 64"
vcase report-trace-mismatch report-trace 64 1 "REJECTED: command report trace_id != build-id"
vcase report-missing report-missing 64 1 "REJECTED: FileNotFoundError"
vcase buildid-bad buildid-bad 64 0 "bad build-id"
for c in text-fail1 struct-fail report-trace-mismatch buildid-bad whatran-fails; do
  check "$c-rejection-recorded" 1 "$(has "$R64" $T/$c.out)"
done
EWR=0 run_case rc64-floor-breach 3 7 1 0 sh -c "echo 64 > $T/st/buck_rc; echo breach > $T/st/mut"
check rc64-floor-breach-recorded 1 "$(has "full-tfa_production_containment_test FLOOR-BREACH avail=0" $T/rc64-floor-breach.out)"
check rc64-floor-breach-stopped 1 "$(has "full-tfa_production_containment_test rc=64 stopped-fail-closed" $T/rc64-floor-breach.out)"
check rc64-floor-breach-verifier-not-run 0 "$(has "verify_containment" $T/rc64-floor-breach.out)"
EWR=1 run_case rc64-hash-mismatch-after-containment 4 7 1 0 sh -c "echo 64 > $T/st/buck_rc; touch $T/st/buck_mutate"
EWR=1 run_case rc64-dirty-after-containment 4 7 1 0 sh -c "echo 64 > $T/st/buck_rc; touch $T/st/buck_dirty"
run_case rc64-wrong-head 4 7 0 0 sh -c "echo 64 > $T/st/buck_rc; echo deadbeef0000 > $T/st/head"
run_case rc64-dirty-before 4 7 0 0 sh -c "echo 64 > $T/st/buck_rc; touch $T/st/dirty"
run_case rc64-hash-mismatch-before 4 7 0 0 sh -c "echo 64 > $T/st/buck_rc; echo '# tampered' >> $T/repo/fbcode/testinfra/tfa/hermit.py"

echo "== submit-gate refuses after a PASS if a runner script changes"
run_case tamper-base 0 0 1 1 noop
for f in floor-step.sh verify-containment.py; do
  cp $T/bin/$f $T/$f.keep
  echo "# edit" >> $T/bin/$f
  gate $T/tamper-base.PASS $T/tamper-$f.gate
  check gate-refuses-changed-$f 7 $?
  check gate-stub-not-run-changed-$f 0 "$(grep -c SUBMIT-STUB-RAN $T/tamper-$f.gate)"
  cp $T/$f.keep $T/bin/$f
done
gate $T/tamper-base.PASS $T/tamper-restored.gate
check gate-accepts-restored-scripts 0 $?
echo "== submit-gate refuses after a PASS if the checkout drifts (head, dirty, hermit hash)"
drift() { # name expected-reason base-rc setup...
  local name=$1 why=$2 brc=$3
  shift 3
  EWR=$([ "$brc" = 64 ] && echo 1 || echo 0) run_case $name-base 0 0 1 1 sh -c "echo $brc > $T/st/buck_rc"
  "$@"
  gate $T/$name-base.PASS $T/$name.gate
  check "$name-gate-refuses" 7 $?
  check "$name-stub-not-run" 0 "$(grep -c SUBMIT-STUB-RAN $T/$name.gate)"
  check "$name-reason" 1 "$(grep -c "submit refused: $why" $T/$name.gate)"
}
for brc in 0 64; do
  drift head-drift-rc$brc "head deadbeef0000 != $H" $brc sh -c "echo deadbeef0000 > $T/st/head"
  drift dirty-drift-rc$brc "dirty tree" $brc touch $T/st/dirty
  drift hash-drift-rc$brc "hermit hash mismatch" $brc sh -c "echo '# drift' >> $T/repo/fbcode/testinfra/tfa/hermit.py"
done
echo "TOTAL FAILS=$FAILS"
exit $FAILS
