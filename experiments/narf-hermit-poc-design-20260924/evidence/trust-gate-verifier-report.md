# Independent trust-gate verification

## Result

**Threshold result: PASS.** I verified all 38 findings (100%). No evidence
reference is unverified, hallucinated, or stale. The full design has 48
`[F-N]` citations covering all 38 unique canonical IDs; it cites no unknown ID.
The actionable-claim inventory has 28 material current-state claim families and
its canonical-ID union is also exactly F-1 through F-38. The canonical gate
reported `verified_pct=1.0000`, `coverage=1.0000`, `blocked=false` against its
`0.95` threshold.

## Method and source identity

I read all 38 findings and checked all 67 evidence items: 58 `file_ref`s, six
captured tool outputs, one captured CI knowledge record, and two pinned external
job URLs. Every file reference was resolved against its stated Git revision,
path, and inclusive line range. The checked repositories were
at the stated revisions:

- Reverie `123df7c4c0169006fbfe1f11a1553fe333eac937`
- Hermit `ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`
- Narf `afe919924a5fd2c8e820ab3a0564b6d45438e6bc`
- Linux man-pages `227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7`

For the man-pages items, the evidence path traverses the checkout's `man2 ->
man/man2` symlink. I resolved it to the tracked object
`man/man2/seccomp_unotify.2`, confirmed byte identity, and checked the object at
the pinned commit. This is not a missing or invented reference.

Literal results were 33 `VERIFIED_EXACT` items and 25
`VERIFIED_SUBSTRING` items. Substring status means the literal snippet is a
contiguous byte-for-byte portion of the declared inclusive range; it does not
mean a fuzzy symbol match. F-37 evidence item 2 is exact source text at lines
823--826, now wholly inside its corrected declared range of lines 816--826.
Every stated revision exists locally and each range was read from that revision
rather than inferred from the checkout.

For F-25, the captured GitHub job metadata binds job `107842987657` to exact
Narf head `afe919924a5fd2c8e820ab3a0564b6d45438e6bc` and records a completed
failure. The complete captured log (SHA-256
`0002f3dddb330d012e0aca4a3993d5d6185f67b2d82c393e718dd1f6c1d9ca64`)
contains fourteen named `smoke_abi_proc2_ptrace_*` passes, both named process
ptrace passes, and the later QEMU SIGSEGV after 457 seconds.

For F-36, the captured check-run snapshot (SHA-256
`8e816cfdff7c65a33ea36005d7caf99694a2f23314ff8aaff040ca38d671f1d2`)
binds both aarch64 jobs to run `36061908203` and exact Narf head
`afe919924a5fd2c8e820ab3a0564b6d45438e6bc`: job `107842987478`
(`boot-smoke / aarch64`) completed with `success`, while job `107842987601`
(`kernel-test (aarch64)`) completed with `failure`.

## Semantic and inverse checks

- F-2: an exhaustive pinned-tree search for Rust `impl Backend for` blocks found
  exactly `PtraceBackend`, `LiteinstBackend`, and `E9patchBackend`; the only
  additional hits were documentation examples.
- F-6/F-7: the complete handlers and repository-wide inverse searches confirm
  that `PR_TSC_SIGSEGV` succeeds without a user-TSC disable path and that
  `arch_prctl` has no CPUID-control case. The GS cases are recognized but return
  `EINVAL`; F-7 is verified as a statement about the handler's case set, not as
  successful GS support.
- F-8: the only stored ptrace option read by behavior is TRACESYSGOOD. A pinned
  repository search found no `PTRACE_EVENT_*` constant or event-emission path.
- F-9: the complete seccomp handler never reads a filter pointer, rejects
  `NEW_LISTENER`, and has no notification object, notification ioctl lifecycle,
  or syscall-dispatch evaluator elsewhere in the pinned tree.
- F-11/F-12/F-13/F-14: I followed the cited declarations into the active
  dispatch, confinement, scheduler, and ELF-loader paths instead of inferring
  runtime behavior from comments alone.
- F-23/F-24: the full match/control-flow context confirms distinct-address-space
  `EPERM` and exhaustively rejects all defined attach types other than fentry and
  XDP in the cited resolver.
- F-26: the complete `sys_ptrace` match-arm inventory confirms the implemented
  trace/control/register/word-memory requests. Tight inverse searches found no
  `GETSIGINFO`, `GETEVENTMSG`, or lifecycle-event request outside explanatory
  comments.
- F-27: Narf does have `TrapContext` and a public `SyscallHandler` trait. They
  are the context and primary implementation contract for one installed syscall
  handler, not a separately registered or chained before/after interposition
  interface. The live dispatcher has concrete ptrace calls and optional text
  logging but no generic observer/interposer trait.
- F-28/F-29: operational resolver, `LinkTarget`, program-type-consumer, boot
  selector, and runtime-cell searches confirm the two current BPF targets and
  the conditional domain backend including `Unenforced`.
- F-30: `compare_io_buffers` is accepted and retained by `ComparisonSpec`, then
  copied into `ComparisonReport`; it is not inferred from the field name alone.
- F-31: broad inverse searches were reviewed rather than accepted as a zero-hit
  assertion. Filesystem journal replay, protocol replay protection, device/mock
  recording, and deterministic helper ordering are real but unrelated. No
  Hermit-style whole-program deterministic scheduler, virtual-time provider, or
  execution-input capture/replay path was found; the scheduler instead uses
  per-CPU queues, optional stealing, hardware clocks, and a monotonic-counter
  seed in its random-steal policy.
- F-32/F-33: active implementations read RDTSC/CNTPCT, fill `getrandom` through
  the CSPRNG, and build stock Reverie's seccomp filter with default `Allow` plus
  `Trace(0)` for each subscribed syscall.
- F-34: the complete helpers validate maximum size, null, integer overflow, and
  both user-range endpoints before calling the x86_64 SMAP or aarch64 uaccess
  guarded copy implementation in both directions.
- F-35: the operational `sys_bpf` path calls `task_may_load_bpf` before reading
  the user attribute; that helper delegates to per-task credential authority.
  The minted capability is subsequently passed into program loading, but is not
  the authorization decision.
- F-37: the full attach arm resolves the target, rejects self-attach, calls
  `ptrace_may_access`, and only then inserts the tracer. All three literal
  snippets are inside their declared pinned ranges.
- F-38: KVM, DBT, preload, SaBRe, and seccomp-notification helper types exist,
  but an exhaustive pinned-tree production search still finds exactly the three
  public `Backend` implementations from F-2. `KvmBackend` implements
  `BackendStatsSource`, not Reverie's public `Backend` trait.

## Findings-file integrity

The canonical findings file has 38 parseable JSON lines, 38 unique non-null
finding IDs, 67 evidence items, and only the declared `native` source kind. All
dependencies resolve. The design has 48 cited-anchor occurrences spanning all
38 IDs, with no missing or unknown ID. The actionable inventory has 28 rows;
its ID union also spans all 38 findings, no inventory ID is absent from the
design, and no design finding is absent from the inventory.

I separately scanned the design's decision-changing **Verified current**
assertions and current-state risk/table rows against the inventory method. Each
maps to one of those 28 families; proposed interface and acceptance requirements
are intentionally governed by the writing, deterministic-scheduling, and
adversarial audits rather than being mislabeled as source findings. Exact
revision identity and explicit no-runtime disclosures remain separately covered
by the revision/platform/runtime tables, as the inventory states.

The verified input hashes for this pass are:

- `findings.jsonl`: `b9a00bca41937b9e0f9770fdf04042a5dae2d36495d59adae035da57e67bde38`
- `actionable-claim-inventory.md`: `a72d2c1d22ce3203fc016fcd85186568967197c55e7bdbcb9cbe50ac4fae0b4b`
- `design.md`: `2c443f583faf18e2baa54345536b101f5ec1b91ed393788c3a716576c2e89e72`

Every file-evidence snippet is exact at its pinned object and within its
declared inclusive range. The detailed verdicts are in `verification.jsonl`.
