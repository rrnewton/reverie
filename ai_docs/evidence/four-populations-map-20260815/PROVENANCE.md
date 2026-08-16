# Provenance — the four-populations map, 2026-08-15

**Produced by `hermit-fixtures`.** Promoted by `hermit-coord`; not authored, not re-derived. Source
`ignored/evidence/four-populations-map-20260815/` — **gitignored, so it survived context loss and not
host loss, and devbig014 could not read it.**

**Promoted because `reasons.json` is what stops the next agent redoing the work.** It is a
**6,429-entry index keyed `cell|mode|backend` → reason string**, so the clustering is **re-derivable
without re-parsing 13 manifests**. That index is the re-usable half; `MAP.txt` is the summary of it.

## Source state at copy time — MEASURED

| | |
|---|---|
| `MAP.txt` | 1,557 bytes |
| `reasons.json` | 722,195 bytes, **6,429 entries** |
| still being written? | **No** — byte-identical across a 35-second interval |
| binaries | none |
| combined content digest | `abda16193a2b4343…` (sha256 over both files concatenated) |

**Copied wholesale, not patched.** Recompute the digest against the source; a mismatch means
**re-promote wholesale rather than patching.**

**`git check-ignore` was run against every destination path BEFORE copying** — no rule would have
skipped either file. That check exists because a global `*.log` silently dropped five files from an
earlier promotion: the commit succeeded, ancestry verified, and every existence check passed, because
they all asked about files that existed. **Only the count disagreed.**

## What the map reconciles

Anchored at hermit `302a1a9c0fde564db0292d1ea5cabc91343e79bc`, 13 manifests plus
`ci/compat-envelope/cells.json`:

- **336 tests · 7,056 declaration triples · 5,376 scorecard cells**
- declarations: **627 enabled / 6,429 disabled**
- scorecard: **625 enabled / 4,751 disabled**, **170 green / 5,206 red**
- **scorecard = declarations minus 1,680 custom triples** (1,678 disabled + 2 enabled)
- **red 5,206 = disabled-red 4,751 + enabled-red 455**

`reasons.json` has exactly **6,429** entries, matching the disabled-declaration count — the two
figures are consistent by construction, which is worth stating because it is the cheapest available
check on the index.

## Bearing on an open disagreement — recorded, not resolved

The top of `MAP.txt` shows **six reason strings at exactly 330** — record/replay and chaos, each
unsupported by KVM, SaBRe and LiteInst. That is the population behind the open **"six triples at 330
vs three"** disagreement in `ai_docs/QUALIFICATION_TALLY_20260815.md`. **This artifact does not settle
it**; it supplies the counts one side is reading. The disagreement stays open.
