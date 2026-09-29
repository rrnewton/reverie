# Packaged-Hermit provenance mismatch: audit (2026-09-25, head b6b8f7a16a8a)

Verdict: owner-only blocker. Neither an in-tree nor a released Hermit is both
functionally qualified and witness-compatible. Re-pinning the packaged digest
would only change the refusal label, from PROVENANCE_MISMATCH to
BLOCKED/WITNESS_MISMATCH. Substituting the preserved witness is forbidden
(`fbcode/testinfra/tfa/README.md:112-124`).

## Observed

Three-case rerun (`../three-case-b6b8f7a16a8a/SUMMARY.txt`):

- validate: rc 0
- dry-run: rc 0
- all six witnesses: rc 1, `HermitError: packaged Hermit compatibility is PROVENANCE_MISMATCH: expected revision=4630ee2a…`

This matches the run at 0a413f6c (`../three-case-0a413f6c`).

## Why no in-tree fix exists

1. `hermit-import-compatibility.v1.json` states:
   - The a919cce import's packaged executable (9bf7064a) is BLOCKED, because
     verify-verbose `/bin/true` TRACE fields differ.
   - Its qualification probe (d24fe3a2) is BLOCKED on three short candidates:
     - asyncssl seed 1: rc 1, sanitizer RLIMIT_CORE
     - thrift seed 0: rc 132, TSan RLIMIT_CORE
     - httpcoro seed 1: rc 1, PMU skid
   - Listed blockers: RLIMIT_CORE, PMU timer skid, and no retained verify-log
     option.
2. fbsource still has the RLIMIT_CORE regression:
   `fbcode/hermetic_infra/hermit/detcore/src/syscalls/sysinfo.rs:76-78` returns
   EPERM for any `prlimit64` set other than STACK/NOFILE.
3. Every fbsource Hermit change since the import is a dependency or toolchain
   bump: D121031265, D120084225, D119231867, D117139384, plus MSDK bumps. Those
   bumps, together with `fbcode-build-info-mode[full]` stamping on
   `//testinfra/tfa:hermit-test-bin-opt`, are why the built digest drifts from
   the recorded one. None of them fixes a blocker.
4. The released MSDK dotslash `fbcode/hermetic_infra/hermit/hermit` is built
   from fbsource e410936c (opt-clang-thinlto). It is the same source with the
   same blockers, so no compatible released package exists.

## Where the fixes are

The fixes exist only in the owner fork (`rrnewton/dev-hermit`, local
`/home/newton/work/dev-hermit`, HEAD 7eae6212). a919cce is an ancestor of HEAD,
and HEAD is 4739 commits ahead of it.

| Commit | Date | Subject | Size |
| --- | --- | --- | --- |
| ccbe37e91 | 2026-09-21 | Accept virtual RLIMIT_CORE updates (#3142) | 2 files, +146/-24 |
| b38586bd6 | 2026-09-01 | Activate recoverable PMU skid handling (#2918) | 14 files, +147/-71, includes dependency bumps |
| 02d978736 | 2026-08-28 | Report completed runs with PMU overshoots (#2733) | 14 files, +954/-82 |

No commit in the fork adds a retained verify-log option.

Correction (2026-09-25): an earlier version of this audit said that 4630ee2a61f9 is
not an object in the local clone. That was wrong. The superproject HEAD doesn't
contain it, but the `dev-hermit/hermit` clone of github.com/rrnewton/hermit does.
It is commit 4630ee2a61f93221a43e2baec0a15f34102a2c61 ("Accept virtual
RLIMIT_CORE updates", parent 4dcb34b4, 2026-09-20). It sits on remote branch
`origin/salvage/devbig014/hermit-tfa-prlimit-fix/hermit-1-4630ee2a61f9`, contains
b38586bd6 and 02d978736, and does not contain ccbe37e91. Details are in
`../external-hermit/source-provenance.txt`. The preserved binary (sha256
5c422b85…) self-reports `g4630ee2a61f9` with no dirty marker. It was freshly
qualified against all three cases (`../external-hermit/qual-4/`, QUALIFIED). It
is used only through the separately labelled `external-oss-prototype` witness
mode, never as the packaged Hermit. The packaged-import lane below is unchanged.

## Owner lane

The owner lane is defined by `fbcode/hermetic_infra/hermit/CLAUDE.md`
("Contributing"). The precedent is D113553943, "[hermit][PR] Hermit v2: sync
validated stable branch" (`rrnewton:stable` a919cce → facebookexperimental/hermit
PR #86, reviewers fiorix, wangbj, #hermit_devs).

1. Hermit owner: promote `rrnewton:stable` to include ccbe37e91, b38586bd6,
   02d978736 and a retained verify-log option, and validate it at L2 or better
   on the ptrace backend.
2. Hermit owner: open the bulk sync PR to facebookexperimental/hermit. This is
   outward-facing and needs explicit approval. My proxy identity is also
   denied.
3. #hermit_devs: import it into fbsource, a reviewed diff under
   `fbcode/hermetic_infra/hermit`.
4. TFA owner (explicit decision required): re-record
   `hermit-import-compatibility.v1.json` with the exact packaged binary built
   from that import, and decide whether the preserved 4630ee2a witness may be
   superseded. The README currently forbids silent substitution.

Porting a subset (e.g. only ccbe37e91) as a draft would still leave PMU skid,
the verify-log gap and the witness mismatch. So it could not produce
passing/failing witness evidence, and I did not attempt it.
