# Qualification of 12 unworked ptrace+replay candidate cells

**Task** `qualify-more-non-privileged-cells` · agent `hermit-observable` · 2026-08-16 · devbig030.

**NOT A RECEIPT.** Read-only on the product: no manifest touched, nothing enabled, no product
file modified. Fixture sources read by `git show <SHA>:<path>` with a hard assert on a non-empty
read; mutants compiled into this gitignored scratch dir.

## Scope, stated on every verdict
Portable lane · devbig030 · **backend `ptrace`** · **mode `replay`** · hermit main
`302a1a9c0fde564db0292d1ea5cabc91343e79bc`.
Instrument: `hermit/target/release/hermit`, `hermit 0.2.0 (2026-08-16, g302a1a9c0fde)`,
sha256 `1ec7968570ff9fc033c23b3ee61b01ff8097d8a795e59e10d1ae68d88436bec8`.

**NO PRIVILEGED CPUID TEST WAS RUN.** Three candidates were excluded by source screen for
CPUID/rdrand dependence and never executed, per the owner directive.

## The two measurements, kept separate
- **OBSERVABLE** (where the bite is proven, never `.text`):
  `hermit --log=error run --backend ptrace --strict --no-virtualize-cpuid --max-timeslice=disabled -- <guest>`
  — passes the guest's own stdout AND stderr through.
- **GATE** (what the verdict reads): the exact replay command from
  `scripts/manifest-to-commands.rs:269-271`:
  `hermit --log=info --backend ptrace record start --strict --verify --data-dir <d> --record-timeout <t> -- <guest>`
  **Exit status only.**

**COMPARATOR NAMED:** the DETLOG/COMMIT-subset comparator (bare `log-diff` policy,
`strip_lines=false`) in `detcore/src/logdiff.rs`. Observed running on **both** arms
(accept 76|76 DETLOG, refuse 70|70, both `:: Success: replay matched recording.`) — the refuse
arm still exits non-zero, so the exit status is propagated **past a passing comparator**.

## Method
Two independent mutation families per cell, each anchored on a string asserted to occur
**exactly once** (`count == 1`), never first-match:
- **M1 behaviour** — issue a permitted syscall at the call site, so the contracted refusal is
  never exercised.
- **M2 oracle** — issue the *same* call and change only the demanded errno, so the mutant's own
  diagnostic reports the genuine refusal it received.

Build preflight runs before any measurement. **The accept arm is run for every cell.**
Two full passes of all 12 cells: **0 disagreements**.

## Result
5 QUALIFIED-PTRACE · 6 NOT-EXIT-GATED · 1 accept arm does not exercise its assertion.

**The instrument produced both answers in the same pass, from the same code path** — so a uniform
result is not a stuck instrument. The six NOT-EXIT-GATED are additionally proven *structurally*:
`main()` in each returns only `0`.

## Files
- `results.tsv` — one row per cell, generated from the captures, not hand-written
- `results-pass1.json`, `results-pass2.json` — full captures, both passes
- `qualify.py` — the driver
- `src/execveat_probe.c` — the discriminating probe that withdrew one verdict
