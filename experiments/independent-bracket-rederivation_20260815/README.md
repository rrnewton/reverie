# Independent re-derivation of two fixture brackets

**Profile: portable lane only. Not a receipt.** Every result here comes from single-cell runs —
`hermit run --strict --verify` directly, or `ci/test_harness.sh run --probe-disabled` for one cell —
all on portable-lane cells, which carry the `--no-virtualize-cpuid --max-timeslice=disabled` profile.
No full or privileged profile was invoked, and none could succeed on this host: devbig030 lacks CPUID
faulting, so full profile fails structurally here (35 full runs since reboot, 16 died on CPUID, zero
passed). A portable-lane pass is **not** a qualifying receipt — `qualifying_receipt` requires
result=pass and `reconcile_receipts` requires full+pass+checks==5, which route to devbig014.

Run 2026-08-15 by `hermit-kvm2` on `devbig030`, against hermit worktree `e18acde58`
(`origin/main` was `302a1a9c0` at the time). The pre-fix sources are taken from `302a1a9c0`.

## Why this exists

`hermit-teardown` found both defects and fixed both, so its own verification is the weakest
evidence available for them — and it said so unprompted, asking in both task notes for someone
else to re-derive the results rather than trust its re-run. That request went unfilled all
session. **I derived none of the original work.** Everything in `results.tsv` is my own observed
result, not a confirmation that theirs looks right.

The original evidence is host-local under `ignored/` — it survives context loss but not host loss,
and the machine that owns the PRs cannot see it. That is the second reason this is tracked.

## Method

All runs under Hermit, never natively: `hermit --log=info run --strict --verify -- <guest>`.
Every guest recompiled from source by me with the harness's exact command for these cells,
`cc -std=c11 -O2 -g -Wall -Wextra -Werror` (the manifest declares no `build.cflags` for any of
the three) — their prebuilt binaries were not reused.

Each mutation is checked in three arms, because the first two alone cannot distinguish a working
gate from a mutation that never applied:

1. **accept** — the fixed base passes.
2. **refuse** — the mutant fails, *and the guest names the specific invariant it violated*.
3. **control** — the same mutation applied to the **pre-fix** source, which must **pass** while
   printing a visibly wrong value. This is what grades the zeros: it proves the mutation took
   effect and that the refusal in arm 2 comes from the fix rather than from anything incidental.

## Result: 7 of 7 fixture mutations re-derived, plus the 4-cell umask bracket

All 21 rows are in `results.tsv`. Summary:

- **7/7 caught by the fix**, each naming a *different* invariant (`counter`, `checksum` ×2,
  `own_offset_kept`, `sem`, `bytes`, `pos`) with observed-vs-wanted — so they discriminate
  individually rather than one blanket check firing.
- **7/7 controls exit 0 on the pre-fix source** while printing the wrong value (37, 32641, 6,
  255, 51, `own_offset_kept=0`). The pre-fix fixtures end `return 0;` unconditionally, so a value
  mutation is uncatchable there by construction.
- **umask**: `baseline_setupfail` exits **0** and Hermit reports
  `:: Success: deterministic. Determinism verified.` — a full green from a run whose setup failed
  and which tested nothing, exactly as the fix's own comment predicts, because both `--verify`
  runs fail setup identically so the comparison agrees. `fixed_setupfail` exits 1 with
  `mkdtemp: No such file or directory`.

Both brackets hold. Nothing in the recorded results was contradicted.

## One thing that nearly produced a false result, recorded because it is the trap this guards

My first pass used `--log=error` and got **rc=1 on all ten runs**. That reads as "all 7 mutants
caught" and would have satisfied the refuse criterion while proving nothing. The cause was mine:
`--verify requires --log=info or a more verbose level`. What exposed it was the **accept** arm —
the fixed bases were failing too, and they must pass. A bracket that only checks the refuse
direction cannot detect this class of error at all.

## Scope

This is about the brackets, not about landing. Both PRs are receipt-blocked, and neither meets a
post-facto-human-review trigger — C fixtures only, no syscall support, no Reverie API change, no
determinization strategy, no DetCore scheduling change.

`prefix-mutants/` holds the pre-fix sources and the seven pre-fix mutants I generated for arm 3.
