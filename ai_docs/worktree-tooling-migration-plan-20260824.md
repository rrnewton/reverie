# Migration plan: dev-hermit's bespoke `scripts/*worktree.rs` → the common `wrkslots`

Date: 2026-08-24
Author: `agent(hermit-011)`, Claude lane
Task: `migration-plan-bespoke-worktree-rs-to-wrkslots`
Status: **plan only — no cutover, nothing implemented**

---

## 0. The gating question, answered: do both tools write `worktree-state.json`?

**No. They do not. This is not a live defect.**

Established structurally, not by grepping for a string. `wrkslots.py` constructs
exactly two state filenames, at `_active_path` and `_archive_path`:

```python
return config.worktrees / f"ACTIVE.{selected}.json"
return config.worktrees / f"ARCHIVED.{selected}.json"
```

Its complete write set is `worktrees/ACTIVE.<machine>.json`,
`ARCHIVED.<machine>.json`, `JOURNAL.<machine>.*`, `ACTIVE.global.lock`, and
`.wrkslots.yml`. Every one passes through `_validate_name`, so no input can
produce a different filename. Its only atomic write is `os.replace(temp, path)`
with `path` from those constructors. **`worktree-state.json` is not constructible
by any code path in the tool.**

The Rust side owns that file alone: `scripts/release-worktree.rs:488`
(`root.join("worktree-state.json")`), the lock at `:116`, the managed
`BEGIN/END` markers at `:549`. 153 slots, modified today.

So there is no shared write path, no interleaving, and no two-notions-of-proof
race on one registry.

### But there is a real defect one step over, and it belongs at the top

**`wrkslots` is installed and inert in dev-hermit, and the hazard is armed rather
than fired.**

| evidence | value |
| --- | --- |
| `worktrees/wrkslots -> ../agent-utils/py/wrkslots.py` | created 2026-08-14 23:33 |
| `ACTIVE.devbig014.json` / `ARCHIVED.devbig014.json` | `revision: 0`, `slots: []`, `records: []`, same minute |
| `revision: 0` per its own schema | **never mutated** |
| `.wrkslots.yml` | committed later, `c3ce7dc9` |
| invocations from `scripts/`, `ci-hub/`, `Makefile`, `AGENTS.md` | **zero** |
| mentions in any `scripts/*.rs` | **zero** |
| slot overlap between the two registries | **0 of 153** |

The first person to run the symlink starts managing slots from an empty registry
with no knowledge of the 153 rows the Rust tool owns — including the 20
`lease-quarantined` ones. It would not conflict. It would believe every slot is
free, and nothing would detect that. A write race at least produces a
disagreement; this produces two confident, disjoint truths.

**Step 0 of this plan is to disarm it.** Remove the symlink until cutover, or
make `wrkslots` refuse when a populated `worktree-state.json` exists.

### Count correction

The originating task recorded 7 quarantined rows. It is **20**. Registry census
at time of writing:

| status | rows |
| --- | --- |
| `released` | 80 |
| `active` | 34 |
| **`lease-quarantined`** | **20** |
| `release-requested` | 12 |
| `owner-lease-revoked` | 7 |

The 7 matches today's `owner-lease-revoked` count, so the earlier note likely
counted the wrong status.

---

## 1. Feature comparison, both directions

Bespoke Rust: `allocate-worktree.rs` 2776 + `release-worktree.rs` 6210 +
`check-worktree-registry.rs` 415 = **9401 lines**. `wrkslots.py`: **3951 lines**.

### What `wrkslots` does that the bespoke tool does not

This is the direction usually skipped, and it is where the migration's value is.
It also inverted two of my own assumptions.

- **N named repos per slot, generically.** `create --repo NAME=PATH` is
  *repeatable*, with matching `--remote-url/--branch/--start` per name. The Rust
  tool **hardcodes exactly three**: `allocate-worktree.rs:165-167` tests
  `dir.join("hermit") && dir.join("reverie") && dir.join("liteinst2")`, and
  `:945`/`:1069` loop over that literal list. I expected multi-product to be the
  blocking gap; `wrkslots` is strictly *more* general.
- **A journal and a resume path.** Every mutating command journals; `recover`
  resumes one interrupted create or removal, with `--discard-partial` gated on
  the prior durable file being valid. The Rust tool has no equivalent.
- **No quarantine concept at all.** Zero hits for
  `quarantin|wedged|stuck|unrecoverable|terminal`. There is no state that only
  manual registry editing can clear — which is exactly the bug being migrated
  away from.
- **The absence-as-evidence mechanism already exists.**
  `recover-unbound-owner` requires `--recovery-note` **and** `--validation`
  **and** an optional `--limitation`: the operator must state what was validated
  *and what that validation could not establish*. That is the correct shape for
  an unprovable case — evidence with a declared boundary, journaled — rather than
  a `--force`. It is the pattern the missing release path needs, already built.
- **Atomic per-machine shard replacement** under an explicit state lock, plus
  `_assert_registry_storage_consistent` and a mutation guard that refuses when an
  unrelated registry row changed (`:1141`).

### What the bespoke tool does that `wrkslots` does not

This is the real migration work.

- **Submodule administration.** dev-hermit slots are submodule-bearing by
  construction. `wrkslots`' *only* submodule reference is
  `--no-recurse-submodules` — it deliberately does not descend. The Rust tool
  carries `--recursive`, `--recover-submodule-cleanup`, and a submodule-admin
  quarantine/recovery apparatus at `release-worktree.rs:2074-2660`. **Largest
  gap; decides the migration's cost.**
- **The tmux/pane lease model.** `owner_sidecar`, `tmux_pane_id`, lease states
  (`active`/`draining`/`quarantined`), `--lease-json`, `--fence-token`,
  `--expected-generation`, `automatic_handoff`. `wrkslots` binds owners by PID
  via `adopt`/`heartbeat`/`finish`. A different liveness model — not obviously
  worse, but every downstream consumer reads the Rust shape.
- **Two-phase drain handoff.** `--request-release` hands a slot between two live
  agents. `wrkslots finish` records an owner-alive handoff, so the concept
  exists, but the two-phase drain does not map 1:1.
- **Reaper integration.** `--stale-clear` / `--stale-clear-all`, and the
  read-mostly sharer model (`--i-promise-this-agent-is-read-mostly`).
- **`--repair` for registry drift, and it is load-bearing today.** The registry
  currently reports **130 drift rows of 152** and 213 drift cells of 456. Any
  migration inherits that drift; `--repair` is what currently contains it.

---

## 2. The known defect, carried in rather than left behind

The `--request-release` quarantine hole is an **input** to this plan, and it
argues for migrating rather than patching: `wrkslots` does not have the hole, and
already has the mechanism the fix would need (`recover-unbound-owner`).

So the plan does **not** port it forward. The ~40-line point fix beside the
quarantine site in `release-worktree.rs` becomes *optional* — worth doing only if
cutover is months away and the 20 rows begin blocking allocation.

### The constraint survives the migration and must be tested, not described

A missing directory cannot satisfy a verifier that compares recorded state
against current state — there is nothing to compare. Absence must therefore be
admitted as **its own positive evidence**. The failure mode this guards against
is specific and severe: a naive `exists()` check conflates *"the slot was
removed"* with *"the filesystem is unreachable"*, so **an unmounted volume would
release every slot at once.** A `--force` or `--skip-checks` flag ships exactly
that, and launders every other unprovable case with it.

#### Acceptance tests — required before any release path lands, in either tool

| # | Given | Expect |
| --- | --- | --- |
| **A1** | slot dir absent; **containing dir exists and is readable**; every recorded path returns `ENOENT` | **RELEASE**, and journal the operator note |
| **A2** | slot dir **exists** and is dirty | **REFUSE** — a release path that cannot refuse is the defect being fixed |
| **A3** | slot dir absent, but `stat` returns **`EACCES`** | **REFUSE.** Permission denied is proof of *ignorance*, not absence |
| **A4** | slot dir absent, but `stat` returns **`EIO`** | **REFUSE.** Same reason |
| **A5** | **containing directory itself unreadable or absent** (unmounted volume) | **REFUSE, for every slot.** This is the mass-release scenario; it must not release even one |
| **A6** | status is anything other than `lease-quarantined` | **REFUSE** — the path must not generalise into a bypass |
| **A7** | operator note empty or missing | **REFUSE** — follows the `--coordinator-finalize-orphan-residue` / `recover-unbound-owner` precedent |
| **A8** | any invocation | **no `--force` and no `--skip-checks` exists to be passed** |

A3–A5 are the tests that distinguish this from a force flag. A1+A2 alone would
pass a bypass wearing a check.

---

## 3. Coupling: what else reads the registry or the layout

Estimates counting only the two worktree scripts are wrong by roughly half again
the size of `wrkslots` itself. These read `worktree-state.json` or the
`worktrees/<slot>/<product>` layout directly and must be ported or retired:

| consumer | lines |
| --- | --- |
| `scripts/status-log.rs` | 2734 |
| `scripts/codex-slot-sentinel.rs` | 930 |
| `scripts/slot-owner-authority.py` | 539 |
| `scripts/worktree_liveness.py` | 432 |
| `scripts/check-worktree-registry.rs` | 415 |
| `scripts/check-agent-slot.sh` | 129 |
| `scripts/verify-slot-pushed.sh` | 120 |
| **total** | **~5300** |

Plus nine `scripts/*worktree*-test.sh` / `check-agent-slot-test.sh` suites that
assert the current schema.

**In-flight and entangled**, as the owner flagged: `ci-hub/validate/aggregate.py`,
`ci-hub/validate/mutation_suite.py`, and `ci-hub/validate/tests/test_worktrees.py`
reference `worktrees/` or `worktree-state`. The validate on-disk record format and
the slot layout are therefore coupled; changing both at once is the thing to
avoid.

---

## 4. Sequence, with what must stabilise before each step

Cutover is **not urgent**. The owner named the pressure-test and the validate
on-disk records as the in-flight work that must settle first. Steps are marked
**BLOCKED** (must wait for that settling) or **FREE** (independent of it).

| # | Step | Gate |
| --- | --- | --- |
| **0** | **Disarm the dormant symlink.** Remove it until cutover, or make `wrkslots` refuse when a populated `worktree-state.json` exists. | **FREE.** Independent of everything; removes a live foot-gun that otherwise sits armed for the whole migration. **Do this now.** |
| **1** | **Spike the submodule gap** against one throwaway slot. | **FREE.** Touches no shared state and no validate record. This is the **go/no-go** and should happen early precisely because it is cheap and can cancel the rest. |
| **2** | **Freeze the schema mapping**: 153-row `worktree-state` model → `ACTIVE`/`ARCHIVED` shards, including an explicit disposition for the 20 `lease-quarantined` and 7 `owner-lease-revoked` rows, which have no `wrkslots` equivalent. | **FREE** to write. **BLOCKED** to execute — clearing accumulated state needs owner authorisation and should not happen during in-flight work. |
| **3** | **Implement missing features in `wrkslots`** (agent-utils), in order: submodules → two-phase drain → reaper integration → `--repair`. | **FREE.** Lands in agent-utils behind its own tests; changes nothing in dev-hermit until adopted. |
| **4** | **Dual-run read-only**: `wrkslots status` against the live Rust registry with a reconciler that reports disagreement and **writes nothing**. Run for several days across real allocations. | **BLOCKED** — needs stable validate records to distinguish real disagreement from record churn. Also where the 130 drift rows get characterised rather than migrated blind. |
| **5** | **Port the consumers** (§3), one at a time, behind the reconciler. | **BLOCKED** — `status-log.rs` and the `ci-hub/validate` trio are the entangled ones. Non-validate consumers (`check-agent-slot.sh`, `verify-slot-pushed.sh`) can go earlier. |
| **6** | **Cut over** one slot → one agent → the fleet. | **BLOCKED.** |
| **7** | **Retire** the Rust scripts and their test suites in one commit, once no consumer references them. | **BLOCKED.** |

The owner's fallback — during migration the coordinator can operate on worktrees
directly — means steps 5–7 do not need continuous tool availability. That removes
the hardest scheduling constraint and is why this can proceed in slices.

---

## 5. Recommendation

**Migrate.** The deciding arguments are the three that inverted expectations:
`wrkslots` is *more* general where it was assumed to be less (N repos vs a
hardcoded three), it has *no* unclearable state, and it *already implements* the
evidence-with-declared-limits pattern the missing release path needs. 9401 lines
of bespoke Rust become ~4000 lines of shared Python plus the submodule work.

**But step 1 is a genuine go/no-go and is not pre-judged here.** Submodule
support is the one place the bespoke tool is clearly ahead, dev-hermit's slots are
submodule-bearing by construction, and `wrkslots` currently opts out with
`--no-recurse-submodules`. If that spike says redesign, the correct plan is the
opposite one: keep the Rust tool, add the absence-proof release path beside the
quarantine site under the A1–A8 tests above, and revisit in a quarter.

## Verification and limits

Read-only inspection of both tools' source, the registry, and the filesystem. No
tool was executed against shared state; nothing was implemented. Slot counts,
drift figures and mtimes are as of 2026-08-24 and will move. The `--repair` drift
figures (130/152 rows, 213/456 cells) come from the allocator's own report during
an ordinary allocation, not from a dedicated audit.
