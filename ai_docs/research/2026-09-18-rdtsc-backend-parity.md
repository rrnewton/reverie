The retained evidence supports three distinct backend defects, rather than one
RDTSC-recording repair. KVM lacks userspace Tool dispatch, LiteInst adds backend
loader work to the observed execution, and SaBRe has an event-transport gap.
All three cross-backend comparisons remain **FAIL**. This research report does
not implement a fix, approve a scheduler design, or establish main as green.

The observation is RUN1874, run ID
`validate-ops-tick-ad3474118660-f2cdd9e5165a`, on Hermit
[`ad3474118660fe31f5497193b1677f67a1bd51fb`](https://github.com/rrnewton/hermit/tree/ad3474118660fe31f5497193b1677f67a1bd51fb),
with its recorded Reverie pin
[`f97b7be1de4e2ef10ecc24cee5d8cc47f2fd254f`](https://github.com/rrnewton/reverie/tree/f97b7be1de4e2ef10ecc24cee5d8cc47f2fd254f).
It addresses the research criterion of TaskGraph task
`sabre_diverges_from_ptrace`: whether KVM, LiteInst, and SaBRe have a common
mechanism behind their differing RDTSC records. Source references below name
these immutable revisions; no new guest execution was performed for the trace.

| Backend | Complete retained pid-probe logs | Dispatch and distinguishing evidence |
| --- | --- | --- |
| ptrace reference | Ten RDTSC records in each reference execution. | Reverie `tracer.rs:1628–1647` enables `PR_TSC_SIGSEGV` before exec. `task.rs:2636–2658` decodes the fault, then `handle_rdtscs` at 2041–2064 calls the shared Detcore callback. |
| KVM | Zero in four candidate executions; first cross-backend divergence at record 10. | `reverie-kvm/src/bootstrap.rs:195,209` enters CPL3 without enabling CR4.TSD. Its vCPU loop has no `Tool::handle_rdtsc_event` dispatch. This is a missing userspace interception/dispatch path, not just an absent INFO line; it says nothing exhaustive about internal hardware/kernel VM exits. |
| LiteInst | Twelve in four candidate executions, versus ten in each reference; first divergence at record 16. | Hermit selects the ptrace-owned host hybrid. The two additional records bracket loading `libreverie_liteinst.so`; they enter the same Tool callback and time accounting as ptrace. |
| SaBRe | Zero in four candidate executions, versus ten in each reference; its first divergence is independently record 3. | A rewritten-instruction route reaches the shared Tool callback, but its plain INFO output is outside the plugin's DETLOG-only forwarding channel. Whether every omitted instruction actually reached that callback remains unmeasured. |

The common emitter is
[Detcore's `handle_rdtsc_event`](https://github.com/rrnewton/hermit/blob/ad3474118660fe31f5497193b1677f67a1bd51fb/detcore/src/lib.rs#L1337).
It charges logical time and emits `info!` at line 1347. Thus "recording is
ptrace-specific" is too broad: SaBRe has an adapter to this callback, and the
selected LiteInst implementation already runs it in the ptrace host. Changing
this logger alone cannot supply KVM interception or remove LiteInst loader work.

LiteInst's selected route is explicit in
[`hermit-cli/src/lib.rs:2753,2967`](https://github.com/rrnewton/hermit/blob/ad3474118660fe31f5497193b1677f67a1bd51fb/hermit-cli/src/lib.rs#L2963):
`run_host_with_preload` / `run_host_with_output_and_preload` use
[`TracerBuilder<Detcore>`](https://github.com/rrnewton/reverie/blob/f97b7be1de4e2ef10ecc24cee5d8cc47f2fd254f/reverie-liteinst/src/backend.rs#L291).
`configure_host_command` at 545–566 prepends the backend DSO to `LD_PRELOAD`.
Detcore observes the process from exec, before the loader reads that DSO.
The direct in-guest `tool_host.rs` implementation is a different entry path.

In both attempts' run1 and run2 LiteInst logs, ordinary RDTSC records occur at
physical lines 14/15. The additional pair is at lines 20/44, with retired-branch
counts 3940/4584 and syscall counts 3/12. Between them are exactly nine inbound
syscalls: openat of the runtime DSO, read, fstat, five mmap calls, and close.
The next line is `access("/etc/ld-nix.so.preload")`, which the reference reaches
without that pair or DSO load. This is execution and accounting, not duplicate
formatting. The complete self-comparison INFO populations are 1165 candidate
versus 101 reference, with 423 versus 33 reported syscalls. Removing two lines
would leave the larger difference unresolved.

The dedicated LiteInst lane previously located the two physical instructions
inside the dynamic loader's `dl_main`, around `handle_preload_list`, on an older
fixed Hermit `b3c05c1` / Reverie `4db9ddb` diagnostic coordinate. Its retained
263,056-byte interpreter has SHA256
`e655ec5a68127ee03586457e67039ea9deef55abaeae519c1f53ad1cf18ee5b2`.
Offsets `0x2401e` and `0x24044` contain RDTSC (`0f31`); the intervening call uses
the `LD_PRELOAD` literal. This attributes that older pair to loader instructions
caused by loading the backend DSO, rather than instructions inside the DSO.

RUN1874's retained fixture names the same Nix interpreter,
`/nix/store/0d8g8n0a11v6f5m2h416ajyxmnkwc3md-glibc-2.42-67/lib/ld-linux-x86-64.so.2`,
and reproduces the bracketed load sequence. This supports the same preload
explanation, but its INFO logs contain no instruction addresses. The older
address capture is not a RUN1874 memory measurement. Even the older capture
followed the events by at least 25.2/39.8 ms, using held-file/map identity and
temporal association, rather than an instruction-time memory snapshot.

SaBRe's first difference is separately explained by initialization and log
transport. The built loader is the bundled
`experimental/reverie-sabre/vendor/sabre` at the recorded Reverie pin, not a
checkout inferred only from its upstream `third-party/sabre` gitlink. Its early
image callback initializes the guest's auxv before the guest dynamic linker.
The bootstrap transfers authenticated PRNG state and an auxv-completion fact;
the later Tool post-exec path consumes that fact instead of drawing again.
All retained SaBRe/reference logs contain identical AT_RANDOM bytes:
`[162,205,24,211,0,83,122,92,176,131,220,72,219,250,14,242]`.

However, plugin DETLOG messages are collected on stderr and
[`extract_sabre_detlogs`](https://github.com/rrnewton/hermit/blob/ad3474118660fe31f5497193b1677f67a1bd51fb/hermit-cli/src/bin/hermit/run.rs#L295)
appends them to the coordinator log after execution. In the retained SaBRe
files the early auxv line is physical line 4, while forwarded seed messages are
lines 36–37, after the coordinator report. This explains the record 3 order
mismatch without proving the entire initialization behavior equivalent.
SaBRe's vendor instruction marker -> Reverie callback ->
[`detcore-sabre` adapter](https://github.com/rrnewton/hermit/blob/ad3474118660fe31f5497193b1677f67a1bd51fb/detcore-sabre/src/lib.rs#L441)
can call the shared RDTSC handler. Its plain `info!` is not `detlog!`, and no
general tracing subscriber forwards it from the plugin. That establishes a
transport defect if delivered; zero text alone cannot prove instruction coverage.

The repair boundaries are therefore separate: KVM instruction delivery and
clock semantics; LiteInst backend-startup isolation; SaBRe instruction-delivery
proof plus faithful event transport/interleaving. These are investigation
locations, not approved designs. Opposing controls must retain real guest
RDTSC/RDTSCP values, callback delivery, loader behavior, the original pid-probe
order/output checks, and the unchanged same-run reference comparator. Ten is
this fixture's observation, not a universal acceptance count. No assertion,
tolerance, exemption, selected case, comparator, failure label, or check was
weakened. All self-verifications matched with exit 0; all three outer
cross-backend comparisons still failed.

The sealed evidence is retained on `devbig014` beneath
`/home/newton/work/dev-hermit/ignored/ci-hub/run1828-fix-forward-20260916/structured-results-obligation-audit-20260917/goal-continuation-20260918-0333/`:

- `sabre-parity-init-trace-1/REPORT.md` SHA256
  `fcf2f51a02fb888517b874e76ea072a21d3055842f0e8b5cec62cbba59a028ba`;
  its manifest is
  `670b85767c81b6cd6c1b1c4c2c2a627db57a63e496deb716a2c8bedd1e0c7880`.
  It binds the SaBRe source trace and eight complete candidate/reference logs.
- `sabre-parity-init-trace-2/REPORT.md` SHA256
  `d76c0db1fa4aebded47906bbf907ad4c1b62d5b61ef607bef87af0ed507e19c1`;
  its manifest is
  `370bc72063f1f4d4666d7f124c1901beadac933cedc3463acb09e625915b29bb`.
  It binds 32 additional raw files, exact source objects, and the older lane's
  report/ELF readback. `COMPLETE-LOG-READBACK.json` records original paths,
  immutable-copy hashes and stable inode/size/mtime checks.
- The original parity bucket is
  `ignored/validate/artifacts/validate-ops-tick-ad3474118660-f2cdd9e5165a/e2e/portable/manifest_backend_parity_c/results.jsonl`
  under the same parent repository. Its 10,625,591 bytes hash to
  `5fb9ca1d963e05407a556dc5d4de1e38c8e9fce454699b25ebbfbad45a5bbd41`.
- The older LiteInst capture report is
  `ignored/liteinst-01a0a13c-review/queue-drain-20260916/liteinst-bootstrap-current-observation/observer-identity-revision/service-control-revision/observer-failure-evidence-design/interpreter-capture-design/REPORT.md`
  under that repository; SHA256
  `da5f8876af14cd1e69aba3bbbb7d99b082a3d0e1dfd0c95e5b96a87c60cc323f`.

These raw artifacts remain local retained evidence, not files published by this
document. The durable report records their interpretation and identities; it
neither claims a backend fix nor changes the research task's disposition.
