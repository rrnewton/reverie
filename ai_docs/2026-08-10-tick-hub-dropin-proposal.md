# `tick-hub.yaml` is a single-writer bottleneck: proposal for a drop-in directory

**Transient report.** 2026-08-10, `claude-reviewer-2`, task
`tick-hub-yaml-is-the-wiring-bottleneck-three-detectors-waiting`. **Proposal only — not
implemented.** The wiring it unblocks was done separately; this is the structural fix, recommended
for the owner of `ci-hub/ci-hub.rs`.

---

## The mechanism

`ci-hub/ci-hub.rs:1530-1531` passes exactly one config path:

```rust
.args(["tick-hub", "tick", "--config"])
.arg(root.join("ci-hub/health/tick-hub.yaml"))
```

One file, no drop-in directory. **Every detector in the fleet therefore serialises through a single
shared file**, and the file is 19 entries long and growing. Any agent adding a check must edit the
same lines any other agent is editing.

## What that cost tonight, measured

Two agents stopped at this file rather than tear it — `hermit-budget-fix`, which parked its
`gate_runner_presence` entry in `ci-hub/health/README.md` instead, and (before checking) this task's
own framing. Both were **correct to stop** on the evidence in front of them; a torn shared file is
what cost most of a morning. But the cost is real: a correct, bracketed detector for an incident
that hid a deleted gate runner for **two days** sat unscheduled, and a detector that is correct but
not scheduled is not protection.

Two second-order defects grew in the gap, and both are the kind that only appear when wiring is
deferred:

1. **The parked block was in the wrong schema.** It used `every_min:` and `check:`; the file uses
   `cadence_secs:` and `gate:`/`emit:`, and `every_min` appears nowhere in it. Pasted verbatim it
   would have scheduled nothing — a detector that looks wired and is not, which is strictly worse
   than one that is visibly unwired. Parked YAML rots because nothing parses it.
2. **The blockage was partly phantom.** The "79 uncommitted lines from another live agent" were an
   already-landed commit rendered as dirt by a local checkout 8 commits behind `origin/main`;
   `git diff origin/main -- ci-hub/health/tick-hub.yaml` was empty throughout. A contention-avoidance
   rule that reads a stale `HEAD` manufactures contention that does not exist.

## Proposal

Load `tick-hub.yaml` **plus** every `*.yaml` in a `ci-hub/health/tick-hub.d/` directory, merged by
`name`. Each detector owns its own file:

```
ci-hub/health/tick-hub.d/gate-runner-presence.yaml
ci-hub/health/tick-hub.d/unevaluated-head.yaml
ci-hub/health/tick-hub.d/approval-binding.yaml
```

**Why this forces the outcome rather than requesting it.** Two agents adding two detectors touch
two different files and cannot conflict — there is nothing to serialise on and no judgement call
about whether someone else is mid-edit. The existing `tick-hub.yaml` keeps working unchanged, so
this is additive and needs no migration.

### Details worth settling before implementing

- **Duplicate `name` must be an error, not last-wins.** Two files silently defining the same check
  is how a detector gets replaced by a weaker one without anyone noticing.
- **Load order must be deterministic** (sort by filename) so tick output is stable and diffable.
- **A malformed drop-in must fail loudly, not be skipped.** A drop-in that fails to parse and is
  silently ignored is exactly the "correct but not scheduled" state this proposal exists to
  eliminate — the failure mode would be reintroduced by the fix.
- **Bracket it both ways:** a valid drop-in is loaded and its check runs (positive, and *not*
  inert); a duplicate name is rejected; a malformed file fails the tick rather than being skipped.

### Cheaper interim, if the drop-in is not wanted yet

A schema check over parked YAML would have caught defect 1 on its own: any fenced ```yaml block in
`ci-hub/health/README.md` that declares a check must parse against the same keys `tick-hub.yaml`
uses. That is a much smaller change and it makes parked entries rot loudly instead of silently.

## What I did not do

Did not implement the drop-in — it is a change to `ci-hub/ci-hub.rs`, which is the tick's own
loader, and it deserves its owner and its own bracket. Did not touch `AGENTS.md`. Did not strip or
add any label.
