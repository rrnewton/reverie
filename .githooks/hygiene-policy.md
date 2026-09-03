# Repo hygiene policy (parent dev-hermit) — experiments/ + debug/

Commit **code + reports + machine-readable state**. Never commit raw giant logs,
boot artifacts, dumps, VM/kernel images, or binaries.

## Default practice: ignored/-first

Write anything you know you won't commit — raw logs, boot artifacts, scratch, big
outputs — **directly into an `ignored/` dir from the start**. `ignored/` (and
`**/ignored/`) is gitignored, so there is **no commit-time judgment call**: junk
never enters the index. Only reports, the ledger/notebook, and machine-readable
state (`*.json`, `NOTEBOOK.md`, small curated `.log`/`.csv`/`.md`) live tracked in
`experiments/<foo>/` and `debug/<bar>/`.

## What is hard-ignored (clearly never commit)

VM/disk images (`*.qcow2 *.img *.raw *.iso`), kernels/initrd (`bzImage vmlinux
initramfs*.cpio* *.cpio`), boot/run dirs (`boots-*/ boot-*/ runs/`), archives
(`*.tar* *.tgz *.gz *.zip *.zst`), binaries (`*.bin *.a *.o *.so`), core dumps and
`*.perf.data`. `*.log` is deliberately **not** blanket-ignored — small curated
logs may be tracked; put big/raw logs under `ignored/`.

## The size backstop (`.githooks/pre-commit`)

A pre-commit hook WARNS and BLOCKS when a staged file exceeds a soft size limit
(default 1024 KiB; `HERMIT_HYGIENE_MAX_KB` to tune). It is only a safety net for
when the ignored/-first default is missed. Override when you have verified the
file is a genuinely-needed curated report/state:

```
HERMIT_HYGIENE_OVERRIDE=1 git commit ...
```

Install (per clone; `core.hooksPath` is local config, not tracked):

```
scripts/setup-hooks.sh
```

## Per-directory tuning

Any `experiments/<foo>/` or `debug/<bar>/` may add its **own `.gitignore`** to tune
what lives-but-ignored locally (`dbg new-episode` scaffolds a starter one).

## VCS_MISSING.md (required per dir)

Every `experiments/<foo>/` and `debug/<bar>/` must carry a `VCS_MISSING.md`
describing what is NOT checked in (won't exist on a fresh clone): which artifacts
are missing, whether each is **regeneratable** (and how), and whether any tracked
code **depends on reading** it (which would fail on another machine). `dbg
new-episode` scaffolds it; `dbg vcs-check <episode>` lists the currently-ignored
paths so you can keep it current.

## Demo-touching commits (demos/**)

Separate hard gate: every runnable-demo change needs an adversarial green-demo
attestation before merge. See `demos/ADVERSARIAL-REVIEW-POLICY.md`, the CI job
`demo-review-gate`, and `.githooks/commit-msg` (checker: `scripts/check-demo-review.sh`).

## Canonical commit-body disclosure

Every new commit in the parent and its product repositories puts the exact
output of `ci-hub/bin/who-am-i --tag --role ROLE` on the first body line:

```
[<team>, <agent>, <model>, devbigNNN, role=<role>]
```

The machine and agent are separate derived fields, not inferences from author
email, branch name, host alias, or a free-form team string.
`scripts/setup-hooks.sh` derives this clone's machine token from the hostname
and installs a composite `commit-msg` hook in the common Git directory for the
parent and every initialized submodule. Product worktrees share those common
directories. The dispatcher checks the machine, agent, and model family and
then delegates to each repository's own tracked `.githooks/<hook>`, so
installing attribution never disables a product guard.

The earlier final-line form `[<role>, <model>] [<team>, devbigNNN]` remains an
explicit compatibility path for stored history and noninteractive producers
that have not migrated. If a five-field disclosure is present, it is
authoritative: it must appear exactly once on the first body line, and a legacy
footer cannot contradict its machine or role. Legacy and canonical team slugs may differ;
historical writers used an agent-specific legacy team before the canonical
producer standardized the default team.

For example, the exact machine-attribution history query for devbig030 is:

```
git log --all --extended-regexp \
  --grep='^\[[^,]+, [^,]+, [^,]+, devbig030, role=[^]]+\]$'
```

`scripts/check_commit_attribution.py --commit HEAD` is the CI/lint form;
`--range <base>..<head>` checks every commit in a proposed publication.

## GitHub main health before push

`.githooks/pre-push` runs `ci-hub/bin/main-health` before every parent
push. It polls current-main `push` workflow runs for `rrnewton/dev-hermit`,
`rrnewton/hermit`, and `rrnewton/reverie` through `with-proxy gh`. A red main or
query failure emits a hard warning. The hook deliberately does not block because
the pending push may be the repair, but the coordinator must not claim green
until the live report is green.

## Primary checkout freshness before commit

`.githooks/pre-commit` runs `scripts/primary_checkout.py check` before every
parent commit. It warns when `hermit/`, `reverie/`, or `liteinst2/` is detached
or differs from the repository's live `origin/main`, when a parent gitlink is
stale, or when Hermit's tracked Cargo manifests do not pin the exact Reverie
main SHA. The warning is nonblocking so a gitlink or tooling repair can still be
committed. Run `make checkout-fresh` to fetch, check out `main`, and fast-forward
each clean primary, then commit and push the three gitlinks as one coherent
snapshot. Dirty primaries are preserved and skipped with a warning. Tick-hub
runs this same strict routine every five minutes; it hard-warns instead of
publishing if any cleanliness, branch, freshness, pin, or parent-main gate fails.

### The freshness invariant (`make check-primary-freshness`)

`check` above is advisory and product-scoped. The **invariant** is stated once,
in `primary_checkout.py`, and evaluated over **every primary including the
parent**:

> A primary is FRESH iff it is initialized, **not bare-flipped**, on `main` (not
> detached, not another branch), **exactly equal** to that branch on `origin`,
> and clean. A product checkout's commit differing from the parent gitlink is
> classified by that product's own check rather than treated as parent dirt;
> a staged gitlink update is still parent dirt and is never hidden.

Drift is reported by class — `bare`, `detached`, `wrong-branch`, `behind`,
`ahead`, `diverged`, `dirty`, `uninitialized`, `unknown` — naming *which* primary
and *how*, each with the exact command to fix it. Exit `0` fresh, `1` drift,
`2` at least one primary could not be evaluated (**nothing proven is not a
pass**).

Three properties are deliberate:

- **`behind`, `ahead` and `diverged` are separate classes.** "HEAD differs from
  origin/main" is not actionable; the three need different responses, and a
  relationship is only asserted when the remote commit is present locally to
  prove it. Otherwise the class is `unclassified-drift` and the tool says to
  fetch.
- **Nothing is reset, forced or fast-forwarded automatically.** A fast-forward
  moves the working tree of a shared integration surface with live sibling
  processes and ~126 dependent worktrees. `--restore-safe` repairs exactly one
  class, the accidental `core.bare` flip, because it rewrites a single config
  flag and touches no ref, index or file content.
- **Dirt is reported, never remedied.** The changes may be another agent's
  (Invariant 5), so the remediation is "attribute the changes first".

**Why the bare flip kept escaping.** Under `core.bare=true` the directory still
has `.git`, `branch --show-current` still answers and `rev-parse HEAD` still
answers, so a refs-only check sees a healthy primary while every work-tree
operation fails. The binding observation is
`git rev-parse --is-bare-repository`; note `git -c core.bare=false …` does **not**
override it.

**The reachable-accident root shape is documented, not yet normalized.**
`hermit/.git` is an in-tree repository directory hosting ~126 worktrees, with no
`.git/modules/hermit` — unlike the parent `reverie` and `liteinst2` submodules,
which use the standard gitfile + `.git/modules/<name>` layout and have never
drifted. The parent `agent-utils` path is a symlink to the nested checkout in
Hermit, not a fourth parent submodule.
Until that is normalized, the guard above is what re-asserts `core.bare=false`.

## The shared index: never `git commit` bare in the parent

Every agent in this parent shares one working tree and therefore **one
`.git/index`**. `git add` is not private. A bare `git commit` commits everything
currently staged, including paths another agent staged seconds earlier. This is
not hypothetical: six staged files were swept into commit `0b40af7` on
2026-08-06. Bare `git commit --amend` sweeps identically — which means the
landing `union-rebase.sh` scripts can absorb a bystander's work.

**The rule: stage and commit your explicit paths in ONE step.**

```bash
git add <your paths> && git commit -m "msg" -- <your paths>
```

`git commit -- <paths>` builds a temporary index from `HEAD` plus the named
paths, so the commit can only contain what you named, and everyone else's staged
work is left untouched. Verify afterwards with
`git log --oneline -1 -- <your paths>` that your commit owns them.

Three things worth knowing before you rely on it:

- **It only protects other people from you.** If someone else commits bare while
  your paths are staged, you are still swept. That is why the rule has to be
  universal, and why `.githooks/pre-commit` warns rather than relying on memory.
- **It commits the WORKING TREE, not what you staged.** If you staged one version
  and then edited the file, the edited version is what lands. Do not combine it
  with partial staging (`git add -p`).
- **A brand-new file must be `git add`ed first**; a pathspec commit alone refuses
  a path git has never seen.

Rejected alternative: a per-agent `GIT_INDEX_FILE`. It does give private staging,
but a commit from an index seeded by an earlier `read-tree HEAD` **silently
reverts** anything committed in the meantime — measured, it discarded a
concurrent change with no conflict and no warning. Strictly worse than the
problem it solves.

The `.githooks/pre-commit` shared-index guard **blocks bare commits by default**
(since 2026-08-06). Deliberate exception: `HERMIT_SHARED_INDEX_GUARD=warn` to
report and allow, `=off` to silence. It is independent of
`HERMIT_PIN_DRIFT_OVERRIDE`, which is a different guard.

The earlier warn-only default cited "12 call sites with no pathspec". **That
census was wrong twice**: it counted error-message strings as call sites, and its
grep missed the `--only` / `-o` plus separate-`--` argv form. Re-derived, every
tool site that commits in the *parent* working tree was already safe:

| site | form |
|---|---|
| `ci-hub/ci-hub.rs` ×2 | `git -C <root> commit -m MSG -o -- <path>` |
| `scripts/primary_checkout.py` | `git -C <root> commit --only -m MSG -- <paths>` |

The union-rebase amends are not parent commits at all — both scripts take
`WT=${1:?hermit worktree path}` and `cd "$WT"`, so they use that worktree's own
index and cannot race the parent. No call site needed converting; `-o -- <path>`
and `--only -- <paths>` were both verified ALLOWED under block before flipping.

Evidence and the planting harness (29 checks):
`experiments/shared-git-index-race_20260806/`.
