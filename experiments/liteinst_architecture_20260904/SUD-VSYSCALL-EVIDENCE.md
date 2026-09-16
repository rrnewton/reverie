# Legacy vsyscall bypasses SUD on this host

## Measured result

**All three legacy x86-64 calls succeeded without entering the installed SUD
dispatcher.** Native/kernel results were returned instead of the dispatcher's
sentinels. Raw syscall controls immediately before and after each legacy call
each entered that same dispatcher exactly once and returned its sentinel output.
This is not a disabled mapping, ENOSYS, SIGSEGV or failed installation.

| Legacy entry | SUD legacy result | All dispatcher callbacks across legacy call | Same-process raw controls |
|---|---|---|---|
| gettimeofday, `0xffffffffff600000` | rc0, seconds1788578686, usec934067 | 5 -> 5 | Each rc0, seconds123456, usec789; +1 callback |
| time, `0xffffffffff600400` | return/output1788578686 | 5 -> 5 | Each return/output123456; +1 callback |
| getcpu, `0xffffffffff600800` | rc0, CPU233/node0 | 5 -> 5 | Each rc0, CPU61/node62; +1 callback |

Interception observations: **legacy 0/3; ordinary raw controls 6/6**. These are
three selected ABI entries and their bracketing controls, not corpus coverage.
All six native/SUD processes exited0. Native legacy outputs were respectively
1788578686.925346, 1788578686, CPU72/node0; all native dispatcher counters stayed0.
CPU numbers need not match between separately scheduled native/SUD processes.
The errno sentinel777 stayed unchanged; it is not an error status.

## Method and exact source

External dynamic Rust executable links the actual unchanged `reverie-preload`
controller from Reverie **8e8d36adf2ea220c35a5d2bc0c6a41aa90f92675**. It calls
public `install(..., InProcessUserDispatch, RuntimeConfig::default())`; alternate
stack=true, null selector, only the existing narrow shared syscall gate trusted.
Every process prints its actual trusted range; none includes the legacy page.
No LiteInst typed Tool, shared Detcore, hosted route or product modifications.

The dispatcher returns distinct results/output buffers for the three syscall
numbers and validates UserDispatch provenance. Native controls call libc's raw
`syscall` wrapper, not vDSO functions. Legacy controls call the fixed ABI function
entries with valid writable buffers. An all-dispatch atomic counter is sampled
immediately around each invocation, excluding diagnostic writes. Target-specific
counters also remain1 across the legacy call and advance to2 on the trailing raw
control. No selector/control change occurs between the controls.

Two external-probe revisions are retained: round1 counted target callbacks;
round2 additionally counts *all* dispatcher entries, excluding a misclassified
syscall-number explanation. Round1 source/binary/logs remain under `round1/`.
No assertion or product configuration was relaxed in the second revision.

- Final observation: **2026-09-04T20:24:46-07:00**, devbig014, Linux
  `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, x86-64.
- Every process: personality `00000000`; mapping
  `ffffffffff600000-ffffffffff601000 --xp [vsyscall]`.
- Installed kernel configuration: `CONFIG_X86_VSYSCALL_EMULATION=y`,
  `CONFIG_LEGACY_VSYSCALL_XONLY=y`, NONE unset. Boot command line retained;
  no explicit vsyscall override appears there.
- Probe source SHA256:
  `6a8344747e10fcf7d2dd55f138cb0a6434950b39f56a516f0746dbcea1507dfe`.
- Actual executable SHA256:
  `8ba9fa5564a5bcd8f00ff91812fa5347897d7e6db262a06f34455e473b936295`.
  Dynamic PIE; interpreter `/lib64/ld-linux-x86-64.so.2`.
- Controller source-manifest SHA256:
  `ee322c342d9d0b5c9ce09fdec5391d2266ffe8a901840dd9eb5d76921a6c58ba`;
  all source hashes match after execution. Product worktree remains clean at8e.
- Rust nightly2026-07-29, rustc commit
  `26ae60a9eeb20b4935be49d7a931a650fa1d2923`; libc0.2.186 locked externally.

## Commands, bounds and retained evidence

Root: `/home/newton/work/dev-hermit/ignored/liteinst-vsyscall-sud-20260904/`.
`admission.json` records the exact offline build and six execution commands;
`admission.log` records observed two-core CPU quota, 2GiB memory/no swap and
descendant cleanup. Build timeout300s; execution step90s; each child10s with
2s kill grace, zero cores, 1MiB output-file cap. The runner command was:

```text
DAGRUN_ENGINE=python /home/newton/work/dev-hermit/agent-utils/bin/dagrun run --dag /home/newton/work/dev-hermit/ignored/liteinst-vsyscall-sud-20260904/admission.json --max-steps 1 --max-cpus 2 --max-mem 2G --admission 0 --no-profile-feedback --run-timeout 420 --perf-dir /home/newton/work/dev-hermit/ignored/liteinst-vsyscall-sud-20260904/profile-round2
```

Both native and SUD cases use the existing `deny-ptrace` launcher, whose inherited
filter kills ptrace and allows other syscalls; stderr verifies seccomp2 and
no_new_privs1. This filter does not supply SUD's interception. No Hermit invocation
occurred. Final orchestration exit0 only means the steps completed; individual
outcomes are the six retained `.status` files, not inferred from that exit.

`results.json` contains exact count/result rows. `native-*.stdout` and
`sud-*.stdout` contain full before/after arrays, total counts, mapping/personality,
config/ranges and results; `.stderr` and `.status` are adjacent. Source, Cargo
manifest/lock, executable, ELF headers, host/config/header evidence and prior
round are retained. No public network, recursive agent, or Pasteur-tree access.

## Interpretation and limits

This establishes a **host-specific legacy-vsyscall interception gap**. The
installed headers declare vsyscall fault-emulation entry points and configuration
enables emulation. The measured successful special-page calls without SUD entry
are consistent with that kernel-emulated route. The complete kernel implementation
and precise internal call chain were not available or traced; do not claim an
all-kernel proof or infer behavior under disabled mapping, another personality,
other boot settings or a different kernel. No such negative environment was run.

The qualified claim about *ordinary native64 syscall instructions* while SUD is
armed, outside its trusted range and without selector exemption is not disproved:
the raw controls demonstrate it here. But it cannot be broadened to every way
of requesting Linux time/CPU services. Legacy vsyscall is now an **observed**
exception rather than merely an unverified caveat, alongside the separate vDSO
non-syscall issue. A no-patch Detcore design must explicitly intercept or refuse
this route; no fix or activation is implemented by this measurement. No typed
Tool/Detcore, CLI/L2, timer/clock or backend-readiness result follows. All340
reference labels/static deferrals and >=306 target remain unchanged.
