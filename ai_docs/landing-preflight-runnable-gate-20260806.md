# The landing preflight, as a runnable gate

**Task:** `landing-preflight-three-checks-before-trusting-any-green` · hermit-clone (opus-5), 2026-08-06
**Local, no egress.** Delivered: `ci-hub/landing/preflight.py` + 24 negative/positive tests +
discoverable usage in `ci-hub/landing/README.md`.

## What was asked, and the bar

> "I have retyped these three into four separate agent dispatches tonight. A rule I retype is a rule
> that decays — it needs to live where an agent finds it without me."
>
> VERIFY: the preflight exists as a runnable script or a checklist an agent can execute **without the
> coordinator restating it** — and **each check has a NEGATIVE test proving it refuses the bad case**.

## The gate

```
python3 ci-hub/landing/preflight.py --sha <handed> --pr <n>        # 1
python3 ci-hub/landing/preflight.py --ledger-event <run-result.json> \
  --receipt-sha <validated-sha>                                    # 2
python3 ci-hub/landing/preflight.py --landed-pr <n> --checkout hermit   # 3
python3 ci-hub/landing/preflight.py --diff-of worktrees/<slot>/hermit   # 4
```

Exit 0 only when every requested check PASSes.

**`UNKNOWN` blocks.** This is the load-bearing design decision. An unresolvable head, or a remote
that was not freshly fetched, returns `UNKNOWN` and the gate exits 1 — because the entire failure
class here is "I could not tell" being laundered into "it is fine". A preflight that passed when it
could not answer would reproduce the defect it exists to prevent.

**Every check is a pure function over injected data**, with the I/O in thin collectors. That is what
lets all 24 tests run with no network, and it keeps each refusal reproducible from recorded inputs
rather than from an API that has since moved on.

## The five checks, and the incident each encodes

| # | check | the defect it refuses |
|---|---|---|
| 1 | `check_sha_is_current` | four handed SHAs went stale in one night, one quoted into agent instructions for hours after main advanced twice |
| 2 | `check_green_carries_executed_tests` | `--features` gating: build ok, target ran, **zero tests executed**, SUCCESS reported |
| 3 | `check_landed_by_ancestry` | ancestry on the PR head read **79 unlanded when 46 had landed**; `MERGED` alone missed ~12 PRs orphaned by force-push |
| 4 | `check_no_uncommitted_patch_override` | a `[patch."…reverie.git"]` override riding along in a commit |
| 5 | `check_no_byte_identical_branch` | opening work that already exists verbatim (live on #355) |

### Check 2 reads the producer-owned row

The original rule parsed libtest banners from a validation log. It now delegates
the decision to `ci-hub receipt-digest`, whose Rust reader validates the typed
ledger event and imports Hermit's shared `HistoryRow`. A missing event or commit
binding refuses; a malformed event or failed reader is `UNKNOWN`; missing,
zero, and negative typed counts do not pass. Human log text is no longer an
input or a fallback.

### Check 3 is structurally prevented from consulting the PR head

The 79-vs-46 misread came from testing ancestry on the head, which after a rebase replay is *never*
an ancestor. A test spies on the ancestry callable and asserts it is invoked with the merge commit
**and nothing else**:

```python
assert asked == [oid], f"the head must never be tested for ancestry: {asked}"
```

That pins the property, not just today's output.

## Verification

The focused preflight tests and the Rust `ci-hub` unit tests pass.

Each check has a negative *and* a positive control, because a check that refuses everything is as
useless as one that refuses nothing and only the pair distinguishes them. Beyond the obvious ones:

- hex **case difference** on a SHA is not staleness (guards against flags-everything)
- a **removed** patch override is the fix, not the defect — the check keys on added lines
- holding human text fixed while the typed reader changes from positive to zero
  moves the verdict from `PASS` to `REFUSE`
- removing the typed row while a passing banner remains still refuses
- `UNKNOWN` blocks the gate (`rc=1`), and the gate can genuinely succeed (`rc=0`)

**Gate exercised end to end:**

```
REFUSE  sha-is-current: the handed SHA is STALE -- the branch has moved
REFUSE  green-carries-executed-tests: shared HistoryRow carries no positive executed-test count
  exit=1
PASS    sha-is-current: handed SHA is still the PR head
PASS    green-carries-executed-tests: shared HistoryRow carries a positive executed-test count
  exit=0
```

### It caught the live trap, unplanted

Run against the worktree the task names as still carrying the override:

```
$ python3 ci-hub/landing/preflight.py --diff-of worktrees/250-delegate/hermit --no-network
landing-preflight: REFUSE  no-uncommitted-patch-override: an uncommitted
  [patch."...reverie.git"] override is in the diff -- it would redirect the dependency
  for everyone who builds this commit
  exit=1
```

Confirmed genuine rather than a false positive — the diff contains
`[patch."https://github.com/rrnewton/reverie.git"]` with five path redirects into the slot, under a
comment that reads *"LOCAL-ONLY build override (NOT for commit)"*. **A real, currently-live defect,
detected by the gate on its first run against real content.**

## Honest limits

- **Checks 1 and 3 could not be exercised against live GitHub** (egress down). Their *logic* is
  fully bracketed via injected heads/ancestry, and the collectors (`gh_pr_head`,
  `gh_pr_merge_info`, `git_is_ancestor_factory`) are thin and unbracketed — a wiring error inside a
  collector would not be caught by these tests. First online use should confirm the collectors
  return what the predicates expect.
- **Check 5 is library-only.** It needs a remote branch/tree listing to be a CLI check, which needs
  network; the predicate and its tests exist, the collector does not.
- The gate does not *enforce* itself on any landing path — it is runnable and discoverable, not yet
  wired into `land-pr.sh`. Wiring it is the obvious follow-up and is deliberately not silent here:
  an unwired gate is exactly the "present but inert" shape this lane has been finding all session.

## Files

`ci-hub/landing/preflight.py` · `ci-hub/landing/test_preflight.py` ·
`ci-hub/ci-hub.rs` · `ci-hub/landing/README.md`.
