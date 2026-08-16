# Provenance — liteinst+verify rows 41-60 qualification evidence

**Produced by `hermit-revents`.** Promoted by `hermit-coord`; not authored, not re-scored, no verdict
re-derived. Source `ignored/liteinst-qualify-20260815/` — **gitignored, so it survived context loss
and not host loss, and devbig014 could not read it.**

## Scoping — the verdicts this evidence supports, in the producing agent's own words

> **Qualification is EXIT-STATUS REFUSABILITY UNDER THE STRIPPED COMPARATOR — `parity=false` on
> every accept arm — NOT PARITY AND NOT L2.**

Invocation: direct `hermit run --strict --verify --verify-json` per `ci/test_harness.sh:3467`. **No
`validate.sh`, no full or privileged profile. Nothing here is a qualifying receipt.**

## IS THE SOURCE STILL BEING WRITTEN? Measured, and the answer has two parts

**This directory: stable.** Its top-level file set was byte-and-mtime identical across a 40-second
interval at promotion time. **The producing agent: still working** — `hermit-revents` has moved on to
ptrace+replay 41-81. The directory holds *completed* liteinst 41-60 work, which is why it is stable
while its author is not.

**So this is a snapshot, and drift is detectable rather than silent.** Recompute the digest below
over the source with the same exclusions; a mismatch means the source advanced and this copy needs
**re-promoting wholesale, not patching**. A tracked copy stranded at 62 lines while its source reached
141 is the failure this header exists to prevent.

| | |
|---|---|
| content digest at promotion | `e89b58c1ef2416e6a96056821424c958fefa41921a48ccd478eaffbdbaaffaef` |
| text files promoted | 186 |
| bytes | 176,040 |

Digest = sha256 over each kept file's relative path followed by its bytes, in sorted path order,
across the same inclusion rule used here.

## What was excluded, and why — a checksum compares the file, not the knowledge

- **33 compiled binaries** (`cells/*/mut`, `cells/*/orig`, 17,752 bytes each). **Rebuildable from the
  `mut.c` and `orig.c` promoted beside them**, which are the actual evidence that the mutation
  differs from the original.
- **`probe`** — one 18,000-byte compiled probe; its `probe.c` is promoted.
- **`hermit-main/`** — a nested git clone, 1,190 files. A working checkout, not evidence.

**Nothing was excluded that is not reconstructible from what is here.**

## What is here

Per cell under `cells/<row>/`: `orig.c` and `mut.c` (the mutation, as source), `out-accept-*.txt`
and `out-refuse-*.txt` for both backends, and `v-accept-*.json` / `v-refuse-*.json` verify verdicts —
**both arms, both backends, named per file.** At top level: `drive.py` (the operator), `results.tsv`,
`drive.log`, and the initial backend-runnability probe outputs.

## A silent drop, caught by counting — `.gitignore` excluded the logs

**The first promotion commit landed without the `.log` files.** `.gitignore:94` carries a global
`*.log`, which applies inside `ai_docs/evidence/` too, so `git add` skipped them without a word and
the commit reported success. **This provenance file listed them as present, so it was correct about
the copy and wrong about the tracked tree** — precisely the failure the header exists to prevent,
committed by the header itself.

Caught by comparing the tracked file count against the copied count, not by reading the commit
output. They are now force-added, because a run log promoted into a tracked evidence directory is
evidence, not the runtime output that rule is for.

**Anyone promoting evidence into a tracked path should check the tracked count against the copied
count.** A gitignore rule written for one purpose applies everywhere, and `git add` does not say so.
