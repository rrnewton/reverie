# Confirmed ftruncate refusal, frozen typed runtime

**Confirmed pre-Tool unpatched-site refusal, not a protected-FD or native
filesystem failure.** One external dynamic probe ran natively and once under
the unchanged frozen typed Detcore harness/DSO at 2026-09-04T22:11:04Z.
It calls ordinary libc `ftruncate` on the same two file kinds/sizes as the
seeded cases and reads only existing exported runtime counters. No backend
instrumentation or product-source change was needed.

| Control / file | ftruncate result / errno | Size before → after | Trap delta | Hook delta | ftruncate refusal delta |
| --- | --- | --- | --- | --- | --- |
| Native anonymous / target 10 | 0 / 0 | 6 → 10 | unavailable | unavailable | unavailable |
| Native regular / target 4096 | 0 / 0 | 0 → 4096 | unavailable | unavailable | unavailable |
| Frozen typed anonymous / target 10 | -1 / 95 (EOPNOTSUPP) | 6 → 6 | 1 | 0 | 1 |
| Frozen typed regular / target 4096 | -1 / 95 (EOPNOTSUPP) | 0 → 0 | 1 | 0 | 1 |

Both typed calls also increment total refusal count by one. Per-site trap and
per-syscall refusal counters progress 0→1→2; hooks remain zero. The first eight
site bytes remain unchanged. Fstat succeeds before/after on the owned writable
fd (typed fd 5); its errno is separately reset and observed as zero. Errno for
ftruncate is captured immediately after the call, before reading counters or
calling fstat. Native counters are absent, not evidence of zero interception.

Both modes resolve `/lib64/libc.so.6`, function offset `0x107430`, syscall offset
`0x107439`; the dynamic symbol reference is `ftruncate@GLIBC_2.2.5`. Libc SHA256
`d932cb6bc88da10cc709d5b7ecda57a1387b6571331689dba88023b4c0517776`.
The probe uses the ordinary linked libc call, not an alternate raw syscall.

## Why this settles the earlier uncertainty

Frozen `reverie-liteinst/src/runtime.rs:2796` increments these refusal counters
immediately before line 2798 returns EOPNOTSUPP. The counters are exported at
`src/lib.rs:223` (site traps), `:229` (hooks), `:243` (total) and `:255`
(syscall-number refusals). This positive evidence locates the return before
the shared Tool/protected-FD handling, rather than inferring it only from a
missing log line. A real kernel errno would not take this refusal path.

The counter does not expose the precise decoder/publication rejection reason.
The previously observed syscall+compare span crosses a 16-byte boundary, but
that subreason is still not directly observed. The confirmed scope is the
claimed site with no installed hook, refused before Tool dispatch.

The original two fixture failures are explained by this independently
reproduced common libc path. Their binaries/results were **not modified or
rerun**. A future approved fallback+logger integration must rerun those exact
original fixtures; this probe is not a substitute for their acceptance checks.

## Execution and identity

- External source: `ftruncate-counter-probe.c`, SHA256
  `f76745d5d0b2e7d63502a67db91a2a4f776866d812a26018da8129b4f854a845`.
- Dynamic probe ELF: `147aef2a287e02565efd33a828720d5e53d2e6637f8ab163f245a24c35689e8f`.
- Frozen DSO: `b57a3047b026a6b14cee1cc67c4b0b599eacfb8ef8b42074c23c1e82c3b21c98`.
- Frozen harness: `da318497951eda5455bf599f0cf2a60aba81649c94f3a72e9b60a548c0662d40`.
- Logging source archive: `8b185ecd8c650340a00715ab5e02aadff54ca009bf1b9ee148b7f45ef527dfed`;
  Hermit base `4d8f866102882b6eeabdf99cc7e81433cc3c95c5`, Reverie base
  `320412c5967790939ebe405c73e394ffd9c41459` plus frozen logging changes.

Commands in `ftruncate-cause-diagnostics.json`: one admitted tiny C compilation,
then native and typed probe executions. Same explicit minimal environment and
host cwd backing the prior tmpfile fixture; typed config byte-identical to
frozen logging `true/config.json`. No changed strict setting or native bypass
inside the typed guest. Existing no-ptrace launcher remains in use: recorded
guest/parent 1992640/1992638, inherited Seccomp=2 and NoNewPrivs=1. Real shared
Detcore/RPC events are retained. Native exits 0; typed guest/harness exit 1;
DAG exit 1 reflects the actual typed failure, not a passing contract.

Serial admitted two-core/eight-GiB/no-swap execution, 30-second child and
35-second step bounds, 1-MiB file/log caps, core dumps disabled. No broad build.
Both stderr captures are empty; typed local log is 7198 bytes, 34 INFO records,
14 inbound syscall events, transport error null. No ftruncate syscall event
appears. Known logging batch defect, real preemption and full CLI envelope
remain unqualified; no L2/90 credit or denominator changes.

Raw evidence, source/ELF hashes, config, counters, logs, shared RPC, argv/status,
and original-results/source immutability checks:
`/home/newton/work/dev-hermit/ignored/liteinst-ftruncate-cause-20260904/`.
`fixture-source-final.sha256` and `fixture.sha256` identify compiled source;
the earlier `identities-before.sha256` includes a pre-compilation draft hash
before adding the separate errno reset for the diagnostic fstat.

Fallback HEAD `7b51e521fcacbbe876c93d1cd0893c442c1a52f6` remains clean and
unchanged. No logging-slot edits. Next action belongs to the assigned owner:
integrate the reviewed fallback with logging under the existing review gates,
then rerun the original two seeded contracts. No speculative backend fix made.
