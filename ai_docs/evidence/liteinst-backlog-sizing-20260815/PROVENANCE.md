# Provenance — liteinst backlog sizing, 2026-08-15

**Produced by `hermit-teardown`.** Promoted by `hermit-coord`; not authored, not re-derived except
where stated below. Source `ignored/evidence/liteinst-backlog-sizing-20260815/`.

**Promoted because it survives context loss and not host loss, and devbig014 cannot see `ignored/`.**
An artifact that states its own floor, its own selection bias, and five things it does not license is
the one most worth surviving this host, and was the least likely to.

## Source state at copy time — MEASURED, not taken

| | |
|---|---|
| `sizing.txt` | 3874 bytes, sha256 prefix `83c6358bbe4e783a` |
| checksum manifest | `d15e241d4e0b96115a393e29b5c96f450d56480d358da10087fd51a0d120ff82` |
| `sha256sum -c` | `./sizing.txt: OK` — 1/1 |
| **still being written?** | **No.** Byte-identical and same size across a 45-second interval, so this is a snapshot at a declared stopping point rather than a live mirror. |

Copied wholesale, not patched. The two checksum manifests travel with it so the tracked copy can be
re-verified against the source while the source exists.

## The 79 — INDEPENDENTLY REPRODUCED BY hermit-coord

The artifact names its own population, which is what makes it checkable:
*"LITEINST CELLS IN THE QUEUE: 79 (all verify mode; no replay or naked rows)."*

Counted directly from `ai_docs/QUALIFIABLE_FIXTURE_QUEUE_20260815.tsv`:

- rows with `backend == liteinst` **and** `mode == verify`: **79** — matches
- rows with `backend == liteinst` in any other mode: **0** — matches "no replay or naked rows"

So the figure can now be cited with its population from a tracked file rather than from a note, and
**79 is arithmetic over the whole queue rather than a judgement about any row** — which is why the
one-row disqualification amended into that queue's header does not touch it.

**Also settled by the same count:** `backend-parity-c/sendfile-copy` does have a `liteinst`/`verify`
row, so it *is* among the 79. I had recorded that as RELAYED-and-unconfirmed when amending the queue
header, because I could not then locate the population; it is now confirmed.

## What the artifact carries — verified present, not summarised here

Its population statement, **three reasons 79 is a floor**, the census-is-not-a-sample point about
where an interval belongs, **the named selection bias**, **five things it does not license**, and the
corrected independence paragraph. Read it there; restating them here would create a second copy to
keep in sync.
