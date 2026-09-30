#!/usr/bin/env bash
# strace_attach_litmus.sh -- INSTRUMENT for the no-ptracer acceptance litmus.
#
# THE LITMUS. Linux permits exactly ONE tracer per process. So if `strace` can
# trace a hermit run end-to-end, there provably is no ptracer in the path.
# That is POSITIVE, UNFAKEABLE evidence. "we removed the ptrace calls" is not:
# a silent fallback satisfies that sentence too, which is precisely how a
# backend can report success while a ptracer is still doing the work.
#
# THIS SCRIPT IS THE INSTRUMENT, NOT THE ACCEPTANCE CHECK. The acceptance task
# is deliberately gated behind the ptracer-removal steps so that nobody records
# a pass against the pre-removal state. Running this to CHARACTERISE a backend
# is fine; quoting it as an acceptance pass before those steps land is not.
#
# THREE OUTCOMES, DELIBERATELY NOT TWO:
#
#   ATTACHED                 strace traced the whole run; no ptracer contended
#   REFUSED-ALREADY-TRACED   hermit could not become tracer because strace is
#                            -- THE INFORMATIVE RESULT, and the reason this is
#                            not folded into ERROR. Collapsing it would rebuild
#                            the ambiguous zero somewhere new: a no-result and
#                            a real finding would share one value again.
#   ERROR                    anything else (strace missing, guest missing,
#                            yama/permission block, harness fault)
#
# WHY THE VERDICT IS NOT KEYED ON EPERM. `ptrace(...) = -1 EPERM` is ambiguous:
# yama ptrace_scope, a uid mismatch, and a dead process all produce it. Keying
# on it would be a proxy. The verdict instead binds to observable evidence:
#   - the EPERM must be on a ptrace op that ESTABLISHES tracing
#     (PTRACE_ATTACH / PTRACE_SEIZE / PTRACE_TRACEME), and
#   - yama ptrace_scope is read and reported, so a yama block is separable, and
#   - in attach mode, /proc/<pid>/status TracerPid is read directly: it NAMES
#     the tracer rather than leaving it to be inferred from an error code.
set -uo pipefail

STRACE=${STRACE:-strace}
BACKEND=""
GUEST=()
MODE=wrap
KEEP_LOG=0
TIMEOUT=${TIMEOUT:-120}
OUTDIR=${OUTDIR:-}

usage() {
    cat >&2 <<USAGE
usage: $0 --backend <be> [--mode wrap|attach] [--timeout N] [--keep-log] -- <guest argv...>

  --mode wrap    (default) run \`strace -f hermit --backend <be> run -- <guest>\`.
                 Exercises the one-tracer rule directly: strace claims the
                 guest first, so a backend that needs to ptrace it cannot.
  --mode attach  start hermit, then attach strace to the live hermit pid and
                 read TracerPid. Independent second signal.

  HERMIT env var selects the binary under test.
USAGE
}

while (($#)); do
    case $1 in
    --backend) BACKEND=$2; shift 2 ;;
    --mode) MODE=$2; shift 2 ;;
    --timeout) TIMEOUT=$2; shift 2 ;;
    --keep-log) KEEP_LOG=1; shift ;;
    -h | --help) usage; exit 0 ;;
    --) shift; GUEST=("$@"); break ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
    esac
done

# A missing precondition is ERROR, never a quiet pass. Report the verdict and
# the reason on one line so a caller cannot read a blank as success.
verdict() {
    local v=$1; shift
    echo "VERDICT=$v"
    echo "REASON=$*"
}

[[ -n $BACKEND ]] || { verdict ERROR "no --backend given"; exit 3; }
((${#GUEST[@]})) || { verdict ERROR "no guest argv after --"; exit 3; }
HERMIT=${HERMIT:-}
[[ -n $HERMIT && -x $HERMIT ]] || { verdict ERROR "HERMIT is unset or not executable: '${HERMIT}'"; exit 3; }
command -v "$STRACE" >/dev/null 2>&1 || { verdict ERROR "strace not found on PATH (set STRACE=)"; exit 3; }

# yama ptrace_scope is read and REPORTED, so a yama refusal can never be
# mistaken for "already traced". This is the discriminator that keeps a
# permission block out of the informative bucket.
YAMA=$(cat /proc/sys/kernel/yama/ptrace_scope 2>/dev/null || echo "unavailable")

workdir=$(mktemp -d -t strace-litmus-XXXXXX)
cleanup() { ((KEEP_LOG)) || rm -rf "$workdir"; }
trap cleanup EXIT
log=$workdir/strace.log
hermit_err=$workdir/hermit.err

echo "HARNESS=strace_attach_litmus.sh"
echo "MODE=$MODE"
echo "BACKEND=$BACKEND"
echo "GUEST=${GUEST[*]}"
echo "HERMIT=$HERMIT"
echo "STRACE_VERSION=$("$STRACE" --version 2>&1 | head -1)"
echo "YAMA_PTRACE_SCOPE=$YAMA"

# A ptrace op that ESTABLISHES tracing. An EPERM on one of these is what "some
# other process already owns this tracee" looks like; an EPERM on PTRACE_PEEK*
# or a ptrace op against an unrelated pid is not, and must not be counted.
ESTABLISHING='PTRACE_ATTACH|PTRACE_SEIZE|PTRACE_TRACEME'

# Count matching lines as EXACTLY ONE integer, and fail loudly if the file
# cannot be read.
#
# `grep -c` PRINTS "0" *and* EXITS 1 when there are no matches, so the idiom
# `n=$(grep -c ... || echo 0)` captures BOTH outputs and yields the two-line
# string "0\n0". That did two things here: it broke `(( n > 0 ))` with an
# arithmetic syntax error on stderr, and -- worse -- it injected a bare "0" line
# into this script's key=value stdout, so a parser reading the instrument's
# output saw an orphan line between fields.
#
# Assign first, THEN correct on failure, so exactly one integer is produced:
#   rc 0  matches found          -> the count
#   rc 1  ran fine, no matches   -> a real, measured 0
#   rc >1 could not read at all  -> NOT a zero. A setup failure is zero
#                                   qualifying trials, never a negative
#                                   finding, so it returns non-zero and the
#                                   caller must report ERROR rather than 0.
count_matches() {
    local pattern=$1 file=$2 n rc
    n=$(grep -cE "$pattern" "$file" 2>/dev/null)
    rc=$?
    ((rc <= 1)) || return 2
    printf '%d' "${n:-0}"
}

classify_wrap() {
    local rc=$1
    local contended established

    # NO-RESULT GUARD RUNS FIRST, BEFORE ANY COUNT IS PRINTED. An absent or
    # empty log means strace never traced anything. Emitting "0" counts for
    # such a run would state a measured absence of contention that was never
    # measured -- the ambiguous zero this harness exists to avoid. The counts
    # are reported as n/a so a no-result can never be read as a clean run.
    if [[ ! -s $log ]]; then
        echo "STRACE_LOG_LINES=0"
        echo "PTRACE_ESTABLISHING_CALLS=n/a"
        echo "PTRACE_ESTABLISHING_EPERM=n/a"
        echo "HERMIT_RC=$rc"
        verdict ERROR "strace produced an empty log; nothing was traced, so this is a no-result not a pass"
        return 4
    fi

    # Evidence, not inference: an EPERM on an establishing op inside the traced
    # process tree. STRACE_LOG_LINES is the denominator these counts are drawn
    # from -- a count without it cannot be audited.
    established=$(count_matches "ptrace\((${ESTABLISHING})" "$log") || {
        verdict ERROR "could not read the strace log at $log; no trial was measured"
        return 4
    }
    contended=$(count_matches "ptrace\((${ESTABLISHING}).*= *-1 +EPERM" "$log") || {
        verdict ERROR "could not read the strace log at $log; no trial was measured"
        return 4
    }
    echo "STRACE_LOG_LINES=$(wc -l <"$log")"
    echo "PTRACE_ESTABLISHING_CALLS=$established"
    echo "PTRACE_ESTABLISHING_EPERM=$contended"
    echo "HERMIT_RC=$rc"
    if ((contended > 0)); then
        verdict REFUSED-ALREADY-TRACED \
            "hermit issued $contended establishing ptrace call(s) that returned EPERM while strace held the tracee; a ptracer is in the path for backend '$BACKEND'"
        return 1
    fi
    if ((rc == 0)); then
        verdict ATTACHED \
            "strace traced the run end-to-end and hermit exited 0 with no contended ptrace attach; no ptracer contended for backend '$BACKEND'"
        return 0
    fi
    verdict ERROR "hermit exited $rc under strace with no contended establishing ptrace call; not a tracer conflict (see $log)"
    return 4
}

run_wrap() {
    timeout "$TIMEOUT" "$STRACE" -f -qq -o "$log" \
        "$HERMIT" --backend "$BACKEND" run -- "${GUEST[@]}" \
        >/dev/null 2>"$hermit_err"
    local rc=$?
    if ((rc == 124)); then
        verdict ERROR "hermit under strace exceeded ${TIMEOUT}s"
        return 4
    fi
    classify_wrap "$rc"
}

run_attach() {
    timeout "$TIMEOUT" "$HERMIT" --backend "$BACKEND" run -- "${GUEST[@]}" \
        >/dev/null 2>"$hermit_err" &
    local pid=$!
    # Give the run time to reach a steady state before probing.
    sleep 2
    if ! kill -0 "$pid" 2>/dev/null; then
        wait "$pid"; local rc=$?
        verdict ERROR "hermit exited (rc=$rc) before strace could attach; guest too short-lived for attach mode -- use --mode wrap"
        return 4
    fi
    # THE BINDING: TracerPid NAMES the tracer. Not inferred from an error code.
    local tracer_pid tracer_name
    tracer_pid=$(awk '/^TracerPid:/{print $2}' "/proc/$pid/status" 2>/dev/null)
    tracer_name=$(cat "/proc/${tracer_pid}/comm" 2>/dev/null || echo "unknown")
    echo "HERMIT_PID=$pid"
    echo "TRACERPID_BEFORE=${tracer_pid:-unreadable}"
    echo "TRACER_COMM_BEFORE=$tracer_name"

    timeout 10 "$STRACE" -qq -p "$pid" -o "$log" >/dev/null 2>"$workdir/strace.err" &
    local spid=$!
    sleep 2
    local tracer_after
    tracer_after=$(awk '/^TracerPid:/{print $2}' "/proc/$pid/status" 2>/dev/null)
    echo "TRACERPID_AFTER=${tracer_after:-unreadable}"
    echo "STRACE_PID=$spid"
    local serr; serr=$(head -3 "$workdir/strace.err" 2>/dev/null | tr '\n' ' ')
    echo "STRACE_STDERR=${serr}"

    kill -- "$spid" 2>/dev/null; wait "$spid" 2>/dev/null
    kill -- "$pid" 2>/dev/null; wait "$pid" 2>/dev/null

    if [[ -n ${tracer_after:-} && $tracer_after == "$spid" ]]; then
        verdict ATTACHED "strace ($spid) became the tracer of hermit pid $pid; TracerPid names it, so no ptracer held it"
        return 0
    fi
    if [[ -n ${tracer_before:-$tracer_pid} && ${tracer_pid:-0} -ne 0 ]]; then
        verdict REFUSED-ALREADY-TRACED \
            "hermit pid $pid already had TracerPid=$tracer_pid ($tracer_name) before strace attached"
        return 1
    fi
    if grep -qE "ptrace\((${ESTABLISHING}).*EPERM|Operation not permitted" "$workdir/strace.err" 2>/dev/null; then
        if [[ $YAMA != "0" && $YAMA != "unavailable" ]]; then
            verdict ERROR "strace attach refused with EPERM and yama ptrace_scope=$YAMA; this is a permission block, NOT evidence of an existing tracer"
            return 4
        fi
        verdict REFUSED-ALREADY-TRACED "strace attach refused on an establishing ptrace op with yama ptrace_scope=0"
        return 1
    fi
    verdict ERROR "strace neither became the tracer nor was refused on an establishing op (TracerPid after=${tracer_after:-unreadable})"
    return 4
}

case $MODE in
wrap) run_wrap ;;
attach) run_attach ;;
*) verdict ERROR "unknown --mode '$MODE'"; exit 3 ;;
esac
