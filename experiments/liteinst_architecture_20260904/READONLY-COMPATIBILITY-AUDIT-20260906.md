# Frozen LiteInst compatibility evidence — completed read-only audit

Recorded 2026-09-06. This records the completed bounded audit, not a new run.
The audit used backend-reality-reviewer and presenting-quantitative-data.
No product, baseline, or task edits; no builds, tests, guests, or network calls.
Only this new external report is written during recordkeeping.

## Immutable population and task bindings

Artifact directory A:
`/home/newton/work/dev-hermit/experiments/liteinst_architecture_20260904`

Recomputed SHA-256 hashes:

| File under A | SHA-256 |
| --- | --- |
| baseline.json | 9ad9f3841f92aba44930ec0e94f389e904cee353b70e82279daafc67239e7bfd |
| baseline.tsv | 4c8a39f9170cdcd4b5e334f0dc7521fde8a856be441d293155bf1de1d3cac48f |
| population-identities.txt | 2cf9960cbe319f9214023d748482a6986e47f5822f728254c96ddfb7248bc530 |
| METHOD.md | deb57f0888296530da472edf5aca74c7138c26b2b6b3b14fb4ce570049ab28bf |
| initial-executable-linkage.json | b439b0f2832e431d705eef03c6e15514d0d1fc1c7a04ab0527909211160385a3 |

Frozen Hermit commit: `4d8f866102882b6eeabdf99cc7e81433cc3c95c5`.
Frozen Reverie pin: `320412c5967790939ebe405c73e394ffd9c41459`.
Original matrix: `ci/compat-envelope/cells.json` at that Hermit commit;
SHA-256 `a35b1ded0c09500e7a8b4e3ad0661dbf64ae95e17e7b8d364970d4827b85e5e2`.
Reading the pinned Git object independently reproduced both matrix and population hashes.

Select every matrix row with backend=ptrace, mode=verify, status=green.
Identity is `lane/category/test/mode/backend`; repeated category components
are intentional. Sorted LF-terminated identities reproduce the population hash.
Recomputed: **340 rows / 340 distinct cells**. Join LiteInst using the identical
`(lane, category, test, mode)` tuple and substitute only backend=liteinst.
Do not filter on linkage, enabled state, measurement, or available evidence.
**Acceptance remains unchanged: >=306/340.** No denominator rewrite occurred.

Local TaskGraph database `hermit2`:
- `liteinst_90_percent_architecture`, note 20642, binds A, the population hash,
  frozen commits, and target.
- `sud-only-in-guest-mode-with-patching-optional-and-backend-consolidation`,
  note 21461, retains the target while requiring actual shared Detcore
  `openat -> add_fd -> injected fstat -> close` rather than a generic Tool fixture.
- No direct dependency edge between these task IDs was found; the binding is documentary.

**Narrative hash discrepancy:** note 20642 records `A/architecture-audit.md`
SHA-256 `c5b522e1999a2ab1a150674f27486d59ca0ae9899bfd337975189667a5843cf3`;
the audited file instead hashes to
`31ce820a4ab1742e53a8b4c68e77e0371adad281043e18e726d3c4e6fca6a8bd`.
That note does not byte-authenticate the current narrative. No cause is inferred;
neither note nor old file was modified. The independently reproduced population
binding remains intact.

## Historical assurance and current architecture limits

253/340 rows contain cached passing canonical-comparison summaries with positive
INFO counts at Hermit `07c10eb80ac4fafbbe65134b504d77ef7e388d25`;
87/340 lack that cached evidence. The 253 are a diagnostic subset, not a new
denominator or 253 independently authenticated strict reports. Five selected
ptrace green-label rows also carry measurement=diverged-unlocated.
Plain lossy `--verify` is not L2.

Legacy LiteInst labels: 28 green / 23 red / 289 not-applicable, all within 340.
They describe the old mandatory ptrace-host route, not new SUD-only qualification.
Current working-tree CLI source selects shared Detcore through:
`hermit-cli/src/bin/hermit/run.rs:4568` -> `hermit-cli/src/lib.rs:2813` ->
`hermit-cli/src/liteinst.rs:558` (`LiteinstBackend::prepare...::<Detcore>`);
runtime installation is at `liteinst-runtime-build/detcore-runtime/src/lib.rs:154`.
These are working-tree observations, not proof of the clean committed tree or a run.
The matrix identity remains `.../verify/liteinst`; old results cannot transfer to
the changed implementation. The actual full CLI remains startup-failed per the
supplied checkpoint. No new-architecture green, numerator, or L2 claim is made.
The finite generic Tool native fixture is not a reference cell.

## Reproducible bounded three-case selection

Curated eligibility shortlist: backend-parity-c/{append-pwrite,file-io-roundtrip,
openat-flags,vectored-file-io}. These are single-process regular-file FD cases,
all in the frozen cached subset, with retained dynamic ELF artifacts and source
hashes matching frozen sources. This is not an exhaustive random sample of 340.
More complex metadata/time/lifecycle cases were not included in this bounded shortlist.

Seed: `liteinst-fd-startup-20260906-v1`.
For each candidate form the full ptrace identity
`portable/backend-parity-c/backend-parity-c/<case>/verify/ptrace`.
Rank by ascending SHA-256 of UTF-8 `seed + NUL + identity` (no trailing newline),
breaking ties by identity; take the first three without replacement:

| Rank | Case | Selection digest |
| --- | --- | --- |
| 1 | vectored-file-io | 08bc8f3d9fa364fe7c45781a8b4544778e1916370af9d35fd315ede918837df2 |
| 2 | file-io-roundtrip | 5d96a65a28b339791719e9c4b401c34c023af642fd18bd28f2792675cbd8fe59 |
| 3 | openat-flags | df30fe3e0257d9e57f5876406a6ba588d5687bfc5d73415db875d83dfdea7b6d |
| 4, not selected | append-pwrite | df4abe7b890e0004be795c46eb4f17b0f912fdedcb573ea9a9d2df8078b96518 |

The three selected full reference identities, literally and in selection order:
1. `portable/backend-parity-c/backend-parity-c/vectored-file-io/verify/ptrace`
2. `portable/backend-parity-c/backend-parity-c/file-io-roundtrip/verify/ptrace`
3. `portable/backend-parity-c/backend-parity-c/openat-flags/verify/ptrace`

Their paired LiteInst identities are respectively:
1. `portable/backend-parity-c/backend-parity-c/vectored-file-io/verify/liteinst`
2. `portable/backend-parity-c/backend-parity-c/file-io-roundtrip/verify/liteinst`
3. `portable/backend-parity-c/backend-parity-c/openat-flags/verify/liteinst`

These identity strings are not executable basenames or row indexes. The short
case labels are vectored-file-io, file-io-roundtrip, and openat-flags; source
basenames are vectored_file_io.c, file_io_roundtrip.c, and openat_flags.c.
Each retained guest executable has basename `program` under its own case's
`fixtures/` directory. Frozen zero-based ptrace/LiteInst matrix row indexes,
in the same order, are 1694/1693, 446/445, and 926/925.
This new diagnostic selection does not change A/sample.json or its original seed.

Useful eventual coverage:
- vectored-file-io: temporary FD creation, vectored IO, fstat, close.
- file-io-roundtrip: open, read/write, fsync/seek, fstat, truncate, close.
- openat-flags: open flags/errors, truncation, fstat, repeated close.

All three retained binaries were inspected read-only and have PT_INTERP:
`/nix/store/0d8g8n0a11v6f5m2h416ajyxmnkwc3md-glibc-2.42-67/lib/ld-linux-x86-64.so.2`.
They are useful dynamic-startup candidates, not evidence that new LiteInst starts
them. Full cases require more than the initial openat/fstat/close slice.

## Exact historical report and command bindings

Run ID R:
`validate-current-tip-full-validate-07c10eb80ac4-1788143718039901059-4036218-e4cf26b8`

Evidence root E is exactly
`/home/newton/work/dev-hermit/ignored/validate/artifacts/<R>/e2e`, substituting R above.
Primary records: `E/portable/manifest_backend_parity_c/results.jsonl`, SHA-256
`f9a7d2c95715ed2987b4e7a8730638f08d88373213a90763c04cee518f1ece29`.
For each case C, retained report path:
`E/runs/<R>/backend-parity-c-<C>-verify-ptrace/verify-1.json`.
Each recomputed report hash matches its recorded attempt binding:

| Case | Report SHA-256 | Compared INFO left/right |
| --- | --- | --- |
| vectored-file-io | 9256650987b4d01e600fc8b4bb43074467635cf66da16d80b8e5e236fa8884c7 | 147/147 |
| file-io-roundtrip | 8d9c1fc0829666196349e77f173a00a0f7b9f076fb10dbb31760e5059e7d200c | 148/148 |
| openat-flags | 39afb639565696dea6a33f17ad97a07820f332ca28b4d5e9cfb86197f050e11d | 168/168 |

Guest binaries are adjacent at `fixtures/program`; recomputed SHA-256:
- vectored-file-io: `de1f95d53766a9c27cf7a39ec9ba9dcddf42d3ccc09439b2b8d14cbbfaad5ced`
- file-io-roundtrip: `dfebfd41ba9dd6b10ecb26baa83185a70c0a6e0c2cb368951239af748593cbd3`
- openat-flags: `ff8c5ede01f5d3de49bc2c33a6a16ea706eaa926f14438db0fc28682758c7106`

Pinned source files are `tests/backend-parity/fixtures/<case_with_underscores>.c`.
Their Git-object hashes match the historical result records:
- vectored_file_io.c: `89d46cdc0020c8b3f2d449ac1f1976819af1807cbb3ebcd542227d8754057f5d`
- file_io_roundtrip.c: `4f4cec85cc5d11a52a031bd0f469abed5d687db10f568e130d9bd7dc07b4d89f`
- openat_flags.c: `7a18d9124ede1c72a48f401df3eaac44abcb20ba62590362e4149804761e4860`

Exact recorded argv template, not a command executed by this audit:
let D be container path `/results/runs/<R>/backend-parity-c-<C>-verify-ptrace`.
The executable is
`/src/target/ci/hermit-e2e-artifacts/b164b4a0072817d4e8697e86187f66c170f12156666f20fc88031556b997c55a/hermit`.
Recorded arguments, in order:

```text
--log info run --base-env=minimal --backend ptrace --strict --verify-strict
--verify --verify-json <D>/verify-1.json --mount=type=tmpfs,target=/test
--workdir /test --env LC_ALL=C --env TZ=UTC --env HOME=<D>/home
--env XDG_CONFIG_HOME=<D>/xdg-config --env E2E_TMPDIR=/test
--env E2E_FIXTURE_DIR=<D>/fixtures --env HERMIT_E2E_SCHEDULED_JOBS=8
-- <D>/fixtures/program
```

Historical records identify Hermit `07c10eb80ac4fafbbe65134b504d77ef7e388d25`,
source_tree_dirty=false, host devbig014, kernel
`7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, log=info, relaxations=none.
Each has one recorded attempt with exit 0, verdict=matched, bitwise_parity=true,
and the positive counts above. Comparison is BitwiseInfoV1, including IO buffers,
removing only the real wall-clock prefix and ordinalizing host addresses while
comparing the remainder exactly. This is retained historical **ptrace L2**
evidence, not literal raw-log byte identity, a fresh rerun, L3/L4, or new LiteInst
qualification. Raw logs were not independently recomputed, and a retained path
alone is not historical execution attestation. Remaining population evidence
gaps are unchanged.
