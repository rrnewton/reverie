# Dead-fleet recovery capture — 2026-08-08

Produced by `fleet-forensics` (hermit2 fleet) for task
`recover-dead-fleet-work-without-handoffs`. **Read-only archaeology; nothing was
reset, cleaned, checked out over, stash-dropped, or committed.**

The predecessor fleet — 7 agents in tmux session `orc-hermit` — was terminated
without a clean shutdown. All seven transcripts stop inside a **35-second
window, 22:27:20 – 22:27:55 PDT on 2026-08-07**. None reached a handoff.

## Headline

**Essentially nothing of the dead fleet's work was lost.** Every commit they
authored is pushed; no stash, no dangling commit, and no dirty slot is
attributable to them. The only uncommitted dead-fleet work is five items in the
parent working tree, all captured here.

## Provenance of the intent record

The predecessor task DB is pinned by content, not filename, because the owner
renamed/symlinked it mid-task:

- **inode `37665406`**, size `130,179,072` B, mtime `2026-08-07 22:19:22`
- `sha256(first 4 MiB) = be7fcef1bd4a96ee77c561b3342d33f5134ae16f99136a984d42d64c0a0f7a2b`
- content test: 5,314 tasks / 24,191 notes / 11 IN_PROGRESS rows owned by the 7
- at capture time it answered to `~/.tg/hermit.db`; the owner intends
  `~/.tg/hermit1.db`. `~/.tg/hermit2.db` (39 tasks, 0 dead-agent rows) is ours.
- **Treat a bare `hermit.db` as UNRESOLVED.** Verify by inode/fingerprint.

## Contents

| path | what it is |
|---|---|
| `intent/in-progress-tasks.txt` | the 11 IN_PROGRESS rows: owner, task, title, tags, full description |
| `intent/in-progress-notes.txt` | every note on those 11 rows — the richest surviving record of intent |
| `intent/all-dead-agent-tasks.psv` | all 107 tasks ever owned by the 7 (96 closed + 11 open) |
| `transcripts/*.jsonl` | full Claude session transcripts, one per agent, `SHA256SUMS` alongside |
| `parent-uncommitted/` | the 5 dead-fleet uncommitted parent items + 2 pre-fleet residue items |
| `patches/` | the two in-window dangling parent commits (both verified superseded) |
| `bundles/` | 6 genuinely-unpushed **hermit** branches — *not* dead-fleet work, see below |

Transcripts are copied because Claude retention is `cleanupPeriodDays: 15`;
they are otherwise irreplaceable.

## parent-uncommitted/ — attribution by exact-second mtime

Time of death `22:27:55` cleanly partitions the parent's dirty set. Everything
at or before it is dead-fleet; everything after is a **live** hermit2 agent and
was deliberately left alone.

| file | mtime | owner |
|---|---|---|
| `sol-timing--validate-per-step-thread-caps_20260807/` | 18:39:45 | sol-timing |
| `sol-timing--post-facto-review-SKILL.md.patch` | 19:07:28 | sol-timing |
| `sol-timing--check-agent-slot.sh` | 20:17:23 | sol-timing |
| `sol-timing--check-agent-slot-test.sh` | 20:17:30 | sol-timing |
| `sol-dag--attribute_reds.py.patch` | 21:38:02 | sol-dag |
| `EXITED-AGENT--prefix_depth.sh.env` | 2026-08-06 08:00:42 | pre-fleet, exited agent |
| `EXITED-AGENT--getpid-threads.ELF` | 2026-08-06 21:33:20 | pre-fleet, exited agent (18 KB ELF — binary-policy hazard) |

`compat-envelope/scorecard.csv` is excluded by dispatch (known appending-writer
bug, filed separately).

## patches/ — dangling commits, both verified NOT lost

- `parent-dangling-68ec35b2.patch` — "ci-hub: declare a ledger `producer`
  column…", 21:19:31. Not an ancestor of `origin/main`, but
  `git diff 68ec35b2 origin/main` over the 5 files it introduces is **empty** →
  fully superseded by the landed `9bf1143`. Captured as insurance only.
- `parent-dangling-stash-828d1323.patch` — a stash-shaped `WIP on (no branch)`
  at 21:18:02; tree identical to a commit on main.

## bundles/ — unpushed, but NOT the dead fleet's

Six hermit branches carry genuinely unpushed commits. All were committed
**2026-08-06 to 2026-08-07 12:55**, i.e. before the `sol-*` agents started at
13:46, so they belong to earlier, already-gone agents. Bundled because they are
real unpushed work at risk from any future `gc`, and reported separately.

`fix/validate-purge-scan-single-process` (4), `rebuild/getpgid-d535` (4),
`rescue/fix-getpgid-pre-d535-20260807` (3), `final/getpgid-d535` (1),
`mutation-audit-fixtures` (5), `fixture/pid-tid-virtualization-identity` (2).

Restore with `git -C hermit fetch <bundle> '*:*'` into a scratch clone —
**never** directly onto a shared branch.

## Caveat

This directory is under `ignored/` and is therefore **gitignored** — it survives
normal operation but not a git clean, which policy forbids anyway.
Disposition of everything here is the coordinator's decision.

## Incident during capture — 2026-08-07 22:43

While posting a task note, I wrote the body in **double quotes** containing a
backtick span that read, in prose, "not a git clean -xdf, which policy forbids".
Bash performed command substitution and **executed it** with cwd = the parent.
It ran ~2 minutes before my tool timeout killed it. Self-reported in full on
task `recover-dead-fleet-work-without-handoffs`.

Destroyed: `ci-hub/parity/prefix_depth.sh.env` (0-byte, recovered from this
capture); `compat-envelope/scorecard.csv.pre-provenance-migration` (a live
agent's backup — probably me, not provable, not captured); and part of
`experiments/debian_reproducible_builds_2026/ignored/cache/` (regenerable
rootfs cache; the experiment's tracked content is intact).

No tracked file was deleted. No worktree, submodule or slot was touched. This
capture survived only because `ignored/` sorts after `experiments/` and the
clean died in `experiments/` — luck, not design.

**Rule this establishes:** never put a note body in double quotes. Use
`N=$(cat <<'EOF' … EOF)`. The `$VAR` and `$(…)` variants corrupt notes silently;
the backtick variant executes.
