# The liveness probe fails OPEN on a mistyped flag — advisory layer only

A latent safety defect in the ADVISORY layer of the slot-reclaim path. The
enforcement is independent and unaffected; see the corrected severity below. Found found 2026-08-15 by
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

## WHY IT MATTERS — AND THE SEVERITY, CORRECTED

***** **THE FAILURE MODE IS NOT NO ANSWER, IT IS THE MOST PERMISSIVE ANSWER.** *****

The trigger is trivial — one wrong flag — and the wrong result is
indistinguishable from a real verdict unless the reader already knows the
vocabulary. **rc 0 is not a neutral "error"; it is an affirmative clearance.**

**SEVERITY CORRECTED, AND THE ORIGINAL FRAMING WAS WRONG IN THE WAY THAT MATTERS.**
This was first recorded — and relayed to the owner — as *"a fail-closed system
whose probe fails open"*. **That overstates it: the probe is not the enforcement.**

- **THE PROBE IS ADVISORY.** It is what an agent or coordinator runs to decide
  whether to *attempt* a reclaim. A typo hands that reader the most permissive
  answer, so **the defect is real and unchanged at the human/agent layer.**
- **THE ENFORCEMENT IS INDEPENDENT AND DOES NOT CONSULT THE PROBE.**
  `scripts/release-worktree.rs` calls `verify_release_authority_absent` from
  `live_process_users` before anything else, which walks `/proc` directly via
  `verify_recorded_owners_absent(root, proc_root, cgroup_root, &authorities.orc)`.
  `live_process_users` then refuses **every readable same-UID reference into the
  target**, tolerating only a named watcher whose exact executable identity and
  descriptor flags prove the reference non-writable — *"every tolerance is
  printed; missing or changing evidence remains a refusal."* A separate Codex
  sentinel path requires durable revocation and re-proves `unit_id`,
  `recorded_main_pid`, `recorded_main_pid_starttime` and `cgroup_path`.
- **SO A MISTYPED PROBE DOES NOT DEFEAT RECLAIM ENFORCEMENT.** The authority is
  also STRICTER than the probe: the probe asks *is this agent alive*, the
  authority asks *is anything at all holding this path*, plus cgroup and
  container checks.

**BOTH FACTS STAND: the defect is real at the advisory layer, and it does not
reach the enforcement layer.**

## THE PROSE AND THE CODE DISAGREE, WHICH IS THE SECOND FINDING

`AGENTS.md:81` names `agent_liveness_probe.py` as the authority for exactly this
decision and spells out its rc semantics. **The reclaim path never calls it, and
no production caller does.** A reader of the policy would reasonably conclude the
probe is what stops a bad release. It is not.

***** **RULE, STATED IN ITS OWN RIGHT: BEFORE RELYING ON A NAMED AUTHORITY, GREP
FOR A CALLER.** ***** Third instance in one session — the approval blob rule named
in policy and in a skill file with no implementation; the `virtualize_time`
rationale; and now this. **Three is a pattern and it should not have to be
rediscovered a fourth time.**

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

**Not surveyed, named explicitly rather than omitted:**

1. **Every other health check under `ci-hub/health/`**, and whether any assigns a
   permissive meaning to rc 0. A separate pass.
2. **Whether `verify_recorded_owners_absent` uses the same `DG_AGENT_NAME` signal
   the probe does, or a different owner record.** Its call site and signature
   were read; **its body was not.** So "the enforcement is independent" is
   established structurally — it does not invoke the probe — but whether the two
   agree on what an owner IS remains open. If they disagree, the advisory and the
   enforcing layers could reach different verdicts about the same slot.
3. **Whether any OTHER authority named in `AGENTS.md` lacks a caller.** Unsurveyed,
   and cheap to check as a set rather than one at a time — which is the natural
   next step given the rule above.

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
