---
name: worktree-provisioning
description: "Provision, register, adopt, use, release, and reclaim dev-hermit worktree slots through one replaceable tool adapter."
---

# Worktree provisioning and release

Use this skill whenever an agent needs an isolated product checkout or a
coordinator needs to hand one back or physically remove one. A **slot** is one
directory at `worktrees/<slot>/` containing up to three product Git worktrees:
`hermit/`, `reverie/`, and `liteinst2/`.

This file is the single source of truth for the complete slot lifecycle:
provision, register, adopt, work, release, and reclaim. The policy is independent
of the implementation. Every current command and implementation-specific state
name is confined to [Current tool adapter](#current-tool-adapter). When the owner
imports replacement tooling, replace that section without rewriting the policy.

The lifecycle is:

1. provision and register one isolated slot;
2. adopt the recorded ownership from the live owner;
3. work only in the assigned product checkout;
4. record a recoverable handoff while the owner is alive;
5. release ownership; and
6. reclaim physical storage only after independent absence and durability
   proofs pass.

## Durable records

The lifecycle writer updates these parent-repository artifacts:

- [`worktree-state.json`](../../../worktree-state.json) is the machine-readable
  ownership record. It names each registered slot's owner, task, product paths,
  branches, status, process-generation evidence, and interrupted operation.
- [`worktrees/ACTIVE.md`](../../../worktrees/ACTIVE.md) is the managed
  human-readable projection of that record. Presence in this file is not proof
  that a process is alive; absence is not permission to use a directory.
- [`worktrees/ARCHIVED.md`](../../../worktrees/ARCHIVED.md) is the durable
  handoff history. One completed handoff entry names every initialized product
  checkout, its exact 40-hex commit, its remote-durability proof, and whether
  the physical slot was retained or removed.

An **owner lease** is evidence for one exact owner-process generation. Depending
on the agent substrate, that may bind a pane and cgroup or a separately
revocable coordinator process. It is not a timeout and does not claim that the
task is healthy.

## Policy that survives a tool replacement

### Provision and register

The coordinator chooses one unused slot token, one mutating owner, one task ID,
one purpose sentence, and the product checkouts the task needs. Never rename a
slot, move its directory, or reuse a previously assigned slot as a shortcut.

Base every product checkout that will change on its freshly fetched
`origin/main`. A local primary checkout can be stale even when its branch is
named `main`; a tool default is not evidence of freshness.

Record the complete provision request in the task note: owner, slot, task,
purpose, selected products, fresh base, task branches, and owner-proof mode.
The adoption request must repeat those values rather than reconstructing them
from memory.

After provision, verify all of these observed facts before the first edit:

- the one requested slot exists at `worktrees/<slot-token>/`;
- [`worktree-state.json`](../../../worktree-state.json) names the expected owner,
  task, product paths, branches, and active status;
- [`worktrees/ACTIVE.md`](../../../worktrees/ACTIVE.md) shows the same managed
  fields;
- every requested product checkout is clean; and
- every branch or detached commit equals the requested fresh base.

If any fact disagrees, stop. Do not repair it with reset, checkout, stash,
manual registry edits, or another slot.

### Adopt after the owner starts

Provision may happen before the agent process exists. The slot is then
registered but its process-generation evidence is unbound. That is a normal
intermediate state, not evidence that the agent is fake and not permission to
recycle the slot.

Once the owner is live, repeat the exact provision request from that owner's
real execution context. This is **adoption**: the lifecycle writer verifies the
same slot and records the exact live owner generation. Adoption does not create
a second slot.

Never attempt release-then-reallocate as an adoption remedy. That sequence can
collide with the still-recorded owner and has already produced incompatible
ownership histories. If adoption refuses, preserve the slot and report the
exact refusal.

### Work inside the assigned slot

The mutating agent writes only inside its allocated product checkout and only
on its registered task branch. One slot has exactly one mutating owner.

A slot is not bound to one branch for its lifetime. The ownership rule prevents
concurrent writers; it does not require the first branch to land before the same
owner can do more work. After the current task commit is clean, pushed to its
authorized remote branch, and recorded in the task, the owner may reuse the
same product checkout for another sequential branch. Fetch `origin/main`, create
an unrelated branch from the resulting current `origin/main` commit, and repeat
the allocation/adoption request with the same agent and slot but the new task,
purpose, and branch so the registry describes the checkout again. Do not branch
from the previous PR's branch unless the work is deliberately stacked and its
inherited landing risk is stated: an accidental stack makes the new work depend
on an unlanded PR. If an earlier PR later needs a rebase, first make the current
branch clean and remotely recoverable, then switch back and update the
registration to match; that branch switch is ordinary reuse, not a reason to
hold later work until the earlier PR lands.

An additional observer may join only through the current tool adapter's
explicit read-only operation. The observer must not edit files, move branches,
run a build that writes into the mutating owner's build directory, or share any
writable build directory. Read-only observation is not joint ownership.

Do not review an agent's live slot working tree. Mutation testing deliberately
plants regressions in that tree, runs the test that should reject each one, and
then restores the file; an observer can therefore catch a planted regression or
a transition between mutants rather than the change under review. Bind review
findings to an immutable published commit: inspect `git show <40-hex-sha>` or
check out that exact commit in the observer's own clean, read-only clone. For
work that is not yet published, read the agent's task notes and wait for a
pinned commit; the mutable slot is progress evidence, not review evidence.

Never "fix" surprising content in another agent's slot. It may be a live
mutation experiment, and changing or restoring it can invalidate the
experiment just as surely as mistaking the planted regression for authored
work. Report the observed path and commit context to the owner without writing
the tree.

Treat every unexpected file, branch, commit, process, or registry value as
somebody else's work. Do not reset, clean, overwrite, stash, absorb, switch over,
or copy it aside. This rule preserved a Reverie commit that was reachable from
no origin branch inside a directory a detector had called reclaimable.

All worktrees of one product repository share `refs/stash`. Never create,
apply, pop, drop, or recover a stash. Commit coherent task-owned work on the
registered feature branch and record its exact commit instead.

Never rename a slot or reuse its directory outside the registered sequential
reuse and released-slot allocation paths described here. Those paths update the
slot record before more work begins; they do not permit a second mutating owner.
Never share a writable build directory between slots or agents.

### Release while the owner is alive: create the continuation first

**An agent cannot prove its own absence and therefore cannot complete physical
reclaim while it is still the recorded owner.** The owner-alive phase is for
making release recoverable, not for forcing deletion.

Before the owner exits, it places one durable task note containing:

- the slot token and owning agent;
- the task ID and purpose;
- for every initialized product checkout, its path, branch or detached state,
  exact 40-hex `HEAD`, and `git status --short --branch` result;
- for every task commit, the origin branch or other remote ref containing that
  exact commit;
- validation results and every known limitation;
- whether a release attempt changed the slot into an unfinished state in
  [`worktree-state.json`](../../../worktree-state.json); and
- the exact continuation from [Current tool adapter](#current-tool-adapter)
  that the coordinator should run after independently proving owner absence.

A **recovery SHA** is the exact 40-hex commit from which all intended task work
can be recovered. Never remove a dirty slot without a documented recovery SHA.
If intended changes are not committed, the slot remains active and dirty; there
is no honest recovery SHA and release must not begin. Intended task commits must
be present on an authorized remote ref before physical reclaim.

Do not invoke release merely to probe availability. A release command is a
mutation attempt. Never use a force or discard option in the normal lifecycle.

### Release and reclaim after the owner exits

The coordinator verifies, from a source independent of the detector that
suggested cleanup, that the recorded owner has exited and that no process uses
the slot by current working directory, executable, open file descriptor, memory
map, container reference, or recorded cgroup. Detached, clean, main-reachable
Git state proves none of those process facts.

Then the coordinator runs the exact continuation recorded while the owner was
alive. The lifecycle writer may refuse because ownership, cleanliness, remote
durability, process absence, path identity, submodule administration, or
interrupted-operation recovery is not proven. A refusal is an authoritative
answer: preserve the registered slot and report the exact reason.

Logical **release** hands ownership back while retaining product directories as
cache. Physical **reclaim** proves the removal boundary, removes the registered
product worktrees and now-empty slot directory, and removes that slot's machine
record. Never call retained cache reclaimed storage.

Never infer remote durability from a local branch name. The exact task commit
must be reachable from an authorized remote ref before reclaim.

### Recover a missed or interrupted handoff

If the owner exited without the required note, or a cleanup attempt left an
unfinished operation, the slot is still owned infrastructure. It is not free,
not reusable, and not repairable by raw Git commands.

The coordinator reconstructs the missing handoff read-only:

1. read the exact slot row in
   [`worktree-state.json`](../../../worktree-state.json) and its managed row in
   [`worktrees/ACTIVE.md`](../../../worktrees/ACTIVE.md);
2. inspect every initialized product checkout for dirt and record every exact
   40-hex `HEAD`;
3. prove each intended commit recoverable from an authorized origin ref,
   publishing only when task and branch ownership permit it;
4. independently prove the historical owner absent and the slot unused; and
5. run the current adapter's exact interrupted-operation recovery.

If process-generation evidence was never bound, only the coordinator may use
the current adapter's binding-only recovery. That operation preserves the
historical owner record and creates separately revocable coordinator evidence;
it never claims that the coordinator was the historical owner.

### Detector output is never permission

A disk-residue or idle-slot detector produces a **candidate set**: slot names
worth inspecting. It does not resolve ownership, process use, dirt, unpublished
commits, or interrupted operations. In one measured sweep the detector proposed
14 slot candidates and the registered lifecycle writer refused all 14 candidate
slots; every refusal preserved real uncertainty or work.

Never delete, reset, release, reclaim, kill, or reassign from a detector result.
Feed one candidate slot at a time to the registered lifecycle, and accept the
lifecycle writer's refusal as the authoritative outcome.

### Complete the record

After successful release or reclaim, append one entry to
[`worktrees/ARCHIVED.md`](../../../worktrees/ARCHIVED.md) naming the slot, owner,
task, every initialized product checkout and exact 40-hex commit, remote
durability, validation, operation used, and whether physical storage was
retained or removed.

Re-read [`worktree-state.json`](../../../worktree-state.json) and
[`worktrees/ACTIVE.md`](../../../worktrees/ACTIVE.md) to confirm the requested
one-slot transition occurred and every unrelated slot row remained unchanged.

## Current tool adapter

> **Replacement boundary.** This is the only section that names worktree
> management executables, flags, or exact commands. Replace this complete
> section when the owner imports new tooling; do not distribute new invocations
> through the policy above.

The only current lifecycle writers are the tracked
[`scripts/allocate-worktree.rs`](../../../scripts/allocate-worktree.rs) and
[`scripts/release-worktree.rs`](../../../scripts/release-worktree.rs). Never
substitute raw `git worktree add`, `git worktree remove`, `git worktree prune`,
directory deletion, registry edits, or an ad hoc checkout.

### Fresh-base provision and adoption

Fetch each selected product first; the current allocator deliberately does not
fetch and otherwise defaults to each primary checkout's `HEAD`.

```bash
with-proxy git -C hermit fetch origin main
with-proxy git -C reverie fetch origin main
with-proxy git -C liteinst2 fetch origin main

./scripts/allocate-worktree.rs \
  --agent <agent-name> \
  --slot <unused-slot-token> \
  --task <task-id> \
  --product <hermit|reverie|liteinst2|both|all> \
  --start-point origin/main \
  --hermit-branch <descriptive-hermit-branch> \
  --reverie-branch <descriptive-reverie-branch> \
  --liteinst2-branch <descriptive-liteinst2-branch> \
  --purpose "<one observable outcome>"
```

Run fetch only for selected products. Pass branch flags only for product
checkouts the task will modify; omit a product's branch flag to leave that
checkout detached. For a Codex-owned slot whose coordinator selects the current
sentinel path, add `--codex-systemd-sentinel`.

Adoption repeats the exact allocation command, including any sentinel flag,
from the live owner's execution context. The
[current sentinel design](../../../ai_docs/slot-reclamation-sentinel-design-20260808.md)
defines its exact process-generation and revocation proofs.

After a published PR, reuse its slot only after the old branch is clean and its
exact task commit is reachable from the authorized remote branch. Record that
commit and remote ref in the old task, then start unrelated work from the
freshly fetched main branch and move the registration with the checkout:

```bash
git -C worktrees/<slot>/hermit status --short --branch
git -C worktrees/<slot>/hermit rev-parse HEAD
git -C worktrees/<slot>/hermit branch -r --contains HEAD
with-proxy git -C hermit fetch origin main
git -C worktrees/<slot>/hermit switch -c <new-branch> origin/main
./scripts/allocate-worktree.rs \
  --agent <same-agent> \
  --slot <same-slot> \
  --task <new-task> \
  --product hermit \
  --start-point origin/main \
  --hermit-branch <new-branch> \
  --purpose "<new observable outcome>"
```

Use the corresponding product and branch flag for Reverie or LiteInst2. Repeat
the post-allocation checks from [Provision and register](#provision-and-register)
before editing. If the new work intentionally depends on the unlanded PR, state
that dependency instead of using `origin/main`; otherwise using the old branch
as the start point silently stacks the new PR.

The current explicit read-only join is the same allocation request with:

```bash
--i-promise-this-agent-is-read-mostly
```

### Release, reclaim, and interrupted-operation continuation

Logical release, where supported without a coordinator sentinel, retains the
physical product worktrees:

```bash
./scripts/release-worktree.rs --slot <slot-token>
```

Ordinary physical reclaim after the owner exits uses:

```bash
./scripts/release-worktree.rs --slot <slot-token> --clean
```

If a clean task branch is not yet on origin, the current releaser can publish
it and repeat remote-durability proof:

```bash
./scripts/release-worktree.rs --slot <slot-token> --clean --push
```

If the machine record contains `owner-lease-revoked`, `releasing`,
`coordinator_lease_revocation`, or `release_journal`, continue only after the
owner exits and the policy proofs above pass:

```bash
./scripts/release-worktree.rs \
  --slot <slot-token> \
  --clean \
  --recover-submodule-cleanup
```

Do not use `--force` in the normal lifecycle.

### Binding-only recovery for a legacy unbound owner

Only the coordinator may run this after the policy's historical-owner absence,
slot non-use, checkout-state, and recovery-SHA proofs pass:

```bash
./scripts/allocate-worktree.rs \
  --agent <historical-owner> \
  --slot <slot-token> \
  --codex-systemd-sentinel \
  --recover-legacy-unbound-owner \
  --recovery-note "<absence proof and recovery evidence>"
```

The releaser then performs exact sentinel revocation and physical reclaim
through the ordinary or interrupted-operation command above.

## Commands that look correct and are not

**None of these are bugs in what the tools DO. They are bugs in what the tools
SAY.** Every one behaves correctly and describes itself misleadingly, so the
failure text points AWAY from the fix and sends the reader to diagnose something
that is not broken. All five were hit live on 2026-08-11, each by a different
agent, and each cost real time.

Entries are written SYMPTOM FIRST, because an agent searches for what it is
seeing, not for what it should have done.

### Symptom: a successful publish looks like it never happened

`make check-agent-utils-pin` reports the OLD submodule sha right after a publish
that printed `AGENT_UTILS_MAIN_WRITE published=<new-sha> ancestry=1/1`.

**Cause:** `scripts/parent-main-write sync` RESETS submodules to the parent's
RECORDED gitlink. Fast-forwarding a submodule checkout and *then* syncing
silently undoes the fast-forward.

**This is the dangerous one.** Nothing failed, but the pin check invites you to
conclude the publish did not land, so the reflex is to re-publish or to declare
the fork-and-pin route broken. Both waste an evening on working machinery.

**Correct order for a pin bump:**

```bash
./scripts/parent-main-write sync                 # FIRST, not last
git -C <submodule> merge --ff-only origin/main   # then advance the checkout
git add <submodule>                              # then stage the gitlink
./scripts/parent-main-write commit -m "..." -- <submodule>
```

### Symptom: `fatal: ref updates aborted by hook` on a parent pull

```
parent-main-write: REFUSED: missing serialized-writer lock receipt
fatal: ref updates aborted by hook
```

after a plain `git merge --ff-only origin/main` or `git pull` in the parent.

**Cause:** the parent serializes its own ref movement. THE PARENT IS NOT WEDGED
and nothing needs repairing.

**Correct command:** `./scripts/parent-main-write sync`

### Symptom: `ABORTED at line 1380 with status 1, and no reason was printed`

From `parent-main-write commit`. The tool honestly labels this a defect in
itself and confirms nothing was published or modified, so it fails safe — but
it does not tell you what to change.

**Cause:** the named paths are not STAGED. An untracked new file or an unstaged
submodule gitlink both produce it.

**Correct command:** `git add <path>` first, then re-run the same commit.

### Symptom: the worktree is gone after a "recovery" command

**Cause:** `./scripts/release-worktree.rs --clean --recover-submodule-cleanup`
DELETES the worktree. The flag name says recover; the operation reclaims.

Read what it does before pointing it at state you care about. It appears above
in the normal lifecycle for exactly that purpose — reclaim — and that is the
correct use; the trap is reaching for it *because* the name sounds safe when a
submodule cleanup was interrupted.

### Symptom: a landing failure that reads as a broken lander

`land-pr.sh` fails in a way that presents as landing machinery being broken.

**Cause:** a wrong identity flag in the invocation. The input was wrong, not the
lander.

**Check the identity arguments before diagnosing the landing path.**
