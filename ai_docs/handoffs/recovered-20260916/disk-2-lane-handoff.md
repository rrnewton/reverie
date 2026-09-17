# disk-2 handoff — slot reclaim, and why it is blocked

Written at context exhaustion 2026-09-16. I have no registered slot, so the
unread-handoff protection cannot preserve this; it is committed here instead,
beside the 77 I recovered, because that is where a stranger already looks.

Counts are in the tick and in the task notes and you can re-measure them. What
follows is the part you cannot re-derive.

## The one thing to know

**Removal is blocked, not slow.** Every instinct I had about throughput — batch
sizes, parallelism, lock backoff — was aimed at the wrong constraint and wasted
maybe an hour. Most removal attempts REFUSE. Fix refusals; do not tune rates.

I also measured the treadmill question and got it wrong twice before getting it
right. Over 30 minutes removal beat creation 4:1 and I reported that. Over 12
hours creation won, net −5. The honest statement: **removal wins only while an
agent is actively grinding at it**, which means the reclaim is not self-sustaining
and the creation side matters more than my cheerful numbers suggested. Do not
quote a short window.

## Why drift, not owner evidence

Of 165 slots with a proven-dead owner and no live process, 135 are blocked by
drift and 15 by degenerate owner evidence. I had the priority inverted until I
measured it, and so did the graph.

The reason it is drift is structural rather than incidental. The recorded branch
and HEAD are captured at slot CREATION. They stop matching because work continued
after the agent died — which is simply what a branch does. So drift is the normal
end state of every slot that was ever used, and the guard therefore fires on
almost the whole population rather than on an anomaly. Degenerate owner evidence,
by contrast, is a write-time accident affecting a minority.

**I believe the four drift refusals are one repair.** "HEAD moved" (76), "branch
moved" (25), "detached" (10) and "not a registered worktree" (24) are four
readings of one question: does the checkout still look the way it did at
creation? They live in the same precondition block and all compare recorded
metadata against live metadata. I did not prove they share a code path — I ran
out of context first — and that is the first thing to verify, because if they are
four separate call sites this is four repairs and my estimate is wrong.

**Why I think removing the drift check is safe**, which is the judgement that
matters most here: salvage runs before removal and captures whatever the checkout
holds. A drifted HEAD therefore costs bookkeeping accuracy, not work. Every guard
that protects something real is a SEPARATE check and none of them is this one:
live owner, live process inside the checkout, unread handoff, deliberate hold.
Those four must survive any repair. I would change the drift comparison to a
recorded observation rather than a precondition.

## What I ruled out, so you do not redo it

- **The cached agent snapshot** as the cause of the wrong live-agent count. It is
  refreshed every five minutes and was 1.5 minutes old. The real cause is word
  splitting: the live-agent list is built by `.split()` on a line of tmux WINDOW
  names, so "Ensure deterministic KVM parity | dev-hermit" becomes six agents.
  Harmless today — no junk token matches a slot owner — but six of the tokens
  (`codex`, `orc`, `bash`, `kvm`, `liteinst`, `parity`) are plausible agent names,
  and the day one matches, that slot silently leaves the breach count.
- **The wrkslots audit's NO_RESULT** as the duplicate-attention defect. It is not:
  the audit now exits 3 producing no census at all, 0.387 CPU-seconds against a
  240-second budget. My dedup fix (86bedda) repairs a census that is no longer
  being produced. The exit-3 cause is unknown and its stderr is hashed and
  discarded, which is why nobody can see it.
- **Zombies** as the orphan reaper's blind spot. 47 of 52 "unreadable" processes
  are zombies misreported as "disappeared"; all were alive, state=Z, with live
  parents. The reaper already has a rule for zombies and never reaches it.

## What I would do next, in order

1. **Verify the four drift refusals share one precondition block.** If yes, change
   drift from a precondition to a recorded observation. That reaches 135 slots and
   is the highest-leverage change available.
2. **Advance the primary's agent-utils pin** from 2781b10 to current main. The
   owner-evidence repair (70f4918) and the audit dedup (86bedda) are landed but
   NOT DEPLOYED, which is why the tick still calls 121 slots permanently
   unremovable. I proved the repair works by removing
   `allow-completed-validation-checkouts`, a machine-init-owned row, from a
   checkout at the newer pin. This is a submodule bump in the shared primary and I
   did not do it unilaterally.
3. **The 26 remaining handoff readings and 26 salvages.** Run the salvages first
   and in parallel — they are append-only and fast, and letting the slow reading
   gate them is a mistake I nearly made.

## Traps that cost me time

- `git log --not --remotes` answers 0 unpushed for every slot here, because these
  worktrees carry `refs/remotes/audit/*` mirrors. Use
  `HEAD --not --glob=refs/remotes/origin/*`. The wrong form told me 116 slots were
  safe to delete when 37 of them held commits on no origin ref. I caught it only
  because a clean zero across a whole population is the shape of a broken query.
- A census is a work-at-risk measure, not a removability measure. "Holds nothing"
  is necessary and nowhere near sufficient; I called eight slots removable and
  five refused on conditions the census does not measure.
- Re-prove process absence immediately before each removal, not from a batch
  proof. A slot that was empty two hours earlier had become occupied.
- Do not run two removal loops at once. wrkslots serialises mutations behind one
  configuration lock and I became my own contention.
- The registry's handoff field records 8; the filesystem holds 77. Enumerate from
  disk, always.
