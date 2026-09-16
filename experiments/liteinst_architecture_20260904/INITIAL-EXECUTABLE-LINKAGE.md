# Frozen 340-cell initial-executable linkage inventory

This is **file inspection, not execution or backend coverage**. The population
is unchanged: all 340 frozen ptrace/verify green **labels**, not just the 253
cached evidence rows. Static deferrals remain in the denominator. No builds,
guest runs, product edits or new test outcomes were produced.

| Initial executable classification | Cells / full population |
| --- | --- |
| Dynamic ELF, PT_INTERP observed | 216 / 340 |
| Static ELF, no PT_INTERP or DT_NEEDED observed | 4 / 340 |
| Script with inspected shebang | 50 / 340 |
| Unknown/unavailable for frozen source | 70 / 340 |
| Total | 340 / 340 |

270 rows resolve to 268 distinct files/hashes; three direct-shell cells share
the same host Bash binary. The 50 scripts are 48 `#!/usr/bin/env bash` wrappers
and two `#!/usr/bin/python3` programs. Env, Bash and Python interpreter binaries
were also inspected and are dynamic ELF **on this host**. This does not attest
the corresponding binaries inside a historical validation image. For env-based
scripts the displayed Bash resolution uses explicit host lookup
`PATH=/usr/bin:/bin`. Script descendants, including compiled guests subsequently
exec'd by wrappers, are not inventoried or presumed dynamic.

All four prior static declarations now have inspected ELF evidence:
- `backend-parity-c/static-nolibc-syscall-sites`
- `c-programs/hello-nostdlib`
- `c-programs/pread64-nostdlib`
- `c-programs/racewrite-nostdlib`

The 70 unknowns are **68 unavailable matching compiled artifacts** and **two
historical source mismatches** (`c-programs/arch-prctl-determinism` and
`c-programs/writev-determinism`). Older artifacts are not silently credited
to the changed frozen sources. By manifest: 64 c-programs, four
determinism-stress-c, one chaos-c and one system-utils. See per-row missing
paths and old/new source hashes; no unbounded search or build was attempted.

## 306/340 feasibility bound, not a success rate

Deferring the four proven static initial executables leaves a best-case
initial-format ceiling of **336/340**, so known static rows alone do not make
306 impossible. The 216 dynamic ELF plus 50 scripts give 266 known non-static
initial file formats; **at least 40 of the 70 unknowns** must remain eligible
to reach 306, even if all 266 otherwise work. Equivalently, at most 30 unknowns
can additionally be static/deferred. Treating all unknowns as static would
reduce the ceiling to 266/340.

These are conditional linkage-only bounds. They do not prove runtime support,
successful script-interpreter loading in the intended environment, or support
for any exec descendant. Syscalls, preemption, logging, lifecycle and the full
strict verification envelope remain independent constraints. There is no new
dynamic-only backend numerator, L2 result or in-process 90-percent claim.

## Exact inputs and bounded mapping

Frozen Hermit `4d8f866102882b6eeabdf99cc7e81433cc3c95c5`; frozen identity-list
SHA256 `2cf9960cbe319f9214023d748482a6986e47f5822f728254c96ddfb7248bc530`.
`baseline.json`, `baseline.tsv`, `population-identities.txt`, `sample.json` and
`paired-execution.json` remain byte-identical. Main inspection interval:
2026-09-04T23:28:19Z–23:28:44Z; interpreter and unknown-detail checks followed.

Candidates are bounded to:
1. The three exact paired-run fixture binaries already recorded in
   `paired-execution.json` (preferred for those identities).
2. The baseline's explicitly named historical run
   `validate-current-tip-full-validate-07c10eb80ac4-1788143718039901059-4036218-e4cf26b8`
   under parent `ignored/validate/artifacts/`, source
   `07c10eb80ac4fafbbe65134b504d77ef7e388d25`. Portable result records yield
   252 distinct frozen identities after joining the three paired records.
   Recorded container `/results/` paths map to that run's retained `e2e/`
   tree. Actual files are inspected now; no historical binary immutability or
   execution attestation is inferred from their path.
3. That same run's compile inventory, only the exact
   `audit-compile/<test-id with slash replaced by hyphen>/fixtures/program`
   path. Pinned harness source explicitly defines this mapping. The C main
   source SHA256 and declared build recipe must match the frozen manifest.
4. Pinned script/direct-command definitions. Source scripts are accepted only
   when the existing worktree file matches the frozen Git blob. Direct-shell
   definitions start Bash according to the pinned runner, not the script named
   later in the shell command. Explicit `/usr/bin/du` and `/usr/bin/find` and
   this host's Bash are inspected as current host binaries, not pinned-image
   artifacts; their package/source SHAs are unknown.

Of the resolved rows: 198 use exact result mappings with matching source hashes,
17 use matching source/recipe compile inventory, 50 use pinned script/direct
source files, and five use the explicit host command resolution above. Source
matching is a main-file/recipe check, not full compiler/header closure or a
signed build attestation. Per-row historical run/source and exact observed
binary path/hash/mtime remain explicit.

## Recomputing and inspecting the evidence

Durable rows: `initial-executable-linkage.json` and
`initial-executable-linkage.tsv`. Each row preserves full cell identity,
classification, exact observed path/hash, source provenance, interpreter,
unknown reason, and the statement that exec descendants are unexamined.

Raw root: `/home/newton/work/dev-hermit/ignored/liteinst-linkage-inventory-20260904/`.
It contains selected result records, both pinned manifest command inventories,
mapping-rule source excerpts, interpreter records and individual `file -bL`
and `readelf -h -l -d` output under `inspections/`. SHA256 manifests anchor
inputs and inspected executables. Existing `git show`, `yq`, `jq`, shell,
`file`, `readelf` and `sha256sum` perform the audit; no script build is required.

Classification rule: an inspected ELF with PT_INTERP is dynamic; an executable
ET_EXEC/ET_DYN ELF without PT_INTERP and DT_NEEDED is static; `#!` identifies a
script whose literal shebang is retained. Other/unavailable or mismatched
artifacts remain unknown. Never infer linkage from language, source suffix,
absence of `-static`, a launcher name or historical result status.

Recompute cell counts with:
`jq '.rows | group_by(.classification) | map({kind:.[0].classification,cells:length})' initial-executable-linkage.json`.
Identity count, uniqueness and byte-equality to the frozen list are separately
checked. No baseline/matrix label or task lifecycle mutation. Fallback
`7b51e521fcacbbe876c93d1cd0893c442c1a52f6` stays clean and unchanged.
