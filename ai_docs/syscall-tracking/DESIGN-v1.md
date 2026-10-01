# Syscall tracking: design and implementation plan (v1, for owner review)

Status: PHASE 1 design, 2026-10-01. Nothing on GitHub or in hermit was
changed. Author: `syscall-tracking-design` (Claude subagent of coord2,
devbig030).

Owner request: rename the `Syscall classification:` issues to
`Syscall tracking:`, map them one to one onto the syscall enum, give them one
enforced section schema and machine-readable fields, mirror them locally, and
build deterministic tooling plus an agent-run consistency check.

## 0. Provenance of every number below

| Input | Exact source |
|---|---|
| Issues | `rrnewton/hermit`, fetched 2026-10-01 ~16:05Z through `herdr-run` with `gh issue list --state all --search '"Syscall classification" in:title' --json number,title,state,stateReason,labels,closedAt,updatedAt,createdAt,body,comments`. 309 issues. One call took 9.0 s and returned 555 KB. |
| Syscall enum | `syscalls` 0.6.18 (`Sysno`, x86_64), re-exported by `reverie-syscalls` at reverie `dbf2b5c8880bac5fd6a06fa1f296b3eec3590569`. It has 373 entries, ids 0..=461. |
| Detcore policy | hermit `origin/main` = `18ece74f8b0f68ff57871c63320669424c750e15`. I exported `detcore/` and `detcore-model/` with `git archive` and **linked them into a rust-script**, which called Detcore's own public functions (see section 5). I did not parse source for this. |
| Scratch | `/var/tmp/syscall-tracking/` holds the raw JSON, `policy.tsv` and the prototypes. It is not durable; the census can be reproduced from the commands in this doc. |

Detcore's policy at that SHA, as reported by Detcore itself:

| `classify_syscall` | count | of which `is_deterministically_refused_syscall` | subscribed in run mode | subscribed under `passthru_opt` (record/replay) |
|---|---:|---:|---:|---:|
| Determinized | 294 | 110 (fixed errno, e.g. ENOSYS/EPERM) | 294 | 294 |
| PassThrough | 78 | 0 | 78 | 4 (`getpid gettid set_tid_address set_robust_list`) |
| Unsupported | 1 (`restart_syscall`) | 0 | 1 | 1 |
| **Total** | **373** | 110 | **373** (`Subscription::all_syscalls()`) | 299 |

The per-name test lists in `syscall_classification.rs`
(`DETERMINIZED_SYSCALLS` 294, `PASSTHROUGH_SYSCALLS` 78,
`UNSUPPORTED_SYSCALLS` 1) agree with the function on all 373 names.

"Determinized" therefore means two different things. In 184 cases Detcore
models or emulates the syscall. In the other 110 it refuses the syscall
deterministically. The schema below keeps the two apart.

## 1. Census

### 1.1 Issue counts

- **309** issues are titled `Syscall classification: <name>`: **235 open, 74 closed**.
- Every one of the 309 titles is exactly `Syscall classification: <Sysno name>`.
  None names something outside the enum, and none names two syscalls.
- Labels: 288 issues have none. The 21 issues filed on 2026-08-07
  (https://github.com/rrnewton/hermit/issues/1935 through
  https://github.com/rrnewton/hermit/issues/1955) carry
  `enhancement, orc-coord, orc-coord-014`. No `syscall-tracking` label exists
  (185 labels in the repository; the nearest are three `mechanism:*syscall*` labels).
- Creation dates: 288 issues were created 2026-07-24 and 21 were created 2026-08-07.
- Comments: 234 issues have none. 74 have one (the closing comment) and 1 has two
  (https://github.com/rrnewton/hermit/issues/404, `restart_syscall`).

### 1.2 Mapping onto the 373 enum entries

| issues per syscall | syscalls | detail |
|---:|---:|---|
| 0 | **114** | 113 are Determinized (107 modeled, 6 refused) and 1 is PassThrough (`mprotect`). These are the syscalls Detcore handled before the July campaign, which filed issues only for the old `UNCLASSIFIED (TEMPORARY PASS-THRU)` block: `read write open close stat ... futex clone3 io_uring_*`. |
| 1 | **209** | |
| 2 | **50** | All 50 pairs are open and both copies use the legacy schema. They fall in the range https://github.com/rrnewton/hermit/issues/422 to https://github.com/rrnewton/hermit/issues/531, filed by two concurrent batch-4/5 agents. Examples: `shmget` (https://github.com/rrnewton/hermit/issues/473, https://github.com/rrnewton/hermit/issues/479), `setuid` (https://github.com/rrnewton/hermit/issues/462, https://github.com/rrnewton/hermit/issues/468), `unshare` (https://github.com/rrnewton/hermit/issues/507, https://github.com/rrnewton/hermit/issues/528). |
| >2 | 0 | |

Check: 209 + 2 x 50 = 309. To reach one issue per syscall, the plan
**creates 114 issues** and **retires 50 duplicates**, leaving 373.

Related issues that cover syscalls but lack the prefix (19 found with
`Syscall in:title`) include:
- https://github.com/rrnewton/hermit/issues/24 and https://github.com/rrnewton/hermit/issues/26 (ppoll);
- https://github.com/rrnewton/hermit/issues/825 (the three `lsm_*` syscalls);
- https://github.com/rrnewton/hermit/issues/1831 (the pinned `Sysno` is 12 entries behind Linux master, so the enum and the issue set will grow);
- https://github.com/rrnewton/hermit/issues/2296 (DBT passes `restart_syscall` through).

These become `related:` links, not tracking issues.

### 1.3 What "closed" meant

- All 74 closed issues have `stateReason: COMPLETED`. One bot sweep closed
  all of them on 2026-09-28 between 15:10:26Z and 15:18:18Z
  (`[hermit2, ..., claude-opus-5-5, devbig014, role=coordinator]`).
- Every closing comment has the same shape. Example from
  https://github.com/rrnewton/hermit/issues/279:
  > Fixed by commit c994dd45 (https://github.com/rrnewton/hermit/pull/860), which gives `lsm_set_self_attr` an explicit Determinized classification. The temporary `UNCLASSIFIED (TEMPORARY PASS-THRU)` block this issue was filed against was removed by commit bf1cab33 (https://github.com/rrnewton/hermit/pull/644). Proof at hermit main 81ca8822f78c: `Sysno::lsm_set_self_attr` at detcore/src/syscall_classification.rs:514 sits in the Determinized arm ...
- So **closed meant "this syscall has an explicit arm in `classify_syscall`"**:
  54 were closed as Determinized and 20 as PassThrough. 35 of the 54
  Determinized closes are refusals, so "handled" often means "refused with a
  fixed errno". The closes cite 28 distinct PRs; the most frequent is
  https://github.com/rrnewton/hermit/pull/644 (11 times).
- Closed did **not** mean the syscall was tested, the determinism claim was
  verified, or any backend other than ptrace was checked.
- **The open/closed bit carries no information today.** Since
  https://github.com/rrnewton/hermit/pull/644 removed the catch-all block,
  every one of the 373 syscalls passes that closing test. Yet 235 issues
  remain open, 166 of them about syscalls that are already Determinized.

### 1.4 Section-heading variants

There are 4 schemas, given by the distinct heading sequences:

| n | variant | headings (in order) | example |
|---:|---|---|---|
| 200 | A1 legacy | `## Syscall: \`X\`` / `### Summary` / `### Determinism Analysis` / `### Nondeterminism Sources` / `### Linux Kernel Implementation Notes` / `### Recommended Action` / `### Testing` | https://github.com/rrnewton/hermit/issues/531 |
| 50 | A2 legacy, other order | `## Syscall: X (SYS_X, x86_64 N)`, the same five, then `### Testing` **before** `### Recommended Action` | https://github.com/rrnewton/hermit/issues/379 |
| 38 | B audit stub | `## Syscall: \`X\`` / `### Summary` / `### Determinism Analysis` / `### Recommended Action` / `### Tracking` | https://github.com/rrnewton/hermit/issues/641 |
| 21 | C release-0.3 rewrite | `## State and denominator` / `## Semantics and nondeterminism` / `## Strategy draft` / `## Required tests` / `## Acceptance` | https://github.com/rrnewton/hermit/issues/1955 |

Content drift:
- **238 of 309 bodies** still describe the syscall as "unclassified" or as on
  "temporary pass-through". That has been false since
  https://github.com/rrnewton/hermit/pull/644.
- The 21 C bodies hard-code a denominator
  (`289 DETERMINIZED, 83 PASSTHROUGH, 1 UNSUPPORTED` at `303113c1`). Today's
  counts are 294 / 78 / 1, so every one of those bodies is stale.
- Only 21 bodies (variant C) contain anything like "required tests". The
  `### Testing` sections in A1 and A2 (250 bodies) hold one-off host `strace`
  evidence from 2026-07-24. They are not a test plan.
- Body size: median 1415 bytes, maximum 2065 bytes.

### 1.5 Machine-readable lines already present

| line | count | values |
|---|---:|---|
| `**Classification:** <V>` | 288 (A1 + A2 + B; always this exact form) | CONDITIONAL 156, NONDETERMINISTIC 81, DETERMINISTIC 51 |
| `### Recommended Action` then `**<V>**` | 288 | NEEDS-HANDLER 166, PASS-THRU 91, NEEDS-INVESTIGATION 31 |
| `**Manual reference:**` | 100 | free text |
| `**Test host:**` | 50 | free text |
| `Source authority: ... at <sha>` | 21 (C) | a hermit SHA |

Variant C has no `Classification:` line; its class appears in prose
("sigaltstack is PASSTHROUGH").

"Classification" is overloaded. The issue field is a judgement about **Linux
semantics** (deterministic, conditional or nondeterministic). Detcore's
`SyscallClassification` is **Hermit's policy** (Determinized, PassThrough or
Unsupported). The two are unrelated axes, so the schema gives them different
names.

The existing fields disagree with the code today (each count is issues, not syscalls):
- 34 issues recommend PASS-THRU, but the syscall is Determinized. Example: the
  `setuid` family is now a credential no-op.
- 9 issues recommend NEEDS-HANDLER, but the syscall is PassThrough
  (`listxattr llistxattr flistxattr getpgrp getppid getsid getpgid fallocate get_robust_list`).
- 4 issues mark the syscall NONDETERMINISTIC while Detcore passes it through
  (`getpgrp getppid getsid getpgid`). The judgement is probably stale, since the
  PID namespace makes these values repeatable. That call belongs to the agent
  check (section 6.2), not to a regex.

## 2. Proposed standard schema (`syscall-tracking/v1`)

### 2.1 Required sections, exact headings, in this order

| # | heading | purpose | who writes it |
|---|---|---|---|
| 1 | `## Tracking data` | One fenced `yaml` block (2.2). The only machine-read part of the body besides Status entries. | tool (code facts) + agent (judgement keys) |
| 2 | `## Summary` | 1-3 sentences: what the syscall does and Hermit's stance today. | agent |
| 3 | `## Linux semantics` | Kernel behaviour that matters for determinism: inputs, outputs, side effects, flag multiplexing. (Was "Linux Kernel Implementation Notes".) | agent |
| 4 | `## Determinism analysis` | Sources of nondeterminism and the conditions under which the syscall repeats. Justifies `linux_determinism`. (Merges "Determinism Analysis" and "Nondeterminism Sources".) | agent |
| 5 | `## Hermit design` | Intended policy and why: model, emulate as a no-op, refuse with a fixed errno, or pass through under stated container assumptions. Justifies `target_policy`. (Was "Strategy draft" / "Recommended Action".) | agent |
| 6 | `## Recommended tests` | **The agent's coverage proposal, not status.** A checklist of items with stable ids `T1`, `T2`, .... Each item gives the behaviour to exercise and the modes/backends it should run on. Checkboxes are not used; coverage is recorded in Status. | agent |
| 7 | `## Done criteria` | What must hold to close the issue (default text in 3.3; override per syscall). | agent |
| 8 | `## Status` | **Dated, append-only log.** Format in 2.3. | tool + agents |
| 9 | `## Additional information` | Catch-all: history, migrated legacy text (for example the 2026-07-24 strace evidence), links, open questions. | anyone |

Lint rules:
- The 9 headings appear exactly once, at level 2 and in this order.
- No other level-2 headings are allowed. Level-3 and deeper headings are free
  inside a section.
- Sections 2-5 and 7 must be non-empty.
- Section 6 may be `_None proposed yet._` only while `tracking_status: todo`.

### 2.2 The `## Tracking data` block

````markdown
## Tracking data

```yaml
# backend values below are illustrative; real ones need C8 evidence
schema: syscall-tracking/v1
syscall: futex_waitv            # == title suffix == Sysno name
nr_x86_64: 449                  # == Sysno::id()
# --- judgement: agent-maintained; every change needs a Status entry ---
linux_determinism: nondeterministic   # deterministic | conditional | nondeterministic
target_policy: refuse                 # model | emulate | refuse | passthrough | investigate
tracking_status: done                 # todo | in-progress | blocked | done | wontfix
backends:                             # qualification of target_policy per backend
  ptrace: verified                    # verified | partial | untested | broken | not-applicable
  dbt: partial
  kvm: verified
  sabre: verified
  liteinst: verified
  record_replay: untested             # ptrace record/replay
related: []                           # full GitHub URLs only
# --- code facts: tool-owned (`syscall-tracking facts --apply`); never hand-edit ---
code_facts:
  as_of: 18ece74f8b0f68ff57871c63320669424c750e15   # first SHA where these values held
  detcore_class: determinized         # determinized | passthrough | unsupported
  dispatch: "pred:is_futex2_enosys_syscall"  # typed | sysno-guard | pred:<fn> | passthrough | unsupported
  refused: true                       # is_deterministically_refused_syscall
  refused_only_when_fail_closed: false  # is_strict_only_deterministic_refusal_syscall
  subscribed_run: true
  subscribed_record_replay: true
  tests_naming: 1                     # test files naming SYS_/__NR_/Sysno:: + name (C9)
```
````

Why each key earns its place:
- `syscall` and `nr_x86_64` make the 1-1 check and the ABI check mechanical.
- `linux_determinism` replaces the overloaded "Classification:". The values
  are unchanged, so the 288 existing lines migrate mechanically.
- `target_policy` replaces "Recommended Action" with values aligned to what
  Detcore can actually do. The old values map as: PASS-THRU to `passthrough`,
  NEEDS-INVESTIGATION to `investigate`, and NEEDS-HANDLER to
  `model | emulate | refuse`, which an agent chooses.
- `tracking_status` is the one status summary field. It is lint-coupled to the
  GitHub state (3.3).
- `backends`: **justified.** The real defects are per backend: KVM executor
  gaps; sabre/liteinst incomplete interception
  (https://github.com/rrnewton/hermit/issues/1047); DBT executing copied
  children outside Detcore (https://github.com/rrnewton/hermit/issues/1793);
  DBT passing `restart_syscall` to the host
  (https://github.com/rrnewton/hermit/issues/2296). Values default to
  `untested`. `verified` must cite a Status entry with evidence (6.1, check C8).
- `code_facts`: the facts are copied into the issue so that GitHub search and
  readers see them. `as_of` changes only when a fact changes, so a hermit
  `main` bump rewrites only the issues whose facts moved: a reclassification
  PR touches the syscalls it reclassifies, not all 373.

### 2.3 Status entries

```
## Status

- 2026-10-01 · hermit 18ece74f8b0f · syscall-tracking-design · migrated to v1; code: determinized, refused ENOSYS (futex2 absent); T1 covered by tests/c/futex_waitv_enosys.c (c-programs/futex-waitv-enosys, verify ci on ptrace/kvm/sabre/liteinst; dbt disabled: log-name evidence issue).
- 2026-10-03 · hermit abcdef012345 · facts-bot · code_facts changed: dispatch typed -> pred:is_futex2_enosys_syscall.
```

Rules (all enforced by lint):
- Each entry is one bullet:
  `- YYYY-MM-DD · hermit <12+ hex> · <agent-name> · <text>`.
- Dates never decrease.
- Entries are append-only. Every entry in the last-pulled mirror copy must
  still be present byte for byte. `lint --against-mirror` checks this.
- Any change to a judgement key in Tracking data must come with a new entry
  in the same push.

### 2.4 Worked example (complete body)

````markdown
## Tracking data

```yaml
(the block from 2.2)
```

## Summary
`futex_waitv(2)` (Linux 5.16+) waits on a vector of futexes. Hermit presents a
kernel without futex2, so it returns a fixed `ENOSYS` and runtimes fall back to
the legacy `futex(2)` that Detcore models.

## Linux semantics
Registers the task on up to 128 futex queues; returns the index of the first
woken futex, `-ETIMEDOUT`, or `-ERESTARTSYS`/`-EINTR` on signals.

## Determinism analysis
Nondeterministic: the winning index and wake order depend on concurrent
threads, the timeout on host time, interruption on signal timing.

## Hermit design
Refuse (`is_futex2_enosys_syscall`): present a kernel without futex2 so
runtimes take their legacy-futex fallback (the contract stated in the
`TODO-HUMAN-REVIEW(PR-852)` comment on that dispatch arm in `detcore/src/lib.rs`). Revisit if a workload needs futex2 with no fallback
(then `model` via the futex waiter pool).

## Recommended tests
- T1: direct `syscall(SYS_futex_waitv, ...)` returns -1/ENOSYS with valid and
  invalid arguments; verify on all five backends.
- T2: the same under record/replay (ptrace) — the refusal is not recorded as a
  host result.
- T3: a pthread program on a futex2-capable host still completes under
  `--strict --verify` (fallback path exercised).

## Done criteria
Code realizes `target_policy` (refused, fixed errno); T1 passes on ptrace and
on every backend not marked `not-applicable`; no backend is `broken`.

## Status
- 2026-10-01 · hermit 18ece74f8b0f · syscall-tracking-design · migrated to v1 ...

## Additional information
Originally filed 2026-07-24 as "Syscall classification: futex_waitv"
(batch 2, rank 88) as https://github.com/rrnewton/hermit/issues/367.
Host strace evidence from filing: `futex_waitv(...) = -1 EINVAL` on Linux
6.17.13 with invalid arguments.
````

## 3. Title, labels, open/closed

### 3.1 Title

The title is exactly `Syscall tracking: <Sysno name>`, with no other
punctuation, suffixes or tags. Lint requires the regex
`^Syscall tracking: [a-z0-9_]+$` and requires the name to equal
`Tracking data.syscall`. The who-am-i tag never goes in the title.

### 3.2 Labels

- `syscall-tracking` goes on all 373 issues. The mirror selects by this label,
  so it never depends on title search.
- `syscall-tracking-duplicate` goes on the 50 retired duplicates instead of
  `syscall-tracking`, so they leave the 1-1 set but stay findable.
- Optional derived labels `detcore:determinized`, `detcore:refused`,
  `detcore:passthrough` and `detcore:unsupported` would make GitHub's UI
  filterable. The tool would own them, from `code_facts`. Open question Q6.

Why a label instead of a title search: the label filter uses the issue
listing API, so it is not capped at 1000 search results and does not suffer
search-index lag.

### 3.3 Open versus closed

- **Closed** means `tracking_status` is `done` or `wontfix`.
- `done` requires all of the following, which lint checks mechanically:
  - `code_facts` realize `target_policy`: `refuse` needs `refused: true`;
    `model`/`emulate` need `determinized` and not refused; `passthrough` needs
    `passthrough`; `investigate` can never be done.
  - `backends.ptrace` is `verified`.
  - No backend is `broken`.
  - The last Status entry carries evidence.
- **Open** is anything else.
- Lint fails when the GitHub state and `tracking_status` disagree.

The issue closes when the syscall is qualified, not when it is classified.
Under this rule, many of the 74 issues closed on 2026-09-28 do not meet `done`
(Q2).

## 4. Local mirror

### 4.1 What `mb` can and cannot do (measured with mb 0.22.0 and a logging fake `gh`; nothing reached GitHub)

- `mb github import --label/--search/--state` exists. It fetches with
  `gh issue list --json number,url,title,body,state,comments`.
- **`import` is not read-only.** For every imported issue it runs
  `gh issue comment <url> --body "MB_DO_NOT_SYNC ... synced to local minibeads issue sc-1 ..."`.
  Against the real repository that would be 309 bot comments.
- **`link` also posts that comment**, even though its help text says it only
  writes local sync state. It does save the link locally before the comment
  fails.
- `mb github sync --pull-only` **is** read-only: it only runs
  `gh issue view <url> --json number,url,title,body,state,comments`. It also
  needs a link, and a link costs a comment.
- Its mirror format is good: `.minibeads/issues/sc-N.md` (YAML front matter
  plus `# Description`), `.minibeads/comments/sc-N.json`, and
  `github-sync-state.json` with local/remote hashes and comment ids.
- What is missing for this use:
  1. a no-backlink import or link mode;
  2. labels, `stateReason`, `closedAt` and `updatedAt` (they are not even
     requested);
  3. a bulk pull: sync costs one `gh issue view` per issue, measured at 1.2 s
     each through `herdr-run`, so about 7.5 min for 373 issues against 9 s for
     one `gh issue list` call;
  4. file names keyed by GitHub number or syscall rather than `sc-N`;
  5. `mb` is not in the `.herdr-run.yaml` allowlist (`git gh cargo ci-hub`),
     so it cannot reach GitHub from a Claude session at all.

### 4.2 Recommendation: a small custom mirror inside the tool

- **`pull`** makes one `gh issue list --label syscall-tracking --state all --limit 1000 --json number,title,state,stateReason,labels,updatedAt,body,comments` call (via `herdr-run` when `gh` is filtered). It writes:
  - `ignored/syscall-tracking/mirror/<syscall>.md`, the body byte for byte, so an untouched file pushes as a no-op;
  - `<syscall>.meta.json`, with the number, state, `stateReason`, labels, `updatedAt` and a sha256 of the body as the push base;
  - `<syscall>.comments.json`;
  - `_unmatched/` for labelled issues whose title is malformed.

  Before the label exists, it pulls with today's title search.
- **Edit locally, then push**, for agents. The agent edits the mirror file and
  runs `lint`. `push --dry-run` prints a unified diff per issue. `push --apply`
  first re-reads that issue's `updatedAt` and body hash and refuses if either
  moved since the pull (optimistic concurrency). There is one exception: if
  the remote change is only appended Status entries, the tool re-applies the
  local edit on top, because append-only logs merge trivially.
- **Edits made directly on GitHub stay legal** (humans, quick fixes). The next
  `pull` picks them up and `lint` flags anything malformed.
- Agents must not run `gh issue edit` on these issues directly. That bypasses
  the lint gate, and the mirror base would be stale.
- Comments are for discussion only. Nothing machine-read lives in comments.
- Why not edit remotely and then pull for agents: lint can only run before a
  write if the text exists locally first. Remote-first lets malformed bodies
  reach GitHub and be caught only afterwards.

### 4.3 Where it lives

- The **mirror goes under `ignored/syscall-tracking/`**: a per-host cache that
  takes 9 s to regenerate. GitHub stays the single source of truth. A tracked
  mirror on the shared parent `main`, pushed from devbig014 and devbig030,
  would be a second truth: it would conflict on 373 files and drift between
  pulls.
- The tracked outputs are this design and
  `ai_docs/syscall-tracking/STATUS.md`, which `aggregate` generates (one table
  row per syscall plus totals and the snapshot SHA and time). It is committed
  only when its content changes. Ryan reviews status there, as a diff.
- Q4 asks whether to track the mirror anyway.

## 5. Tooling

### 5.1 Shape: two rust-scripts in the parent repository

**1. `scripts/syscall-tracking.rs`** is light: `clap`, `serde`, `serde_json`,
`serde_yaml`, `sha2`, `similar`, and `syscalls = "=0.6.18"`.

| subcommand | does | GitHub writes |
|---|---|---|
| `pull` | the mirror (4.2) | none |
| `lint [--against-mirror]` | schema well-formedness (2.1-2.3, 3.1, 3.3) | none |
| `census` | 1-1 with the enum, duplicates, unmatched; also checks that hermit's `Cargo.lock` pins the same `syscalls` version | none |
| `facts [--rev <sha>] [--apply]` | runs script 2 and diffs `code_facts`; `--apply` rewrites `code_facts` in the mirror and appends the facts-bot Status entry | none |
| `consistency` | the mechanical checks in 6.1 | none |
| `aggregate` | writes `ai_docs/syscall-tracking/STATUS.md` | none |
| `migrate --render` | writes the proposed v1 bodies and titles into `ignored/syscall-tracking/proposed/` | none |
| `push --dry-run \| --apply [--limit N]` | create, edit title or body, label, close or reopen, with a per-issue base check and read-back | only with `--apply` |
| `agent-check prepare \| run \| collect` | 6.2 | none |

**2. `scripts/syscall-tracking-facts.rs`** is heavy. It links hermit's own
crate, so the facts come from Detcore and are never re-parsed. I prototyped
it and it ran:

```rust
//! [dependencies]
//! detcore = { package = "hermit-detcore", path = "<export>/detcore" }
//! reverie = { package = "reverie-core", git = "https://github.com/rrnewton/reverie.git", rev = "<hermit's pin>" }
let run = <Detcore as Tool>::subscriptions(&run_cfg).iter_syscalls().collect::<BTreeSet<_>>();
let rr  = <Detcore as Tool>::subscriptions(&Config { passthru_opt: true, ..run_cfg }).iter_syscalls()...;
for s in detcore::all_pinned_syscalls() {   // not Sysno::iter(): it drops the last row
    detcore::is_unsupported_syscall(s); detcore::is_determinized_syscall(s);
    detcore::is_deterministically_refused_syscall(s);
    detcore::is_strict_only_deterministic_refusal_syscall(s);
}
```

- `<export>` is `ignored/syscall-tracking/hermit-src/`. `facts --rev` fills it
  with `git archive <rev> detcore detcore-model`, so the facts are bound to an
  exact SHA and never come from the shared tree's possibly dirty `hermit/`
  submodule. Right now the parent shows ` M hermit`.
- The `reverie` rev must equal the pin in hermit's `detcore/Cargo.toml`. The
  wrapper rewrites the header from that file, or refuses on a mismatch.
- Two prototype measurements:
  - Output was byte-identical across 4 runs. A cached run took 7 ms.
  - A relative `path =` resolves relative to the script file.
- The cold build was served from rust-script's shared cache, so I have **no
  cold-build number**.

### 5.2 What comes from which crate

| fact | crate and item |
|---|---|
| names, numbers, count | `syscalls` 0.6.18 `Sysno` (re-exported as `reverie::syscalls::Sysno`). Iterate with `detcore::all_pinned_syscalls()`: `Sysno::iter()` skips id 461. |
| class | `detcore::is_determinized_syscall`, `detcore::is_unsupported_syscall`, else PassThrough. `classify_syscall` itself is `pub(crate)`. |
| refusal | `detcore::is_deterministically_refused_syscall`, `detcore::is_strict_only_deterministic_refusal_syscall` |
| subscriptions | `<detcore::Detcore as reverie::Tool>::subscriptions(&detcore::Config)` and `reverie::Subscription::iter_syscalls()` |
| dispatch family | **not exposed.** The family predicates (`is_futex2_enosys_syscall` and others) and the dispatch match in `detcore/src/lib.rs` are private. Phase 2 parses source (6.1, C4). A later small hermit PR could add `pub fn syscall_dispatch_family(Sysno) -> &'static str`, which would make C4 exact. |

## 6. Checks

### 6.1 Mechanical checks (`consistency`, `lint`, `census`)

| id | check | how | how deterministic | today's result |
|---|---|---|---|---|
| C1 | exactly one `syscall-tracking` issue per `Sysno`, and every such issue names a `Sysno` | mirror vs enum | exact | 114 missing, 50 duplicated, 0 unmatched |
| C2 | `nr_x86_64 == Sysno::id()` | enum | exact | n/a (field is new) |
| C3 | `code_facts` == facts at the checked SHA | script 2 | exact for a given SHA; drift is expected and is reported as "needs `facts --apply`" | n/a |
| C4 | every Determinized syscall has an explicit dispatch arm, not the `unexpected => handle_unsupported_syscall` fallback | source parse of the `match classify_syscall` arms in `detcore/src/lib.rs`: typed `Syscall::Camel` variants, `call.number() == Sysno::x` guards, and `is_*(call.number())` predicates expanded from `syscall_classification.rs` | deterministic but brittle: a refactor can break the parser, which must then report **parser-failed**, never pass. Becomes exact once the hermit API above exists. | **294/294 have an arm** (173 typed, 8 sysno guards, 113 via 18 predicates). The probe bites: renaming `Syscall::Membarrier` made it report `membarrier` |
| C5 | Determinized ∪ Unsupported ⊆ record/replay subscription; run mode subscribes all | script 2 | exact. Duplicates detcore's own unit tests (`passthru_opt_subscribes_every_determinized_syscall`), which is cheap and catches a stale pin | holds: 299 ⊆ 299 |
| C6 | judgement vs code: `done` requires `code_facts` to realize `target_policy` (3.3). A warning when `linux_determinism: nondeterministic` and `detcore_class: passthrough` while not `blocked` | fields | exact | 4 such pairs today (1.5) |
| C7 | GitHub state ⇔ `tracking_status`; title ⇔ `syscall`; schema; Status format, monotone dates, append-only | mirror | exact | 309 of 309 fail the schema (expected before migration) |
| C8 | `backends.<B>: verified` needs evidence: at least one Status entry naming a test path that exists at the SHA, plus a manifest cell for B, from that test's `program:`, with the mode enabled and `ci` true for B | `tests/e2e/manifests/*.yaml` (17 files) | the file lookup is exact; "this test exercises syscall X" is heuristic (below) | n/a |
| C9 | tests naming the syscall: `SYS_x`, `__NR_x` or `Sysno::x` in `tests/`, `hermit-cli/tests/` and `detcore/tests/` | `git grep` at the SHA | deterministic but **under-counts**: libc wrappers like `read(` are not counted, and counting them would be noise. So it is advisory only | 148 of 373 syscalls are named explicitly, in 162 files |
| C10 | stale text: "UNCLASSIFIED", "TEMPORARY PASS-THRU", or a hard-coded denominator | regex | exact, advisory | 238 bodies |

There is no scorecard check per syscall. `ci/compat-envelope/cells.json`
holds 9008 cells keyed by test × mode × backend, with no syscall dimension.
The only join is test → `program:` → the C9 heuristic, which is what C8
uses. An exact per-backend "intercepted" fact would need a dynamic probe:
run the `syscaller` binary (`detcore/scripts/syscaller.rs`) for every number
under each backend, and read Detcore's `inbound syscall:` DETLOG line.
- That is deterministic on a fixed host.
- It costs about 373 × 5 runs.
- sabre and liteinst have no in-guest DETLOG sink today, so their column
  would read "unobservable".

This is Phase 6, and it relates to
https://github.com/rrnewton/hermit/issues/1557.

### 6.2 Agent-launched consistency check (`agent-check`)

- **Input bundle**, per syscall or per dispatch family (113 Determinized
  syscalls share 18 predicates, so about 60 to 80 bundles in total instead of
  373). Each bundle holds:
  - the mirror body and comments;
  - the `code_facts` row;
  - the dispatch arm and the handler function bodies, located by C4;
  - the test files from C9 and their manifest cells;
  - the Linux man-page name.
- **Prompt (fixed, versioned in the script):** "For each claim in Summary,
  Linux semantics, Determinism analysis, Hermit design and `backends`, decide
  agree / disagree / unverifiable against the supplied code and tests. Cite
  file:line at the given SHA. Propose missing Recommended tests. Do not edit
  code, GitHub or the mirror."
- **Output:** `ignored/syscall-tracking/agent-checks/<sha>/<name>.json`, of the form
  `{syscall, hermit_sha, prompt_sha256, inputs_sha256, model, claims:[{field, verdict, evidence}], proposed_tests:[...], proposed_status_entry}`.
  `collect` turns the outputs into proposed mirror edits. Those edits go
  through `lint` and `push --dry-run` like any other edit.
- **Determinism:** none, because the verdicts come from an LLM. The tool
  records the model, the prompt hash and the inputs hash, and treats the
  output as advisory. To measure agreement, re-run a fixed sample of 10
  bundles and report the share of identical verdicts. A `disagree` becomes a
  dated Status entry or a GitHub issue. It never becomes a code change.

## 7. Migration plan

Every GitHub write goes through `push --dry-run` first. Ryan reviews the
rendered diff of a sample, then the tool applies in batches of 25, reading
each issue back after the write. Pace: at most 1 write per 2 s and well under
GitHub's secondary limit on content creation (about 80 per minute, 500 per
hour).

| phase | work | writes to GitHub | effort |
|---|---|---|---|
| P1 tooling | `pull`, `census`, `lint`, `facts` (script 2), `consistency` C1-C7, C9, C10 | 0 | ~1 agent-day |
| P2 mechanical render | `migrate --render` for 373 bodies. Variant mapping: A1/A2 Summary → Summary; Kernel Notes → Linux semantics; Determinism Analysis + Nondeterminism Sources → Determinism analysis (with `**Classification:**` → `linux_determinism`); Recommended Action → `target_policy` (NEEDS-HANDLER → `investigate` until an agent decides); Testing → Additional information ("2026-07-24 host strace evidence"). B: Tracking → Additional information. C: State and denominator → first Status entry dated 2026-08-07 at `303113c1`; Semantics → Determinism analysis; Strategy draft → Hermit design; Required tests → Recommended tests; Acceptance → Done criteria. All variants: one first Status entry "migrated to v1", `code_facts` filled in, `backends: untested`. For each duplicate pair, the lower number survives and the other's unique text is moved into Additional information. | 0 | ~0.5 agent-day, plus Ryan's review of about 5 sample diffs |
| P3 agent prose | agents, batched by family, write Recommended tests (missing in 288 bodies), replace stale "unclassified" text (238), choose `target_policy` where NEEDS-HANDLER, and write the 114 new bodies | 0 (mirror and proposed edits only) | ~60-80 bundles; 1-2 days wall time with 4 parallel agents |
| P4 apply | one `gh issue edit --title --body-file --add-label` per existing issue: 259 survivors renamed to `Syscall tracking: <name>`, rewritten and labelled `syscall-tracking`; 50 duplicates renamed and labelled `syscall-tracking-duplicate`; 114 `gh issue create` with the label (373 labelled in total); 50 duplicate closes with `not planned` and a comment linking the survivor (gh 2.86 has no `--duplicate-of`); then open/close by 3.3 | about 259 + 50 + 114 + 50 = 473 writes plus one read-back each ≈ 950 calls × 1.2 s ≈ 19 min of calls, spread over about 2 hours to respect the hourly limit | ~0.5 agent-day |
| P5 routine | on each hermit gitlink bump: `facts --apply`, `push`, `consistency`, `aggregate` | only the issues whose facts changed | automated |
| P6 optional | hermit PR exposing `syscall_dispatch_family`; the dynamic per-backend probe matrix (6.1); agent-check over all bundles | none until reviewed | ~2-3 agent-days |

When the pinned `syscalls` crate moves
(https://github.com/rrnewton/hermit/issues/1831: 12 Linux-master entries are
missing), `census` reports the new names and `push` creates their issues.

## 8. Open questions for Ryan

- **Q1 Duplicates.** I plan to retire the higher-numbered issue of each of
  the 50 pairs: rename it too, relabel it `syscall-tracking-duplicate`, close
  it as not planned, and link the survivor. Is that what you want? Or would
  you rather delete the duplicates (only possible for an admin)?
- **Q2 Closed meaning.** Under 3.3 (qualified, not classified), most of the 74
  issues closed on 2026-09-28 fail `done`, because ptrace was never
  `verified`. Should those be reopened, or grandfathered as `done` with a
  Status entry recording the weaker basis?
- **Q3 Backends.** Should the `backends` map stay at six keys (five backends
  plus record/replay), or be ptrace-only until the probe matrix (P6) exists?
- **Q4 Mirror.** I recommend keeping it in `ignored/` and tracking only
  `STATUS.md`. Do you want the mirror itself tracked under `ai_docs/` for
  history?
- **Q5 Who may set `done`.** Is an agent with evidence enough? Or does a
  determinism claim need human review, like hermit's post-facto review
  triggers?
- **Q6 Derived labels.** Should the tool own `detcore:*` labels, or should
  this information stay in the body only?
- **Q7 `mb`.** Should we ask upstream minibeads for a no-backlink import,
  label import and bulk pull? Or drop `mb` for this, as recommended?
- **Q8 Where the tool lives.** The plan puts it in parent `scripts/`, which
  link hermit by `git archive` per SHA. Do you prefer hermit's
  `ci/rust-script-bin/`, so the facts exporter is reviewed with the code?
- **Q9 Stale docs.** `docs/SYSCALL_COVERAGE.md` ("87 trapped / 286 direct")
  and `ai_docs/syscall-coverage-map.md` (2026-07-21) contradict the current
  373/373 subscription. Should `aggregate` replace them?
