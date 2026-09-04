# Results

## The finding: leaves 0x04 and 0x0b now disagree across backends

`subleaf_probe.c` issues `__cpuid_count` for subleaves 0 and 1 of the four basic
leaves that hermit's table treats as selected by ECX and that hold a non-zero
subleaf-0 row, plus leaf `0x0d`. Only the `sub=1` rows matter; `sub=0` is printed
so a reader can see what the aliased answer was aliasing to.

Raw output is in `logs/`. Subleaf 1 only, side by side:

| leaf | base `4d8f8661`, ptrace | head `c3a85da2`, ptrace | head `c3a85da2`, KVM |
|---|---|---|---|
| `0x04` | `00000120 01c0003f 0000003f 00000001` | `00000000 00000000 00000000 00000000` | `00000120 01c0003f 0000003f 00000001` |
| `0x07` | `00000000 00180fb9 00000000 00000000` | `00000000 00000000 00000000 00000000` | `00000000 00000000 00000000 00000000` |
| `0x0b` | `00000000 00000001 00000100 00000001` | `00000000 00000000 00000000 00000000` | `00000000 00000001 00000100 00000001` |
| `0x0d` | all zero | all zero | all zero |

Read the table by column pairs, not by row:

- At the base, ptrace and KVM **agreed** at leaves `0x04` and `0x0b` (both
  repeated the subleaf-0 row) and **disagreed** at leaf `0x07`.
- At the head that is inverted. Leaf `0x07` agrees; leaves `0x04` and `0x0b`
  disagree. **The disagreement at those two leaves is created by this diff.**

Leaf `0x0d` is unchanged everywhere on the ptrace side because hermit's
`CPUIDS[0x0d]` row is already four zeros, so aliasing it and zeroing it give the
same answer. Under KVM leaf `0x0d` subleaf 0 is `00000007 00000340 00000340
00000000` (see the raw log), which is a separate pre-existing difference from
ptrace and is not what this finding is about.

## Why the disagreement matters beyond tidiness

Leaf `0x0b` is the extended topology enumeration. A guest walks subleaves until
the level type in `ecx[15:8]` reaches zero. Under KVM, subleaf 1 returns
`ecx=0x00000100`, which is level type 1, and so does every subleaf after it:
**that walk does not terminate under the KVM backend.** After this diff it does
terminate under ptrace. The diff removes the condition from one backend of three
and leaves it on the other two, undocumented.

## Where the third table is

Three separate deterministic CPUID tables exist. The diff changes one.

| table | reached by | subleaf handling |
|---|---|---|
| `detcore/src/cpuid.rs` | ptrace, LiteInst, e9patch-under-ptrace | after the diff: zero for a non-zero subleaf on leaves `0x04 0x07 0x0b 0x0d 0x0f 0x10 0x12 0x14` |
| `reverie-kvm/src/cpuid.rs` | KVM | only the leaf `0xd` rows carry `KVM_CPUID_FLAG_SIGNIFCANT_INDEX`; the leaf `0x04` and `0x0b` rows are built by `cpuid_entry()` with `flags` zero, so KVM ignores the index and returns the first row matching the function |
| `reverie-dbt/native/client.c:1189-1210` | DBT | `if (leaf == 7) { if (subleaf != 0) return zeros; }` and nothing else |

`reverie-dbt/native/client.c:168` says "Keep this synthetic CPU identity aligned
with Hermit's ptrace backend." That sentence is false after this diff.

The DBT column is read from source, not executed. Building the DBT backend needs
DynamoRIO and was out of proportion to the question; the code is four lines and
unambiguous, but it is a source reading and is labelled as one here.

## The fixture probes the one leaf that cannot fail

`tests/backend-parity/fixtures/cpuid_probe.c` gained exactly one new assertion:
leaf 7 subleaf 1 reads as four zeros. Of the three ECX-selected leaves with a
non-zero subleaf-0 row (`0x04`, `0x07`, `0x0b`), that is the only one where all
three backends already agreed — DBT special-cases leaf 7 already, and the KVM
table has no leaf 7 entry at all. So the new backend-parity coverage is precisely
the subset incapable of diverging.

## Supporting measurements

All at `c3a85da2` in slot `review-cpuid` unless stated.

| measurement | result | log |
|---|---|---|
| `cargo test -p hermit-detcore --lib` | 644 passed, 0 failed | `logs/unit-detcore-lib-644.log` |
| `test-harness run --test backend-parity-c/cpuid-probe --mode verify --backend ptrace` | PASS | `logs/cell-head-verify-ptrace.log` |
| same cell, `--backend kvm` | PASS | `logs/cell-head-verify-kvm.log` |
| same ptrace cell, lookup reverted to leaf-only | FAIL on both attempts, exit 125 | `logs/cell-lookup-reverted-verify-ptrace.log` |
| `indexed_leaves_do_not_alias_subleaf_zero`, lookup reverted | fails at `detcore/src/cpuid.rs:114`, `left: [0, 1576889, 0, 0]` — `1576889` is `0x00180fb9`, leaf 7 subleaf 0's EBX | `logs/unit-lookup-reverted.log` |
| same test narrowed to `[0x0d, 0x0f, 0x10, 0x12, 0x14]` with the leaf-7 assertions removed, lookup reverted | **passes** | `logs/unit-narrowed-to-all-zero-leaves.log` |

The last row is a coverage measurement worth keeping: those five leaves have
all-zero subleaf-0 rows, so they assert nothing. Three of the test's eight loop
leaves — `0x04`, `0x07`, `0x0b` — carry the check. A later reader should not
count eight.

## What these measurements do not establish

- One host, one run per leg. No repeat count, no L2 or L3 claim, no
  `--verify-strict --verify-json` with `bitwise_parity: true`, no stress.
- The DBT column is source-read, not executed.
- Nothing here bears on reverie PR 521, which is approved on different evidence.
- The four leaves `0x0f 0x10 0x12 0x14` in `uses_subleaf` are inert today: their
  `CPUIDS` rows are already all-zero, so listing them changes no observable
  value.
