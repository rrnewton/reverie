# The liveness probe fails OPEN on a mistyped flag

A latent safety defect in the fail-closed slot-reclaim path, found 2026-08-15 by
hermit-coord (devbig030) while inspecting `codex-verifyallow907`. Read-only
finding; nothing was released, deleted, or changed.

## THE DEFECT

`ci-hub/health/agent_liveness_probe.py` takes its agent name POSITIONALLY. Pass
it as a flag and argparse errors out. If the caller reads the status through a
pipeline, the shell reports the PIPELINE's status, not the probe's — and that
renders as **rc 0**, which in this probe's vocabulary means **VERIFIED DEAD**:
the one code that PERMITS reclaim.

    # WRONG — argparse error, and the rc read through a pipe is 0
    python3 ci-hub/health/agent_liveness_probe.py --agent hermit-907 2>&1 | head -8
    #   agent_liveness_probe.py: error: unrecognized arguments: --agent
    #   observed rc through the pipe: 0     <-- reads as VERIFIED DEAD

    # CORRECT — positional, status captured directly
    out=$(python3 ci-hub/health/agent_liveness_probe.py --json hermit-907 2>&1); rc=$?
    #   rc=1  state=alive  may_replace=false
    #   "2 live process(es) carry DG_AGENT_NAME=hermit-907: pid 2917788,2919090"

**Same agent, same moment. The mistyped invocation says reclaim is permitted;
the correct one says the owner is alive and reclaim is refused.**

## WHY THIS IS THE DANGEROUS SHAPE

***** **THE FAILURE MODE IS NOT NO ANSWER, IT IS THE MOST PERMISSIVE ANSWER.** *****

**A fail-closed system whose probe fails open on a mistyped flag is not
fail-closed.** The trigger is trivial — one wrong flag — and the wrong result is
indistinguishable from a real verdict unless the reader already knows the
vocabulary. rc 0 is not a neutral "error"; it is an affirmative clearance.

## THE RULE

**Capture the probe's exit status DIRECTLY, never through a pipeline.** Assign
the output to a variable and read `$?` from the probe itself, as in the correct
form above. Never `probe ... | head`, `| tee`, `| jq`, or any pipe, when the exit
status is what the decision turns on.

Corollary for reviewers: **an rc 0 from this probe should be accompanied by the
probe's own JSON showing `state` and `may_replace`.** A bare rc 0 with no payload
is as likely to be a usage error as a verdict.

## SCOPE — MEASURED, AND NARROW

**Found on ONE probe. Whether other fail-closed checks in `ci-hub/` share the
shape is UNSURVEYED and is NOT claimed here.**

What was measured: a no-truncation grep for `agent_liveness_probe` across the
repository. **Every hit is documentation, a comment, or the probe's own test —
there is no production caller that invokes it programmatically at all**, so this
defect is currently reachable only by a human or agent typing the command. That
narrows today's exposure and does not fix the probe.

Two adjacent files were checked and neither reproduces the shape:
`scripts/slot-owner-authority.py` deliberately MATCHES the probe's exit codes but
does not invoke it; `scripts/release-worktree.rs`, the reclaim authority, reads
subprocess status via `.output()` and `.code()` rather than through a shell pipe.

**Not surveyed:** every other health check under `ci-hub/health/`, and whether
any of them assigns a permissive meaning to rc 0. That is a separate pass.

## THE SLOT VERDICT THIS CAME FROM

`codex-verifyallow907` crossed both reclaim thresholds — idle past the time bound
and 5.0 GiB — and is **NOT RECLAIMABLE.**

- **Recovery SHA: documented.** HEAD `acf891dc4a8b37ae241af35331ebdc9cbf4664ec`,
  branch `fix/dbt-exit-status-parity-matrix`, contained in
  `origin/fix/dbt-exit-status-parity-matrix`. 3 ahead / 21 behind `origin/main`.
- **Working tree: CLEAN.** Zero staged, unstaged, or untracked.
- **Owner: ALIVE, rc 1.** `hermit-907`, two live pids, `may_replace: false`,
  `process_view: full`, `blind_spot: 0`.

**A live owner refuses regardless of idle time and regardless of size.** Both
thresholds crossing made it a candidate; the owner check decided it.

`ACTIVE.md:32` records the slot as `release-requested`. **A release having been
requested is not a release having been authorised**, and it does not override a
live-owner refusal. Resolution lies with `hermit-907`, which is still running.

Recorded because **a refusal with a reason is worth more than a slot left
unexamined** — the next detector tick will promote it again, and this is the
answer to why it must not be taken.

## WHAT THE DETECTOR DID CORRECTLY THE SAME TICK

`slot07` crossed 24.1h and was **not** promoted, because at 0.01 GiB it fails the
size floor. Two conditions, both required — the detector declining to manufacture
a candidate from one half of its predicate.

<!-- PROVENANCE: promoted 2026-08-15 by hermit-coord. Finished artifact, not live.
     probe source  ci-hub/health/agent_liveness_probe.py  sha256 2bf2d9190da1e28978619b37eb456134fb6d9666f2fd1ef9befa48ae618f3528
     authority     scripts/release-worktree.rs             sha256 2f4d3806efb57e13fd413248d629f92379c8ae3029f53869972aa24eca69c0f2
     registry      worktrees/ACTIVE.md                     sha256 a07886f1e4c0ed1b84ce1a5abd0b921ae23383068eb50679267772c733d2e569
     If a hash no longer matches, re-verify before relying on the invocation shown. -->
