# Phase-two evidence, curated copy

[hermit2, sub-tfa2-030, opus, devbig030, role=impl]

This is owner decision 9 from [../../WAY_FORWARD.md](../../WAY_FORWARD.md), decided by coord2 on 2026-09-29 as "YES, but CURATED". It preserves the evidence of the TFA phase-two prototype, the 22 unpublished fbsource diffs described in [../../STACK_STATE.md](../../STACK_STATE.md), in a tracked directory. Before this, the evidence lived only in another agent's slot and in `/tmp`.

Copied 2026-09-29 by sub-tfa2-030. Nothing was moved or deleted in the source slot.

## What is here

| | Files | Bytes |
|---|---:|---:|
| Tracked here (text files of at most 1 MiB: reports, logs, JSON results, verdicts, patches, small scripts) | 9,327 | 28,107,443 after host-name redaction (28,121,191 before) |
| Archived outside git (text files over 1 MiB) | 214 | 1,268,882,584 uncompressed |

The archive is untracked, at `/home/newton/work/dev-hermit/ignored/tfa-phase2-archive/tfa-phase2-large-text-20260929.tar.zst` on devbig030:

- 60,864,481 bytes, zstd level 12.
- sha256 `92b68b4d7456bac5dae12b1a63747e97ad98e265810cf6444a17fe1f4a664c7b`.
- The output went through `head -c 524288000` as a 500 MB cap. It came to 60.9 MB, so nothing was cut off.
- `zstd -t` passes, and `tar -t` lists all 214 files.

[ARCHIVE-CONTENTS.tsv](ARCHIVE-CONTENTS.tsv) lists every archived file and its size. The archived files are mostly raw Hermit run logs from phase-two stress and relocation runs, and three investigator transcripts.

[MANIFEST.tsv](MANIFEST.tsv) maps every tracked file to its original path, with its size and sha256 as stored here, and the number of redactions made in it.

## Layout

- `slot/tfa-phase2-evidence/` and `slot/tfa-phase2-three-case/` come from
  `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/`.
- `tmp/` comes from `/tmp` on devbig030. The files were taken from coord2's byte-identical preserved copy, `/home/newton/work/dev-hermit/ignored/tfa-phase2-preserved-20260928/files/tmp/`; `KEEP-LIST-NOTES.md` in that directory explains the selection. The subdirectories are:
  - `tmp/tfa-phase2/`
  - `tmp/tfa-phase2-controller-followup-evidence-20260924/`
  - loose `tmp/tfa-*` files.

Where to start:

- **Results report:** `slot/tfa-phase2-three-case/RESULTS.md`.
- **The three candidate test patches:** `slot/tfa-phase2-three-case/aggregation/{asyncssl,httpcoro,thrift}/patch.diff`. Each directory also holds that patch's hypotheses, notes, validation JSON and candidate report. The agent outputs are in `slot/tfa-phase2-three-case/investigation/`.
- **Stress replays before and after the patches:** `slot/tfa-phase2-three-case/stress/`: the driver `postfix_20260925.sh`, `postfix-*.out` and `.rc`, and per-case directories.
- **Relocated replays:** `slot/tfa-phase2-three-case/relocated/` and `relocated-v2/`.
- **Codex containment diagnostics:** `slot/tfa-phase2-evidence/diagnostic/`, runs v1 to v8b with their receipts.
- **Receipts, gates and reviews:** `slot/tfa-phase2-evidence/receipts/`, `gates-*/`, `review/` and `reviews/`, plus `slot/tfa-phase2-three-case/gates/` and `reviews/`.

## Changes made while copying

- **Redaction.** 40 files contained Devmate "fixtests" links whose signature expired on 2026-09-24/25. Each `signature=<hex>` was replaced with `signature=REDACTED`, so those files no longer match their originals byte for byte. [MANIFEST.tsv](MANIFEST.tsv) records the number of redactions in each file.
- **Renames.** 89 files ending in `.log` are stored as `.log.txt`, because the repository's `.gitignore` excludes `*.log` (line 94). [MANIFEST.tsv](MANIFEST.tsv) gives each original name.
- **Host-name redaction (2026-09-29).** In 760 of these files, 1,333 internal host names were each replaced with their first DNS label: for example, a machine's full name became `devbig030`, and `www.<internal domain>` became `www`. This follows the privacy rule in hermit's `.llms/skills/benchmark/SKILL.md`. [MANIFEST.tsv](MANIFEST.tsv) now gives each file's bytes and sha256 after redaction, and a new last column counts its host names. The tool is [../../tools/redact_internal_fqdns.py](../../tools/redact_internal_fqdns.py), and the per-file counts are in [../../REDACTIONS.json](../../REDACTIONS.json). Internal host names are not in the archive-contents list and were never in the file paths; the untracked archive itself is unchanged.
- **Credential scan.** Every tracked file was scanned for credential-like patterns: GitHub, Slack and AWS tokens, bearer or basic auth headers, private keys, and signed-link parameters. Nothing else matched.

## Not copied (measured with `find -printf '%s'`)

| Source | Bytes | Files | Why |
|---|---:|---:|---|
| `tfa-phase2-evidence/meta-build/` | 11,652,799,227 | 10,364 | build output |
| `tfa-phase2-evidence/codex-build/` | 1,388,938,544 | 1 | unsigned Codex binary; the receipts record its sha256 and build recipe |
| `tfa-phase2-evidence/codex-source/` | 122,615,268 | 8,680 | copy of a source tree |
| `tfa-phase2-evidence/meta-source/` | 105,575,176 | 17,614 | copy of a source tree |
| Python bytecode cache trees (`pyc*` directories under `gates/`, `reviews/` and `tmpdir/`) | 145,501,571 | 5,770 | caches |
| Other build or cache directories (`__pycache__` and similar) | 97,550,895 | 4,829 | caches |
| Binary files (NUL bytes) outside those trees | 22,515 | 7 | binaries |

Also not copied: the slot's `ignored/fbsource`, an EdenFS fbsource checkout that was never walked. The items from `/tmp` left out of the keep list are described in `KEEP-LIST-NOTES.md`.

`df -h /home/newton` read 253G free before the copy, and 251G free after the archive was built.
