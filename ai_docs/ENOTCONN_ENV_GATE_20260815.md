<!-- PROVENANCE: snapshot of ignored/enotconn-env-gate.md promoted 2026-08-15 by hermit-coord
     source sha256 da211e82bec05573f4c8183356ecc9553d9087b936a3f5f265eef0c22c53c4bb
     source lines 103
     THIS IS A SNAPSHOT. If `sha256sum ignored/enotconn-env-gate.md` no longer equals the hash
     above, THIS COPY IS STALE. A snapshot with no embedded source hash is
     indistinguishable from a current file. -->
# Transient EdenFS ENOTCONN breaks `/usr/bin/env <cmd>` — detection, and its limits

Measured on devbig030, 2026-08-15, in the hermit-liteinst cell lane.
**Describe-only. Not deployed to shared CI code.** Generalising this into
`ci-hub/` or `ci/test_harness.sh` is a CI-infrastructure change and must go
through the out-of-place clone protocol and the owner.

## The condition

A PATH entry backed by an EdenFS mount can transiently return `ENOTCONN`.
`/usr/bin/env` walks PATH to resolve its command argument and **errors hard**
on `ENOTCONN`, so the whole exec fails:

```
/usr/bin/env: 'bash': Transport endpoint is not connected.
  (If you are seeing this message in an Eden checkout, try running "eden doctor")
```

Exit status is **126**. On this host the offending entry was under
`/home/newton/fbsource/...`.

Every e2e shell fixture begins `#!/usr/bin/env bash`, so while the mount is
disconnected **every shell fixture fails identically**. Ungated, that records as
a wall of red cells indistinguishable from a determinism regression.

**Critical asymmetry:** a merely *nonexistent* PATH entry returns `ENOENT`, and
`env` tolerates it silently. On this host 23 PATH entries were ENOENT the whole
time and caused nothing. **Do not scan PATH for "dead" directories** — that finds
23 harmless entries and misses the one that matters. Only `ENOTCONN` bites.

**It recovers.** Three retries at 3s intervals returned `alive` with no
intervention. `eden doctor` was NOT run.

## What the gate is

`bracket42.sh`, top of file, immediately after `cd "$REPO"` (lines 14-18):

```bash
for _t in 1 2 3 4 5; do
  /usr/bin/env bash -c 'exit 0' 2>/dev/null && break
  [[ $_t == 5 ]] && { printf '%-46s INFRA env-bash ENOTCONN, not a cell result\n' "$1"; exit 9; }
  sleep 5
done
```

It **executes the condition** — a real `/usr/bin/env bash` exec — and retries 5
times at 5s. On persistent failure it emits `INFRA` and exits **9**, a status
distinct from every cell outcome, so no caller can mistake it for a result.

**It does NOT key on exit code 126.** That is deliberate. An exit-code matcher
would suppress any genuine failure that happens to exit 126 — a gate that hides
real red cells, which is the worse direction of error.

## Limits — read these before adopting

1. **It is a PREFLIGHT, not a per-run classifier.** It runs once before a cell is
   bracketed. If the mount drops *mid-batch*, after the probe passed, the gate
   does not fire. This is the main gap.

2. **Mid-batch onset is SAFE in strict mode.** `shrun.sh:39` classifies from the
   `--verify-json` verdict: no verdict file, or `verdict in (null, no_result)`,
   yields `NO-RESULT`, kept distinct from `DIVERGED`. So an rc=126 mid-run
   records as NO-RESULT in both arms — the accept arm does not book a PASS and
   the refuse arm does not book a FAIL. It fails safe.

3. **Mid-batch onset is UNSAFE in exit-code mode.** `shrun.sh:42` classifies
   `case $rc in 0) PASS;; 124|137) TIMEOUT;; *) NONZERO;; esac`. **rc=126 lands in
   `NONZERO`**, i.e. a genuine failure. In a refuse arm that FORGES a refusal; in
   an accept arm it forges a failure. `bracket42.sh` is not exposed because it
   uses strict mode for both arms, but `stripped.sh` and any exit-code-decided
   harness are. **Anyone adopting this must add 126 to the non-result branch, or
   re-probe on any nonzero before classifying.**

   This matters beyond my lane: a cell WITHOUT `assert = { bitwise_parity = true }`
   is decided by the guest exit code in `ci/test_harness.sh` (`:3796`), which is
   the same shape as limit 3.

## The discriminator, for other lanes seeing rc=126

`rc=126` alone is ambiguous. To tell ENOTCONN from a genuine "cannot execute":

- **ENOTCONN:** stderr contains `Transport endpoint is not connected`, and
  `/usr/bin/env bash -c 'exit 0'` fails *right now* but succeeds on retry within
  ~15s. The script file is intact, present, and executable.
- **Genuine:** the script is missing, non-executable, has a bad interpreter line,
  or a wrong cwd. `/usr/bin/env bash -c 'exit 0'` succeeds throughout.

Cheapest single discriminator: run `/usr/bin/env bash -c 'exit 0'; echo $?`
immediately, then again after 10s. `126` then `0` is the transient. `0` both
times means look at the script, not the mount.

Relevant to hermit-coord's noise-band arms 2-4, which died rc=126 "cannot
execute" with the script intact and executable and were attributed to cwd drift.
Those are competing explanations for one symptom and the probe above separates
them. This note does not claim coord's failures WERE ENOTCONN — only that the
symptom matches and is cheap to discriminate.

## Carrier caveat

This file and `ignored/mechanical-78-cells.tsv` are both under `ignored/`, which
is gitignored and therefore **local to devbig030 and does not travel by git**.
For genuine cross-lane adoption they need promotion to a tracked path (e.g.
parent `ai_docs/`) through the serialized parent-main writer. Flagged, not done.
