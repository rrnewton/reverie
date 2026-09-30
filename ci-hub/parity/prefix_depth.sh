#!/usr/bin/env bash
# prefix_depth.sh — PREFIX PARITY DEPTH: how many DETCORE COMMIT records into the
# INFO log a backend stays identical to the ptrace golden log.
#
# WHY THIS SHAPE. "does demo05 boot" is a BOOLEAN. It reads 0 for months and
# cannot show progress, so it cannot be ratcheted. Prefix depth is MONOTONIC:
# every divergence fixed moves the number up, so partial progress is visible.
#
# Y/Z where Z = COMMIT records in the golden ptrace log, Y = length of the
# identical leading run. Y=0 with a known Z is a REAL DATUM; no measurement is not.
#
# This is INFO-log depth. Today's published parity is STDOUT-ONLY, so this is
# strictly stronger and WILL read worse. That is correct, not a regression.
set -uo pipefail
ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
readonly ROOT_DIR
LU=${LU:-$ROOT_DIR/ignored/lu-parity/usr/lib64}
export LD_LIBRARY_PATH="$LU"
H=${HERMIT:-$ROOT_DIR/ignored/prefix-build/target/release/hermit}
OUT=${OUT:-$ROOT_DIR/scratch/prefix}
mkdir -p "$OUT"

commits() {  # $1=logfile+errfile prefix -> ordered COMMIT records, normalised
  # STREAM ORDERING: REFUSE, do not concatenate.
  #
  # This used to `cat "$1".log "$1".err`. When a backend emits COMMIT records on
  # BOTH streams that appends all .err records after all .log records, so the
  # "prefix" is a prefix of A SEQUENCE THAT NEVER OCCURRED -- an ORDERING
  # artifact, not a magnitude one, and invisible in the output. Interleaving
  # cannot be recovered after the fact: the two streams are separately buffered
  # and the log file carries no ordering relation to stderr.
  # So: use whichever stream carries the records, and if BOTH do, refuse rather
  # than invent an order. ptrace writes to the log file, sabre to stderr; the
  # both-streams case is the one that silently fabricated a sequence.
  local log_n err_n
  # NB `grep -c` prints 0 AND exits 1 on no-match, so a `|| echo 0` fallback
  # appends a SECOND zero and yields the literal "0\n0", which then fails
  # `[ -gt ]` with "integer expression expected". Capture plainly and default
  # only when the file is absent.
  log_n=$(grep -c 'COMMIT turn' "$1".log 2>/dev/null); log_n=${log_n:-0}
  err_n=$(grep -c 'COMMIT turn' "$1".err 2>/dev/null); err_n=${err_n:-0}
  if [ "$log_n" -gt 0 ] && [ "$err_n" -gt 0 ]; then
    echo "AMBIGUOUS-STREAM-ORDER: $log_n on .log and $err_n on .err;" \
         "interleaving is unrecoverable, refusing to fabricate a sequence" >&2
    return 3
  fi
  if [ "$err_n" -gt 0 ]; then cat "$1".err; else cat "$1".log 2>/dev/null; fi \
    | grep -o 'COMMIT turn .*' \
    | sed -E 's/0x[0-9a-f]+/HEX/g'
}

# THE PROLOGUE. Every guest begins with the same process-setup records, and for a
# dynamically linked guest the dynamic loader's opens as well. MEASURED
# 2026-08-07 at hermit g294e89bfeeeb, --base-env minimal:
#   /bin/true, /bin/echo, wc   ParentContinue, MemAddrSpace, Path(ld.so.cache), Path(libc.so.6)
#   static guests              ParentContinue, MemAddrSpace                (no loader)
# so the first 2 records (static) or 4 (dynamic) are IDENTICAL regardless of what
# the guest does. A depth at or below that boundary says the backend agreed
# through process startup and NOTHING about the guest -- which is why dbi=3,
# sabre=1 and liteinst=8 were constant while Z varied 2,765x. Depth is reported
# against the prologue boundary for that reason, and a depth inside the prologue
# is labelled rather than published as coverage.
prologue_len() { # $1=prefix -> number of leading records that are process/loader setup
  commits "$1" 2>/dev/null | grep -nE 'ParentContinue|MemAddrSpace|Path\("/etc/ld\.so|Path\("/lib' \
    | awk -F: 'NR==1&&$1!=1{exit} {last=$1} END{print last+0}'
}

run() { # $1=backend $2=tag $3.. = guest argv
  local be="$1" tag="$2"; shift 2
  timeout 240 "$H" --log=info --log-file="$OUT/$tag.$be.log" \
      --backend "$be" run -- "$@" >/dev/null 2>"$OUT/$tag.$be.err"
  echo $?
}

depth() { # $1=golden-prefix $2=cand-prefix -> Y
  commits "$1" > "$OUT/.g"; commits "$2" > "$OUT/.c"
  awk 'NR==FNR{g[FNR]=$0;n=FNR;next}{if(FNR>n||$0!=g[FNR]){print FNR-1;found=1;exit}}
       END{if(!found)print (FNR<n?FNR:n)}' "$OUT/.g" "$OUT/.c"
}

# PROVENANCE OF THE BINARY, NOT OF A CHECKOUT.
#
# This used to be `git -C <parent>/hermit rev-parse HEAD` -- the PRIMARY's HEAD,
# which is not where $H came from and is not even the same tree. $H defaults to
# ignored/prefix-build/target/, a CARGO_TARGET_DIR holding no source at all, and
# $HERMIT can point anywhere. So the old stamp described a checkout that had no
# causal link to the measurement, while looking like a precise 40-hex
# attribution. Measured 2026-08-07: the stamp read
# f89c69766371806d3c9b2c3003531df2d59d6118 (clean) for a binary that self-reports
# gf89c69766371-DIRTY. The SHA even matched -- only the qualification that makes
# it meaningless was dropped, which is the worst case: unfalsifiable because it
# looks right. ai_docs/measurements/prefix-parity-depth-ratchet_20260806.md
# carries that false-precision stamp.
#
# The binary embeds its own build provenance (hermit-cli/build.rs ->
# HERMIT_BUILD_GIT_SHA, `<ver> (<date>, g<sha12>[-dirty])`), where -dirty means
# tracked/index changes existed in the BUILD tree. Ask the artifact that produced
# the numbers, and fail closed to a loud UNKNOWN rather than to anything that
# could be mistaken for a clean commit.
#
# A -dirty build is NOT re-identifiable from its SHA, so binary_sha256 is stamped
# too: it is the only field that distinguishes two different dirty builds of the
# same commit. Do not drop it because the SHA "looks specific enough".
binary_provenance() { # $1=path to hermit binary -> g<sha>[-dirty] | UNKNOWN(reason)
  local bin="$1" v g
  [ -x "$bin" ] || { echo "UNKNOWN(no-executable-at:$bin)"; return 1; }
  v=$("$bin" --version 2>/dev/null) || { echo "UNKNOWN(version-call-failed)"; return 1; }
  # format: hermit <ver> (<date>, g<sha12>[-dirty]); build.rs emits g<sha>=unknown
  # outside a checkout, which must stay visible rather than becoming a blank.
  g=$(printf '%s' "$v" | sed -n 's/.*, \(g[^)]*\)).*/\1/p' | head -n1)
  # The stamp is a single-line record, so an unrecognised --version must not be
  # pasted in raw: a multi-line or long banner would break the line and spill
  # into the record (observed with coreutils' two-line --version). Collapse to
  # the first line, strip whitespace runs, and cap it -- enough to recognise what
  # was pointed at, never enough to corrupt the format.
  [ -n "$g" ] || {
    local first
    first=$(printf '%s' "$v" | head -n1 | tr -s '[:space:]' ' ' | cut -c1-60)
    echo "UNKNOWN(unparsable-version:${first})"
    return 1
  }
  printf '%s\n' "$g"
}
GOLDEN_SHA=$(binary_provenance "$H")
BIN_SHA256=$(sha256sum "$H" 2>/dev/null | cut -d' ' -f1); [ -n "$BIN_SHA256" ] || BIN_SHA256=UNKNOWN
case "$GOLDEN_SHA" in
  UNKNOWN*) echo "WARNING: cannot attribute this measurement to any source revision:" \
                 "$GOLDEN_SHA (binary=$H). Values below are UNATTRIBUTED." >&2 ;;
  *-dirty)  echo "WARNING: binary was built from a DIRTY tree ($GOLDEN_SHA)." \
                 "The commit does not identify the source; binary_sha256=$BIN_SHA256" \
                 "is the only handle on what actually ran." >&2 ;;
esac
# Identity alone is not staleness. The stamp above says WHICH binary; this says
# how far behind main it is -- the 23-commits-behind binary that produced three
# false nondeterminism verdicts on 2026-08-07 had a perfectly precise identity.
BIN_PROV=$(python3 - "$H" "$ROOT_DIR/hermit" "$ROOT_DIR/ci-hub" 2>/dev/null <<'PROV'
import sys
sys.path.insert(0, sys.argv[3])
try:
    from provenance import binary_provenance
except ImportError:
    print("staleness=UNAVAILABLE"); raise SystemExit(0)
print(binary_provenance(sys.argv[1], sys.argv[2]).render())
PROV
)
[ -n "$BIN_PROV" ] && echo "$BIN_PROV" >&2
DATE=$(date -u +%Y-%m-%d)
# EMIT is the candidate's own comparable-record count. It is printed as its own
# column because Y alone cannot distinguish "compared and diverged immediately"
# (Y=0, EMIT>0) from "produced nothing to compare" (NO-RUN, EMIT=0). Z is the
# denominator for both: a count is self-describing only next to the size of the
# thing it counted.
printf '%-22s %-9s %6s %6s %6s  %-8s %s\n' GUEST BACKEND Y Z EMIT RC NOTE
status=0
for spec in "$@"; do
  tag="${spec%%=*}"; cmd="${spec#*=}"
  # shellcheck disable=SC2086
  grc=$(run ptrace "$tag" $cmd)
  Z=$(commits "$OUT/$tag.ptrace" | wc -l)
  # A4: Z is the denominator for every row of this guest. If the golden run
  # produced no comparable records there is nothing to be a prefix OF, and the
  # self-reference row would otherwise print Y=0 Z=0 -- which reads as the golden
  # matching itself perfectly, the most reassuring line on the page -- and every
  # backend row would be Y/0. A missing golden is fatal for the guest, not a zero.
  if [ "$Z" -eq 0 ]; then
    printf '%-22s %-9s %6s %6s %6s  rc=%-5s %s\n' \
      "$tag" "ptrace(golden)" "NO-GOLDEN" "0" "0" "$grc" \
      "golden emitted 0 COMMIT records; no denominator, so no rows for this guest"
    status=2
    continue
  fi
  printf '%-22s %-9s %6s %6s %6s  rc=%-5s %s\n' "$tag" "ptrace(golden)" "$Z" "$Z" "$Z" "$grc" "self-reference"
  for be in dbi sabre e9patch; do
    # shellcheck disable=SC2086
    rc=$(run "$be" "$tag" $cmd)
    emitted=$(commits "$OUT/$tag.$be" | wc -l)
    # A3: NO-RUN is decided by the RECORDS, not by the exit code. The old guard
    # was AND-gated (records==0 && rc!=0), so a backend that exits 0 while
    # emitting nothing fell through to depth(), whose awk prints 0 for an empty
    # candidate file, and Y=0 was published as "diverges at record 0". That is
    # sabre's documented failure mode exactly: patched_sites=0, silent ptrace
    # fallback, rc=0. Zero comparable records is NOT-EXERCISED whatever the rc.
    if [ "$emitted" -eq 0 ]; then
      printf '%-22s %-9s %6s %6s %6s  rc=%-5s %s\n' \
        "$tag" "$be" "NO-RUN" "$Z" "0" "$rc" "emitted 0 comparable records (not a depth of 0)"
      continue
    fi
    Y=$(depth "$OUT/$tag.ptrace" "$OUT/$tag.$be")
    # A depth at or inside the PROLOGUE is not coverage. Label it instead of
    # publishing a ratio: Y/Z with Y in the prologue says the backend agreed
    # through process startup, and Z only says how long the guest ran -- which
    # is why 3/145 and 3/400940 are THE SAME FACT.
    PL=$(prologue_len "$OUT/$tag.ptrace")
    if [ "${Y:-0}" -le "${PL:-0}" ]; then
      note="PROLOGUE-ONLY (prologue=$PL): agreement is process/loader setup, NOT guest execution; do not quote Y/Z as coverage"
    else
      note="beyond prologue by $((Y-PL)) record(s) (prologue=$PL)"
    fi
    printf '%-22s %-9s %6s %6s %6s  rc=%-5s %s\n' "$tag" "$be" "$Y" "$Z" "$emitted" "$rc" "$note"
  done
done
echo
echo "golden_sha=$GOLDEN_SHA  binary=$H  binary_sha256=$BIN_SHA256  flags='--log=info --backend <be>'  date=$DATE  metric=COMMIT-record prefix depth (INFO log)"
# Distinct exit status so a consumer can tell "measured" from "could not measure":
# 0 = every guest had a golden denominator, 2 = at least one guest was NO-GOLDEN.
exit "$status"
