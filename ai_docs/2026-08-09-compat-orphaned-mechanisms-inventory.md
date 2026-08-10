# Retiring M2 (liteinst) and M3 (dbi) into the shared corpus: inventory and the two numbers that change the plan

**Transient report.** 2026-08-09, `claude-reviewer-2`, task `compat_retire_the_liteinst`.
Measured at hermit `b5a4748e87a6bc32bc3d87935ab571c1c57c25ad` in `worktrees/compat-retire`.

**Status: the retirement itself is BLOCKED and should stay blocked.** Its dependency
`compat_one_backend_agnostic` is `IN_PROGRESS` and is still designing the row schema — as of
its 2026-08-10 01:41 note it has an open question from `compat_re_attempt_kvm` about canonical
argv selection. Porting rows into today's `ci/compat/corpus-{strict,sabre,e9patch,rr}.json`
would mean **writing 76 new rows into the four per-backend JSONs that the P0 task exists to
delete** — adding to per-backend structure, the exact inverse of "backend-agnostic in
structure," and thrown away on migration.

What follows is the inventory that unblocks it, plus two measurements that change what the
retirement should actually do.

**Baseline agrees with the P0 task.** Corpus rows: strict 191, sabre 212, e9patch 172, rr 174;
union **214 distinct labels**. Same numbers `compat_one_backend_agnostic` cites, so the keys
below are comparable to its artifact.

---

## The headline: M2 and M3 are opposite problems and must not get the same treatment

| | M2 `liteinst_advanced.rs` | M3 `collect-dbi-corpus.rs` |
|---|---|---|
| population | 76 guest binaries | 314 manifest test ids |
| overlap with the 214 | **74 of 76** | **0 of 314** |
| identity scheme | bare label (`awk`) — same as M1 | namespaced (`backend-parity-c/aio-refusal`) |
| what it is | a **duplicate** mechanism | a **disjoint corpus** |
| denominator effect of folding | 214 → **216** | 214 → **528**, 78% hand-written C |
| right action | retire by deletion + enable liteinst on existing rows | make tracked, keep as a **separate bucket** |

### M2 is a duplicate, not an orphan — this makes the work much cheaper

76 distinct guest binaries confirmed. **74 already exist as labels in the 214-label union.
Exactly two are new: `sh` and `tail`.**

So the liteinst column is not "76 programs outside the corpus." It is 74 programs *inside*
the corpus being exercised by a second mechanism, plus 2 missing labels. Retiring M2 moves the
denominator **214 → 216**, not 214 → 290. Most of the retirement is deletion.

**But same label is not the same cell, and this is the number the schema owner needs.**
Of the 74 overlapping labels, **62 invoke the program with argv matching no M1 row.** Only 6
agree; 6 more my parser could not capture and I count those separately rather than as
agreement.

```
label     M2 argv                      M1 argv
cat       (none)                       /bin/cat README.md
base64    --wrap=0                     /usr/bin/base64 README.md
bzip2     -c                           bash -c 'bzip2 -c README.md | sha256sum'
basename  /tmp/hermit-example.txt .txt /usr/bin/basename /usr/local/bin/hermit
```

`compat_re_attempt_kvm` has already asked `compat_one_backend_agnostic` for a canonical argv
policy covering **three** labels (`bc`, `lua`, `perl`). Folding M2 in raises that to **as many
as 62**. That is the "no shared cell identity" defect named in the task, quantified.

**Note the direction, because it points at the answer.** M2's invocations are consistently
*weaker* than M1's — a bare invocation versus a pipeline with a diff or hash oracle. So the
resolution is probably **not** to add 62 argv variants; it is to drop M2's argv and let the
existing, stronger M1 row own the cell. That is the corpus owner's call. I am supplying the
count, not making the decision.

### M3 is a disjoint population — folding it in would inflict the very distortion this fixes

M3's population is the e2e manifest: **314 distinct test ids across 13 TOML manifests**, and
its **intersection with the 214 real-program labels is zero**. It uses a different identity
scheme entirely.

**244 of the 314 (78%) are hand-written C micro-tests** — `c-programs.toml` (159) and
`backend-parity-c.toml` (85). That is the category the owner explicitly deprioritised.

Merging M3 into the real-program corpus would take the denominator from 214 to 528 and make it
78% hand-written C. **Every backend percentage would move for reasons unrelated to
capability** — the same ratio distortion this task exists to remove, applied in the opposite
direction. The owner's correction about ptrace's `36/217` was precisely about denominators that
misrepresent; a 528-row denominator dominated by C micro-tests would misrepresent harder.

M3 should become **tracked but separate**. Making it tracked is cheap and needs no rewrite:
`--csv` is already a parameter (`collect-dbi-corpus.rs:39,43`); only the *default* destination
is `ignored/`, which is gitignored at `.gitignore:120`. **A destination change, not a code
change.**

---

## Front-door classification of M2's tests

The dispatch asked that where retiring a test means rewriting it as a front-door test, I say so
rather than porting verbatim. M2 has **23** `#[test]` functions, not 24 as the task states, with
no `#[ignore]` and no `#[cfg]` gating.

**19 are front-door shaped** — they run a guest through `run_liteinst_strict_verify` and assert
on guest output. These become corpus rows and carry no liteinst-specific logic:
the 17 `*_utilities` / `micro_suite` groups, plus `liteinst_strict_verify_python_random_example`
(runs `examples/rand.py`) and `liteinst_strict_verify_heap_growth_avoids_trampoline_mappings`
(runs `mmap_guest()`; its *name* is liteinst-internal but its assertion is a stdout marker).

**4 are backend-mechanism tests and are not corpus cells at all** — they test liteinst's
machinery, not "does backend X run program P":

- `liteinst_preload_is_inert_without_host_runtime_selector`
- `liteinst_thread_clone_fails_closed_without_sigsys`
- `liteinst_fork_fails_closed_without_hanging`
- `liteinst_abnormal_exit_after_registration_does_not_hang`

These must stay backend-specific. Critically, **they must not be counted in the compat
denominator in either direction** — neither folded in as cells nor recorded as NOT_ATTEMPTED
for other backends, because they are not questions about other backends.

**A third guest population exists and needs an identity decision.** `examples/rand.py` and
`mmap_guest()` are repo-local guests — neither `/usr/bin` programs (M1's scheme) nor manifest
ids (M3's scheme). That is a fourth identity scheme hiding inside the second mechanism.

---

## The four states, as they apply here

For the 74 labels M2 already covers, liteinst's cell state comes from the product, per the P0
task's own rule — `unavailable_reason(b) => NOT_ATTEMPTED(reason)`, else run.

Two cases need an explicit state rather than silence:

- **`sh` and `tail`** — new labels. Until they have rows they are `NOT_ATTEMPTED` for *every*
  backend, reason "label absent from corpus (arrived with liteinst M2 retirement)". They should
  not be quietly added to liteinst alone; that would recreate a per-backend population.
- **The 62 argv-divergent labels** — if the corpus adopts M1's argv and drops M2's, then
  liteinst's *previously passing* invocation stops being represented. That is a real coverage
  change and should be recorded as such, not absorbed silently. If instead both argv are kept,
  they are two cells and the denominator is 214 + 62, not 216.

---

## The unbounded-timeout defect: confirmed, and sharper than filed

The **shared** run path is unbounded — `liteinst_advanced.rs:193` `command.output()` and `:209`
`wait_with_output()`, no deadline, used by all 19 corpus-style tests.

But **two tests hand-roll their own deadlines** at `:788` and `:904`
(`Duration::from_secs(10)` and `(5)`). The hang was known and patched locally twice, never
centrally.

Meanwhile **M3 already solved this class properly.** `collect-dbi-corpus.rs:228-235` redirects
child output to *files, not pipes*, with the reason in a comment: hermit forks a supervisor that
reparents to init and outlives the `timeout`-killed foreground, and if you read from a pipe that
survivor holds it open and `Command::output()` blocks forever. It then SIGKILLs the child's
process group to reap the survivor.

**The retirement should adopt M3's containment and must not carry M2's.** Per-row wall and CPU
budgets alone are not sufficient if the harness reads from a pipe — the budget expires, the
survivor keeps the pipe open, and the read still blocks. That is a design constraint on the
corpus runner, not a detail of the port.

---

## Recommended sequence

1. **Do not port anything until `compat_one_backend_agnostic` publishes the row schema and the
   argv policy.** The 62-label argv divergence above is an input to that decision.
2. **Retire M2 by deletion, not translation.** 74 labels already exist; add `sh` and `tail`;
   delete the 19 front-door tests once the corpus covers liteinst; keep the 4 mechanism tests.
3. **Make M3 tracked in place** — change the default `--csv` destination out of `ignored/`.
   One line, independent of the schema work, and it can be done today.
4. **Keep M3 a separate bucket** with its own denominator. Report real programs and C
   micro-tests side by side, never summed into one ratio.
5. **Give the corpus runner M3's process containment** before it inherits M2's workload.

## Limits

- The 62/6/6 argv split comes from regex extraction over Rust source; the 6 uncaptured are
  counted as uncaptured, not as agreement. Exact figures should be regenerated from a parsed
  AST if they are going to gate a schema decision.
- The 314 manifest ids are harvested by regex over 13 TOML files and include any `name =` key,
  so that figure is an upper bound on guest count. The *zero intersection* with the 214 does not
  depend on it being exact — the two schemes share no strings at all.
- I ran no guests and produced no cell states. This is inventory, not measurement of outcomes.
