# ci-hub changes: isolated clone, sealed commit, serialized activation

`ci-hub/` is live coordinator infrastructure. Agents run `validate-run`,
`apply-local-label`, landing, receipt verification, and health checks from the
shared primary checkout while work is in progress. Therefore **no task edits
`ci-hub/` in `/home/newton/work/dev-hermit`**. A file lock is insufficient:
it prevents a second writer, but readers can still execute a half-applied edit.

The supported workflow is `scripts/ci-hub-clone-change.py`:

1. A coordinator assigns the task and its complete `ci-hub/` path claims.
2. The coordinator runs `create`. It makes a fresh, per-change clone at
   `~/temp/dev-hermit`, records the exact fetched base and claims under the
   clone's `.git/`, installs hooks, and creates a task branch. Creation refuses
   an existing destination; the old clone must be dispositioned before another
   change starts.
3. The implementer is granted that exact clone as a temporary write lane. All
   source edits, generated files, and tests stay there.
4. After one coherent commit, `seal` fetches `origin/main`, requires the
   candidate to be exactly one commit on that current base, requires a clean
   tree, rejects every changed path outside the recorded claims, runs at least
   one named test command, and records the exact candidate, base, changed-path
   list, and binary-diff digest under `.git/`.
5. `publish` rechecks the seal and current remote base, then invokes the
   serialized `scripts/parent-main-write publish HEAD`. A competing publisher
   moves `origin/main` and makes this candidate refuse; the loser rebases its
   private commit, reruns tests, and reseals.
6. The coordinator runs `activate` against the primary. It refuses any local
   staged, unstaged, or untracked state in the candidate paths, requires current
   remote main to contain the published candidate, calls the registered
   `parent-main-write sync --automatic`, and verifies that the activated remote
   tip still contains the candidate and that its candidate paths are clean.

Example:

```bash
# Coordinator, before dispatch. The default destination is deliberately fixed.
scripts/ci-hub-clone-change.py create \
  --task fix-receipt-carrier \
  --path ci-hub/validation

# Implementer, inside ~/temp/dev-hermit, after one commit.
scripts/ci-hub-clone-change.py seal \
  --test 'bash ci-hub/validation/test_verify_receipt.sh'
scripts/ci-hub-clone-change.py publish

# Coordinator, from either checkout after publication.
scripts/ci-hub-clone-change.py activate \
  --clone ~/temp/dev-hermit \
  --primary /home/newton/work/dev-hermit
```

## Answers to the operational questions

**Is the clone reused?** No. It is per-change and disposable. The fixed default
path serializes same-box work and makes an abandoned clone visible. Disposal is
coordinator work after publication, activation, and recovery checks; this tool
does not delete a clone. A separately authorized `--clone` path is permitted
for migration or another machine, but it does not weaken the publication CAS.

**How is completeness checked?** The seal proves structural completeness: one
commit, latest base, clean tree, no unclaimed paths, nonempty diff, successful
named tests, and an exact diff digest. Review still decides semantic
completeness; no script can infer that a required companion edit was omitted.
The reviewer inspects the full one-commit diff and the recorded test commands.

**What stops conflicting clones?** On one box the fixed destination prevents a
second normal clone. Explicit alternate clones require coordinator path claims.
Across boxes, publication is serialized by `parent-main-write`; its fresh-base
check and Git's non-fast-forward rule allow only one candidate from a base to
land. A losing candidate cannot reuse its old seal. Activation may occur after
later complete commits land, but only when the sealed candidate is their
ancestor; it never mistakes an unrelated or rewritten history for publication.

**How does an agent get a clone without violating its write lane?** It does not
create one speculatively. The coordinator runs `create`, records the exact path
on the task, and grants that path as a temporary lane. The agent writes only
there. Primary sync remains coordinator-owned.

## Important limit: Git pull is not a filesystem-atomic directory swap

This protocol removes the long-lived half-edit window and guarantees that only
one complete, tested commit is eligible for activation. It does **not** make
`git merge --ff-only` atomically replace several files: Git updates the index
and working tree path by path, and ci-hub readers do not take the parent writer
lock. A reader can therefore observe a short mixed-version checkout window.

Do not describe the current activation as strict working-old/working-new
atomicity. That stronger property requires a second mechanism change: execute
ci-hub from a content-addressed, immutable commit tree and switch one `current`
pointer with `rename(2)` (the pattern already used by
`scripts/main-tooling-link.rs`), or make every reader participate in a shared
activation lock. Until then, the improvement is from hours of exposed partial
edits to one serialized fast-forward operation, not zero interleavings.

## Migration on 2026-08-11

- The receipt-carrier compatibility fix is already on `origin/main` in commit
  `07110b3`; it must not be recopied into a new change.
- The tag-printer change is already on `origin/main` in commit `b2e03696`; its
  earlier dirty-primary implementation was published before this protocol
  landed and must not be recopied. Future `ci-hub/` changes use this workflow.
