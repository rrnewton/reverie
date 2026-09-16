# Three frozen samples: typed in-process Detcore diagnostics

Executed once each, 2026-09-04T22:00:09Z–22:00:10Z. **One successful guest
contract, two guest contract failures; no L1/L2/90% or matrix numerator claim.**
No builds, new integration code, product edits, retries or extra guests.
Fallback source remains byte-identical to the snapshot Jason is reviewing.

| Frozen case | Guest / harness exit | Observed result | Retained INFO / inbound syscalls |
| --- | --- | --- | --- |
| ioctl-fioclex | 0 / 0 | Exact `fioclex-ok\n`; original fixture assertions satisfied | 38 / 16 |
| o-tmpfile-anon | 1 / 1 | `ok=5 size=6 nlink=0 preadlen=6 truncsize=-1` (requires six checks) | 22 / 8 |
| record-lock | 1 / 1 | `ok=5 getlk_type=2 unlck=2` (requires six checks) | 28 / 12 |

All guest stderr files are empty. Captured log transport reports `error:null`
with 7324 / 4516 / 5924 bytes respectively. Those are retained-event counts,
**not comparison counts or proof of complete instrumentation**. The frozen
logging batch defect reported by the owner is not fixed or qualified here;
transport success does not establish event completeness. No repeated-run
comparator or full CLI namespace/affinity envelope ran; real preemption and
exact accounting remain unqualified. Frozen population 340, target >=306,
static labels and original seed/list remain untouched.

## Failure localization, logs first

Both failed fixtures isolate the truncation contract, not their namesake APIs:
- `o-tmpfile-anon/guest.detlog:12` records successful pread64, followed at
  line 15 by close. There is no ftruncate event or subsequent size-check fstat.
  Source and `truncsize=-1` locate the failed `ftruncate(fd,10)` step. Initial
  anonymous open, six-byte write, file type/size/link-count and readback succeed.
- `record-lock/guest.detlog:8` completes open, followed at line 9 by F_SETLK.
  The required `ftruncate(fd,4096)` between them has no event. All five locking
  checks succeed; output/source isolate the failed initial truncation.

Thus the two observations form **one likely interception/fallback cluster**,
not evidence of independent O_TMPFILE and record-lock emulation defects.
The historical paired ptrace logs for the identical binaries record both
ftruncate calls returning Ok(0); they are historical reference, not reruns.

Frozen runtime `reverie-liteinst/src/runtime.rs:2798` refuses unpatched sites
with EOPNOTSUPP before shared Tool dispatch. Host libc disassembly places the
ftruncate syscall at ELF offset `0x107439`, followed by a six-byte compare;
the eight-byte instruction span crosses a 16-byte boundary. This is a concrete
candidate for the existing patch/fallback limitation, **not proof that this
exact refusal fired**. Neither errno nor the fallback counter was captured;
logging loss is another uncertainty. No unsupported result is called a pass.

Next scopes: (1) finish independent fallback review; after approved integration,
instrument/observe this exact ftruncate site and rerun these same contracts,
without a second syscall engine; (2) independently repair/qualify the known
logging batch defect; (3) retain timer/TF/full-CLI qualification as separate
gates. Do not change the file/lock assertions or infer readiness from ioctl.

## Exact path, resources and identities

Uses the unchanged `liteinst-detcore-strict-probe PRELOAD OUTPUT PROGRAM [ARGS]`
harness and `LiteinstBackend::run_with_output_and_preload_data_and_log::<Detcore>`.
The original `run-probe.sh` postchecker accepts only true/echo names, so the
existing general harness was invoked directly with the same admitted bounds;
no checker or integration code was broadened. The official DAG admission route
owns containment; no legacy Hermit CLI, hosted backend or ad-hoc CLI invocation.
No-ptrace launch filter kills SYS_ptrace and is inherited by both coordinator
and guest. Recorded guest/parent PIDs: 1293421/1293406, 1294809/1294798,
1295904/1295902; each guest reports Seccomp=2, NoNewPrivs=1, and each
coordinator has one seccomp filter. Shared Detcore local syscall events and
global CreateChildThread RPC are retained.

Exact existing dynamic fixture ELF paths/argv and seven environment overrides
come from the ptrace-side rows of `paired-execution.json`; hashes match those
previously used by both paired backends. Environment is explicitly minimal
(`PATH=/usr/bin:/bin` plus those overrides); no RUST_LOG or legacy LD_PRELOAD.
Harness adds its existing typed bootstrap variables. Cwd is the prior fixture's
host `workdir/1`, which formerly backed guest `/tmp/test`; no namespace alias is
claimed. These fixtures do not depend on that cwd spelling. Their absolute
`/tmp` operations now use host `/tmp`, not the CLI filesystem envelope.

Every emitted config is byte-identical to the frozen true probe: seven existing
strict-derived differences, sequentialization/deterministic I/O/unsupported
refusal enabled, unchanged timeslice, no_rcb_time=false, imprecise_timers=false.
`has_uts_namespace=true` is a config value, not proof of a created namespace.
Guest logging INFO, coordinator TRACE. No strict setting was weakened.

Serial admitted DAG: two CPU bandwidth cores, eight GiB, zero swap; 35-second
steps, 30-second hard-kill child bound, 60-second CPU budget, core dumps disabled,
1-MiB file/retained-log guards. Observed cgroup admission and teardown retained.
DAG exit 1 faithfully records two failures. Full commands: `typed-seeded-diagnostics.json`.

Hermit base `4d8f866102882b6eeabdf99cc7e81433cc3c95c5`; Reverie base
`320412c5967790939ebe405c73e394ffd9c41459` plus frozen logging changes and
external subscriber teardown patch (notes 20705/20707). Exact identities and
results: `typed-seeded-results.json`. All raw logs/configs/status/ELF checks and
before/after hashes stay outside product repositories at
`/home/newton/work/dev-hermit/ignored/liteinst-typed-seeded-20260904/`.
