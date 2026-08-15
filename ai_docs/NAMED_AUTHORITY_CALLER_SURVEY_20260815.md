# Do the authorities our policy names actually have callers?

Survey run 2026-08-15 by hermit-coord (devbig030). Read-only; nothing changed.

## SCOPE — STATED HERE, NOT IN A FOOTNOTE, BECAUSE IT IS A REAL COVERAGE GAP

**THIS SURVEY COVERS EXECUTABLE AUTHORITIES ONLY.** The method greps for
invocation-shaped references to a named script or binary. **It structurally
cannot find the `virtualize_time` class of defect — a rationale that is real but
lives one repository away from the code it governs — because that authority is
prose, not an executable.** One of the three calibration cases is therefore out
of reach by construction. A separate method is needed for prose authorities and
this is not it.

## THE METHOD FAILED ITS FIRST CALIBRATION AND THAT IS REPORTED, NOT HIDDEN

Three known instances were used as a calibration set. **Pass 1 reported
`agent_liveness_probe.py` with 4 production callers when it has none** — the
filter excluded `.md` and tests but not comments and docstrings inside `.py`
files, and all four hits were prose:

    operational_health.py:905  needs an independent source such as `...agent_liveness_probe.py`.
    slot_disk_residue.py:13    uses that census or `agent_liveness_probe.py`.
    agent_identity.py:6        ``agent_liveness_probe.py``.  Slot, pane, cgroup...

**Without a calibration case that pass would have reported the whole population
healthy.** Pass 2 adds a comment/docstring filter and an invocation-shaped
requirement; the probe then correctly returns 0.

**PASS 2 HAS TWO KNOWN FAILURE MODES, BOTH FOUND BY COMPLETING THE SURVEY.**

- **FALSE POSITIVE on echoed paths.** `close-task` returns 1, and that 1 is an
  `echo` line in `orphaned-task-detector.sh`. **Counts of 1-2 are unreliable.**
- **FALSE ZERO on imported modules.** `validate_rows.py` returns 0 invocation
  callers and **has 15 importers** (`import validate_rows as canonical_ledger`
  across `ci-hub/validate/`, `history/`, `remediation/`, `health/`, `ledger/`).
  A Python module is IMPORTED, not invoked, and an invocation-shaped matcher
  cannot see that. Same for `validate_status.rs`, which is not invoked at all —
  it is compiled in via `#[path] mod validate_status` at `ci-hub.rs:49`.
  **A zero means "no invocation", NOT "no caller".**

**SELF-CORRECTION TO THE FIRST PUBLICATION OF THIS SURVEY.** It reported
`scripts/check-reverie-pin.rs` as `exists=NO-FILE` and flagged that as
"exactly the shape this survey exists to find". **That was MY path error, not a
defect.** The file is at `hermit/scripts/check-reverie-pin.rs`; `AGENTS.md` links
it by its correct GitHub URL under `rrnewton/hermit`. I had put a parent-relative
path in the input list. Its 6 "callers" are self-references inside the file. **The
alarm was mine and the mechanism is fine.**

## THE CLASS SPLIT THAT MATTERS: A MISSING CALLER MEANS DIFFERENT THINGS

**INTENDED-INTERACTIVE.** A mandated CLI entry point an agent is required to run
by hand. **It has no code caller BY CONSTRUCTION, so a zero says nothing about
enforcement.** The real question is whether any gate verifies the tool was
actually used — an unverified disclosure prefix is the same shape as a
hand-applied label: an assertion with no oracle behind it.

**INTENDED-PROGRAMMATIC.** A mechanism the policy presents as an authority
consulted by a decision procedure. **Here a missing caller means the mechanism is
inert.** This is the defect found on the probe, and only this one.

## RESULT — 16 OF 16 SURVEYED

| mechanism | invocation-callers | class | verdict |
|---|---|---|---|
| `ci-hub/health/agent_liveness_probe.py` | **0** | intended-PROGRAMMATIC | **DEFECT** — see below |
| `ci-hub/bin/who-am-i` | 0 | intended-interactive | healthy, oracle exists |
| `ci-hub/bin/close-task` | 1 (an `echo`) | intended-interactive | healthy, oracle exists |
| `ci-hub/bin/review-attest` | 0 | intended-interactive | healthy, oracle exists |
| `.orc/.../gh-pr-close-test.sh` | 0 | UNCLASSIFIED | unsurveyed intent |
| `.orc/.../gh-issue-create` | 2 | wrapper | called |
| `scripts/release-worktree.rs` | 4 | programmatic | called |
| `scripts/parent-main-write` | 4 | programmatic | called |
| `scripts/allocate-worktree.rs` | 8 | programmatic | called |
| `scripts/setup-hooks.sh` | 64 | programmatic | called |
| `ci-hub/bin/gh` | 802 | wrapper | called |
| `scripts/allocate-worktree.rs` | 8 | programmatic | called |
| `ci-hub/landing/land-pr.sh` | 5 | programmatic | called |
| `hermit/scripts/check-reverie-pin.rs` | n/a | programmatic | **present**; earlier NO-FILE was a path error of mine |
| `ci-hub/ledger/validate_rows.py` | 0 invocations / **15 imports** | programmatic | called, via import |
| `ci-hub/lib/validate_status.rs` | 0 invocations / compiled in | programmatic | called, via `mod` |
| `ci-hub/health/approval_binding.py` | 3 invocations + 1 import | programmatic | called |

## THE INTERACTIVE GROUP IS HEALTHY — ALL THREE HAVE ORACLES

This is the reassuring half and it was the open question:

- **`who-am-i`** — its disclosure line IS verified. `.githooks/commit-msg` and
  `.githooks/dev-hermit-hook-dispatch` invoke `scripts/check_commit_attribution.py`.
  **Observed live twice this session:** it refused a malformed role/team line and
  refused a commit whose local main was stale against origin.
- **`close-task`** — closures without it ARE detectable.
  `ci-hub/closure/verified_close.py` emits `CLOSURE-VERIFIED` and
  `operational_health.py:1013` queries task notes for its absence.
- **`review-attest`** — its output IS consumed. `approval_binding.py` binds each
  `passed-review-*` to an exact head, and `cherry_pick_context.py:63` parses the
  `APPROVED-AT` line. **Observed live this session:** the gate read an attestation
  posted minutes earlier and returned `state=ok`.

**So three of the four zero-caller mechanisms are not defects. Only the probe is.**

## THE ONE DEFECT, AND ITS SEVERITY IS BOUNDED

`AGENTS.md:81` presents `agent_liveness_probe.py` as the authority for the
slot-reclaim decision. **No production caller invokes it.** But
`scripts/release-worktree.rs` enforces independently via
`verify_release_authority_absent` / `verify_recorded_owners_absent`, walking
`/proc` directly. **So the probe is ADVISORY and its absence from the code path
does not defeat reclaim enforcement** — the doc overstates what it enforces
rather than the enforcement being missing. Detail in
`LIVENESS_PROBE_USAGE_ERROR_FAILS_OPEN_20260815.md`.

## OUTCOME CLASSES OBSERVED

1. **No implementation at all** — the approval blob rule (calibration case;
   established separately, zero hits in code).
2. **Implemented but uncalled AND intended-programmatic** — the liveness probe.
   The only confirmed instance.
3. **Implemented, called, doc overstates what it enforces** — the probe again, in
   its second aspect. The class a reader is least likely to catch, because the
   mechanism genuinely exists and genuinely runs.
4. **Implemented, uncalled, intended-interactive, oracle present** — `who-am-i`,
   `close-task`, `review-attest`. **Not defects**, and lumping them with class 2
   would have manufactured three.

## FINAL CLASSIFICATION

1. **No implementation at all** — the approval blob rule. Calibration case,
   established separately; not an executable, so outside this method's reach and
   included only because it was seeded.
2. **Intended-programmatic and INERT** — `ci-hub/health/agent_liveness_probe.py`.
   **The only confirmed defect in 16.**
3. **Intended-interactive WITH an enforcer** — `who-am-i`, `close-task`,
   `review-attest`. Not defects.
4. **Intended-interactive WITHOUT an enforcer** — **EMPTY.** This was the bucket
   expected to be interesting and nothing landed in it. Every mandated CLI tool
   surveyed has something that verifies it was used.
5. **AMBIGUOUS, not forced** — `.orc/plugins/hermit-dev/gh-pr-close-test.sh`,
   0 callers. Its name suggests a test harness, in which case zero callers is
   correct and it belongs in no defect class; but it sits in a plugin directory
   beside a mandated wrapper. **Intent unresolved and NOT guessed.**

Everything else (11 of 16) is programmatic and genuinely called.

## LIVE-OBSERVATION STATUS, PER MECHANISM

Observing an enforcer refuse is worth more than reading that it exists. Where
that was available cheaply it was done; where not, it is said rather than
silently downgraded.

- **`who-am-i` — OBSERVED REFUSING, twice this session.** A malformed role/team
  line and a stale-local-main commit were both rejected by
  `check_commit_attribution.py` via `.githooks/commit-msg`.
- **`review-attest` — OBSERVED ENFORCING.** `approval_binding --gate` read an
  attestation posted minutes earlier and returned `state=ok`; the same tool's
  `--who`/`--reviewer` guard refused a self-attestation earlier in the session.
- **`close-task` — SOURCE READ ONLY, no live observation.** The detector at
  `operational_health.py:1013` queries task notes for missing `CLOSURE-VERIFIED`.
  Triggering it would require closing a task incorrectly, which is a mutation and
  was not done. **Marked unobserved rather than downgraded silently.**
- **`agent_liveness_probe.py` — OBSERVED, and its rc semantics measured directly**
  (rc 1 alive on a live owner; rc 0 from a mistyped flag through a pipe).

## SCOPE THAT COMPLETING THE SEVEN DOES *NOT* CLOSE

The header caveat stands unchanged and completing the table does not weaken it:
**this survey covers executable authorities only and structurally cannot
rediscover the `virtualize_time` case.** 16 of 16 executables is not 16 of 16
authorities. **Every prose authority in `AGENTS.md` and the canonical skills
remains unsurveyed**, and a different method is required for them.

<!-- PROVENANCE: promoted 2026-08-15 by hermit-coord. Finished artifact; scan 16 of 16 complete.
     AGENTS.md                                    sha256 50b9f56e89f1d9cfa54a85e33e86003d1442314f68607f012f5b721aeb7ccc18
     ci-hub/health/agent_liveness_probe.py        sha256 2bf2d9190da1e28978619b37eb456134fb6d9666f2fd1ef9befa48ae618f3528
     scripts/release-worktree.rs                  sha256 2f4d3806efb57e13fd413248d629f92379c8ae3029f53869972aa24eca69c0f2
     hermit/scripts/check-reverie-pin.rs          sha256 f24aeff6c48e9c054283b7680d908814046a92a7e7a9ae5e1405210e43940b09
     Method: invocation-shaped grep, comment/docstring excluded. KNOWN LIMITS: counts of 1-2
     unreliable (echoed paths); a 0 means no INVOCATION and misses Python imports and Rust
     `mod` inclusion. Executable authorities only; prose authorities are out of reach. -->
