# `detcore_misc` co-run flake, re-measured at the current reverie pin (2026-08-11)

**Date:** 2026-08-11 · **Host:** devbig014 (316 cores, 754 GiB) · **Agent:** codex-2 (a Claude agent)
**Task:** `measure-detcore-misc-corun-flake-at-current-reverie-pin`
**Follows:** [`multisect_detcore_misc_20260803`](../multisect_detcore_misc_20260803/README.md), whose
harness design this reuses.

## Question

`test.detcore_misc`'s `vfork::vfork_parent_resumes_after_child_exec` was the one node with
evidence-backed need for serialization: 16–23 % FLAKY at C=32 on 2026-08-03, a reverie
safeptrace notifier ESRCH hot-spin. `hermit/ci/dag/portable.json` still says in a node
description that "detcore_misc currently livelocks under load (reverie#355 is the established
fix)". Is that still true at the pin hermit builds today?

This gates the `hermit_guest: 1` / box-exclusivity decision: if this node is safe, no node in
the 56-node DAG has measured evidence that it must be excluded from cross-run concurrency.

## Subject and provenance

| | |
|---|---|
| hermit | `2b6005cf3696c0351a4183ea713f469d2b50abec` (main) |
| reverie **as built** | `349460925ee56f2aca686a3392b534e8861ba375` (`detcore/Cargo.toml` `rev=`, **not** the submodule gitlink) |
| build | `cargo test -p hermit-detcore --test tests_misc --no-run` in registered slot `w18`, 21.78 s warm |
| binary md5 | `bc14e40d79e95eac56e1e4855ddf896d` |

Provenance was verified, not assumed: the binary embeds
`checkouts/reverie-2fc770f7a9c80803/3494609`, and its md5 differs from both controls. Note the
package was renamed `detcore` → `hermit-detcore` since 2026-08-03, so the sibling experiment's
`build-seq.sh` no longer builds as shipped.

## Method

`matched-pgsafe.sh` — the sibling experiment's matched-load co-scheduling design, unchanged in
every measurable respect (same test, `--exact --test-threads=1`, round-robin interleave so all
labels share one instantaneous load window, trinary PASS/FAIL/FLAKY, calibrator-gated wave
validity, exit 124 = the 20 s `timeout` fired = a hang).

**One deviation, and it is a safety fix, not a design change.** `matched.sh` reaps strays with
`pkill -9 -x "$(basename $BIN)"`. Hard Invariant 15 forbids name-matched kills here: `comm` is
truncated to 15 chars and a concurrent validate's own `test.detcore_misc` node runs a binary
also called `tests_misc-<hash>`, so that `pkill` can kill a sibling agent's live work. Each
instance is instead launched into its own process group (bash monitor mode; `$!` is the group
leader) and only those recorded negative pgids are signalled.

**Three labels, so the bracket fires in both directions** — the original had only the positive
control:

- `cur` — the subject, above.
- `zhead` — `0ac1c1d7`, recorded FLAKY at 16.3 % (52/320). Positive control: a wave counts
  **only** if this hangs at least once. A wave where it stays clean was under-powered and
  proves nothing.
- `aclean` — `21e91ff3`, recorded CLEAN at 0/320. Negative control: proves the probe is not
  simply timing everything out.

Both control binaries survive in the sibling experiment's gitignored `bins/`, so no rebuild of
historical commits was needed.

## Results — pooled over 36 valid waves, 1632 samples per label

| label | hangs / samples | rate | 95 % CI (Clopper–Pearson) |
|---|---|---|---|
| `cur` | 1 / 1632 | **0.061 %** | [0.0016 %, 0.341 %] |
| `zhead` (known-bad) | 65 / 1632 | 3.983 % | [3.087 %, 5.049 %] |
| `aclean` (known-clean) | 1 / 1632 | **0.061 %** | [0.0016 %, 0.341 %] |

`cur` vs `zhead`: Fisher two-sided **p = 9.7e-19**. `cur` vs `aclean`: **p = 1.000**.

By concurrency:

| batch | valid waves | discarded | `cur` | `zhead` | `aclean` |
|---|---|---|---|---|---|
| C=32 | 21 | 9 | 1/672 (0.149 %) | 30/672 (4.46 %) | 0/672 |
| C=64 | 15 | 0 | 0/960 (0.000 %) | 35/960 (3.65 %) | 1/960 (0.104 %) |

All 4896 instances exited either 0 or 124. **Zero** non-zero non-124 exits, so every failure is
a hang, not an assertion.

## Findings

1. **The current pin has returned to the CLEAN baseline — not to zero.** `cur` and the commit
   the multisect classified as CLEAN produced the same single hang in the same waves, ~65×
   below the FLAKY commit. The historical CLEAN band already contained a 0.63 % commit
   (`d495b602`, 2/320), so 0.061 % sits comfortably inside it.
2. **No evidence the residual is concurrency-driven.** Doubling C from 32 to 64 took `cur` to
   zero while the calibrator stayed high at both. The single `cur` hang and the single `aclean`
   hang look like one shared low background rate — consistent with the multisect's own
   "latent since 2021 at ~0 %".
3. **reverie PR #355 (`820b2b64`) is still NOT an ancestor of `3494609`.** Re-checked. The
   improvement came from something else — `reverie-ptrace/src/tracer.rs` has been substantially
   rewritten and now commits the reservation *after* storing child cleanup ownership, which is
   the ordering the original root-cause blamed. **This experiment measured the outcome, not the
   mechanism**; #355 or a successor should still land, because a latent 1-in-1632 hang is not a
   fixed one.

## Power — why the absolute rate is an under-estimate

The calibrator returned 3.98 % where the *same binary* was recorded at 16.3 %, because ambient
fleet load was 24–54 on 316 cores against the original's 286–590. This probe therefore has
roughly a quarter of the original's detection power. **The load-matched comparison against two
controls in the same waves is the result to rely on, not the absolute rate.**

## Verdict for the exclusivity decision

`test.detcore_misc` is no longer in the flaky regime and, on this evidence, is safe to co-run.
The measured condition (32–64 concurrent instances of the exact hanging test) over-stresses the
operational condition (**two** — one node per concurrent validate) by 16–32×. That lower bound
is an argument from the direction of the amplifier, not a measurement: a C=2 wave cannot power
the calibrator, so every wave would be discarded as invalid and the design cannot measure there.

This **retracts** the "one node genuinely requires exclusion" finding of
`p0-is-validate-box-exclusivity-a-hardware-requirement-or-a-default`. Still open: `kvm` (2
nodes, 124.4 s) is unmeasured rather than cleared, and `privileged-e2e.manifest_applications`
at 67.3 % of its 130 s budget (1.5× headroom) is now the leading blocker — a budget defect, not
a hardware requirement.

## Reproduce

```
# controls come from the sibling experiment's gitignored bins/
cd experiments/multisect_detcore_misc_20260803
../detcore_misc_corun_remeasure_20260811/matched-pgsafe.sh 32 20 25 \
    cur:ignored/bins/<sha> zhead:ignored/bins/head aclean:ignored/bins/21e91ff3
```

Raw per-wave exit-code files: `multisect_detcore_misc_20260803/ignored/matched-pgsafe/<stamp>/`
(gitignored). Per-wave counts are in `results.csv`; `metadata.json` has exact SHAs.
