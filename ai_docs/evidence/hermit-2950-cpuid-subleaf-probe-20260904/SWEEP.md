# Complete leaf and subleaf enumeration

`subleaf_probe.c` answers one question and stops. This sweep answers "is that the
only one", which is a different question and was asked after the fact. It is
recorded separately rather than folded in, because the first probe's narrowness
is itself part of the record: the finding was reached by looking at four leaves at
two subleaves, and the sweep is what makes the claim "there is no third sibling"
sayable.

## Method

`subleaf_sweep.c` reads basic leaves `0x00`–`0x14`, extended leaves
`0x80000000`–`0x8000000a`, and `0x15`, `0x1f`, `0x8000000b` to reach past both
advertised maxima, at subleaves 0, 1, 2, 3, 17, 18, 19 and `0xffffffff`. 280 rows
per run. Three runs: ptrace at the base, ptrace at the head, KVM at the head. Each
log names the sha256 of the binary that produced it — `458329500c4b…` is the
base-reverted build, `05c7db82a852…` the head build.

Comparing the base-versus-KVM and head-versus-KVM disagreement sets separates
what the diff created from what was already there.

## Created by hermit PR 2950 — 14 rows, 2 leaves, and the set is closed

| leaf | disagreeing subleaves |
|---|---|
| `0x04` | 1, 2, 3, 17, 18, 19, `0xffffffff` |
| `0x0b` | 1, 2, 3, 17, 18, 19, `0xffffffff` |

**Every non-zero subleaf, not subleaf 1.** This is what makes "add a terminating
row at subleaf 1" not a fix on the KVM side: without
`KVM_CPUID_FLAG_SIGNIFCANT_INDEX` on the **subleaf-0** row, KVM returns the first
row whose function matches and never reads the index, so an added row at any
other index is unreachable. Setting the flag on the existing subleaf-0 rows is
the fix, and then no extra rows are needed.

## Fixed by hermit PR 2950 — 7 rows, 1 leaf

Leaf `0x07`, the same seven subleaves: ptrace and KVM newly agree.

## Pre-existing, untouched — 39 rows, 14 leaves, three groups

1. **Any leaf above the advertised maximum**: `0x0e`, `0x0f`, `0x10`, `0x11`,
   `0x12`, `0x13`, `0x14`, `0x15`, `0x1f`, `0x8000000b`, at subleaves 0 and 2.
   KVM returns leaf `0x0d`'s row at the requested index, which is Intel's
   documented out-of-range behaviour; hermit's table returns zeros. Ten leaves,
   one cause. Neither repository appears to record which answer is intended.
2. **Leaf `0x01` at every subleaf**: ECX `90b82201` on ptrace, `1c982201` on KVM.
   KVM advertises XSAVE and OSXSAVE (bits 26 and 27) and drops bits 21 and 31.
   Deliberate, and the reason its leaf `0xd` rows carry real values.
3. **Leaf `0x07` subleaf 0**: EBX `00180fb9` on ptrace, zero on KVM, which has no
   leaf 7 entry at all. hermit's table advertises HLE and RTM (bits 4 and 11)
   that both other paths mask.

## One further defect, found by decoding rather than diffing

The processor brand string, from leaves `0x80000002`–`0x80000004`:

```
ptrace : "QEMU Virtual CPU version 2.5+"
KVM    : "QEMU Virtual PCU version 2.5+"
```

`reverie-kvm/src/cpuid.rs` `DETERMINISTIC_EXTENDED_CPUIDS` row 2 ends
`0x5543_5020` where hermit's `EXTENDED_CPUIDS` row 2 ends `0x55504320` — two bytes
transposed. Guest-visible, low severity, pre-existing, one constant to fix.
Reported at https://github.com/rrnewton/reverie/pull/521#issuecomment-5545584852.

## Limits

ptrace and KVM only. The DBT column is not measured anywhere in this directory;
building that backend needs DynamoRIO and was out of proportion. Eight subleaf
values, not all 2^32 — enough to distinguish "repeats subleaf 0" from
"terminates", which is the property at issue, and not enough to prove anything
about a subleaf not in the list.
