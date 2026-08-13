# Host capability: CPUID faulting, and what it costs a receipt

2026-08-12. Task `devbig030-lacks-cpuid-faulting-limits-privileged-receipts`, raised by
hermit-901 while completing the privileged-e2e manifest work. Read-only investigation.

## The one-line rule

**`devbig030` cannot produce a full (fused portable+privileged) validation receipt today,
because its running kernel does not support CPUID faulting. `devbig014` can. Route full and
privileged-lane validation to `devbig014`. The portable lane is unaffected on both.**

Check before you start, on whatever box you are on:

```python
python3 - <<'PY'
import ctypes, os, sys
ARCH_SET_CPUID, SYS_arch_prctl = 0x1012, 158
pid = os.fork()                      # probe in a CHILD: enabling faulting makes any
if pid == 0:                         # later CPUID in that process trap
    libc = ctypes.CDLL(None, use_errno=True)
    os._exit(0 if libc.syscall(SYS_arch_prctl, ARCH_SET_CPUID, 0) == 0 else 1)
ok = os.waitpid(pid, 0)[1] == 0
print(f"{os.uname().nodename} kernel={os.uname().release} CPUID_FAULTING={'YES' if ok else 'NO'}")
sys.exit(0 if ok else 1)
PY
```

Exit 0 means the box can run the privileged lane; exit 1 means a full receipt will fail here
and the work must be routed. On devbig030 today it prints:

```
devbig030.atn3.facebook.com kernel=6.13.2-0_fbk15_hardened_0_g33ebba20e5e4 CPUID_FAULTING=NO
```

Fork matters: on a capable host, enabling faulting in your own process makes every later
`CPUID` instruction in it trap. The Hermit test forks for the same reason
(`detcore/tests/misc/mod.rs:35`).

## Exactly what requires the capability

One test, reached by one DAG node. This is a narrow gap, not a broad "no privileged work
here".

| | |
| --- | --- |
| test | `hermit-detcore` `tests_misc::rdrand_rdseed_is_masked`, `detcore/tests/misc/mod.rs:1181` |
| node | `cpuid.faulting` in `hermit/ci/dag/privileged.json` |
| command | `cargo test -p hermit-detcore --test tests_misc rdrand_rdseed_is_masked -- --exact` |
| what it proves | Detcore masks the host's RDRAND/RDSEED feature bits from the guest, which requires intercepting `CPUID`, which requires CPUID faulting |

The test **asserts rather than skips**, deliberately:

```rust
assert!(
    cpuid_faulting_supported(),
    "ERROR: rdrand_rdseed_is_masked requires host CPUID faulting support"
);
```

That is the correct design and should not be "fixed" by turning it into a skip. A skip would
make an incapable box look green while the masking behaviour went unmeasured. Report the
host limitation and route the work; do not weaken the assertion.

Profiles:

- **portable lane** — unaffected. `hermit/ci/dag/portable.json` has no CPUID node, and one of
  its nodes explicitly runs Hermit with `--no-virtualize-cpuid`.
- **privileged lane** — one of its nine nodes needs it.
- **full / fused plan (the default)** — cannot be green without it. `hermit/scripts/validate.rs`
  line 927 hard-requires the node to be present in the fused plan:
  `for required in ["test.strict_compat", "privileged-cpuid.faulting"]`.

Hermit itself degrades rather than dying: `reverie-ptrace/src/task.rs:1825` logs
`ARCH_GET_CPUID reported a valid state, but ARCH_SET_CPUID returned ENODEV … continuing
without CPUID interception`. So ordinary `hermit run` work still functions on devbig030 —
what fails is the test that asserts the masking actually happened.

## How each machine's capability was established

### devbig030 — direct measurement

`arch_prctl(ARCH_SET_CPUID, 0)` returns **ENODEV (19)**, both to enable and to disable, while
`ARCH_GET_CPUID` returns 1 (a valid state). That is precisely the state Reverie's message
describes. Supporting facts, all read off this box:

```
CPU        AMD EPYC 9D85 158-Core Processor (AuthenticAMD, family 26, model 17)
kernel     6.13.2-0_fbk15_hardened_0_g33ebba20e5e4
/proc/cpuinfo   no cpuid_fault flag
```

Mechanism, from kernel source rather than inference. `arch/x86/kernel/process.c:379` gates on
`boot_cpu_has(X86_FEATURE_CPUID_FAULT)` and returns `ENODEV` otherwise. In the 6.16-rc7 tree
available locally (`ignored/linux`, commit `f6a6c280059c`) that bit is set in exactly one
place — `arch/x86/kernel/cpu/intel.c:495` — and `cpufeatures.h:185` names it
`/* "cpuid_fault" Intel CPUID faulting */`. An AMD host therefore needs a kernel that adds
AMD support.

### devbig014 — behavioural, from the exact consumer

I could **not** probe devbig014 directly: `ssh devbig014` returns
`Permission denied (publickey,password)` for this account. The capability claim rests on the
gate's own recorded outcomes in `ledger/hermit/devbig014/2026-08.jsonl`:

```
privileged-cpuid.faulting   137 runs, 137 pass, 0 fail
most recent                 2026-08-12T20:16:34Z, exit 0
```

That is evidence from the running thing — the exact node that would have to pass — rather
than from a hardware inventory. It does not tell us devbig014's CPU or kernel, only that the
capability was present there as recently as today.

## When devbig030 lost it

| when (UTC) | what |
| --- | --- |
| 2026-08-09T23:51 → 2026-08-11T02:48 | 54 consecutive `privileged-cpuid.faulting` **pass** |
| 2026-08-11T05:54 (2026-08-10 22:54 PDT) | `kernel-6.13.2-0_fbk15_hardened` rpm installed |
| 2026-08-11 06:00 and 07:02 (23:00 / 00:02 PDT) | `wtmp` records boots onto 6.13.2-fbk15 |
| 2026-08-12T18:39 → 2026-08-13T02:22 | 5 consecutive **fail**, `exit 101`, ~0.1 s each |

No CPUID-gate run happened between the last pass and the kernel install, so the ledger
brackets the change without witnessing it. The kernel the 54 passes ran under is no longer
installed (`rpm -qa` lists only 6.13.2-fbk15 and 6.19.2-fbk0), so it could not be identified.

## Is a reboot the fix? Unresolved, and not mine to try

A **6.19.2** kernel was installed on devbig030 today (`/boot/vmlinuz-6.19.2-0_fbk0_hardened_rc25_0_g1d99d2f6053c`,
rpm install time 2026-08-12 14:37 PDT). Reverie's message asserts "On AMD hosts, use Linux
6.17+ upstream or a kernel with CPUID faulting backported", which makes that kernel a
candidate remedy.

Evidence for, checkable without booting: `MSR_K7_HWCR_CPUID_USER_DIS` — the AMD CpuidUserDis
control bit — is **absent** from `6.13.2-fbk15`'s `msr-index.h` and **present** in
`6.19.2-fbk0`'s.

Evidence against over-reading that: both trees still describe `X86_FEATURE_CPUID_FAULT` as
"Intel CPUID faulting", and a header define proves the source knows the bit, not that the
`arch_prctl` path sets the feature for AMD. Only the `.c` files would settle it and
`kernel-devel` does not ship them.

So the reboot is **plausible but unverified**. Settling it means booting devbig030 onto
6.19.2 and rerunning the probe above. That is an owner decision, not an agent action: this box
carries live agents. Note this supersedes the earlier belief that restoring the capability
would require a reprovision because no other kernel was installed — that was true when
written and is no longer.

## Three separate constraints on this box — do not merge them

Merging any two of these produces a wrong routing rule.

1. **CPUID faulting (this note).** A running-kernel capability. Blocks exactly one node, hence
   the privileged lane and the full plan. Measured above.
2. **The sandbox boundary** (agents cannot create systemd units). An agent-permission
   constraint, unrelated to hardware, and it does not care which lane you run.
3. **Load and capacity.** A scheduling concern. Separately measured earlier: 316 cores, load
   ~80 is ~25% with zero PSI, so load alone is not contention.

And within (1), do not widen the rule to "no privileged work on devbig030":

- **PMU works on devbig030.** `privileged-pmu.preemption` records 57 genuine passes and only 5
  collateral aborts, with a genuine pass at **2026-08-13T01:30:34Z** — after the reboot and
  after the CPUID failures began.
- **A CPUID failure is one failure, not many.** Every failing run records `failures=1`. The
  13–14 other non-passing gates in those runs all carry
  `ABORTED (eager-exit after another step failed)`; the CPUID node dies in ~0.1 s and the
  runner's eager-exit cancels whatever was still in flight. An earlier note of mine described
  this as the cell "killing four nodes" — that was collateral, not four capability failures,
  and it is corrected here.

## What an agent should do

- Running the **portable** lane on devbig030: fine, nothing here applies.
- Needing a **full** or **privileged** receipt: run the probe first. On devbig030 it exits 1;
  route the run to devbig014 rather than starting one that will die in 0.1 s and abort a
  dozen sibling gates.
- Seeing `privileged-cpuid.faulting … exit 101` in a devbig030 log: that is this capability
  gap, not a code defect in the commit under test, and **not** a flake. Re-running on the same
  box cannot help. "NEEDS-RERUN" is the wrong advice here.
- Never convert the assertion into a skip to make a local run green.

## Commands used

```sh
# capability, directly
python3 <the probe above>

# the machine
lscpu | grep -E 'Vendor ID|Model name'; uname -r; grep -c cpuid_fault /proc/cpuinfo

# the mechanism, from source
grep -rn X86_FEATURE_CPUID_FAULT ignored/linux/arch/x86/
grep -n MSR_K7_HWCR_CPUID_USER_DIS /usr/src/kernels/*/arch/x86/include/asm/msr-index.h

# what needs it
grep -rn cpuid_faulting_supported hermit/ --include=*.rs
grep -n '"group": "cpuid"' -A6 hermit/ci/dag/privileged.json

# per-machine history, from the exact consumer
#   ledger/hermit/<machine>/<UTC-year>-<UTC-month>.jsonl, gate name privileged-cpuid.faulting

# when it changed here
rpm -qa --qf '%{INSTALLTIME:date}  %{NAME}-%{VERSION}-%{RELEASE}\n' | grep kernel
last -x reboot | head
```

## Not established

- devbig014's CPU or kernel (no SSH access from this account); only its gate history.
- Any machine other than devbig014 and devbig030 — those are the only two with ledgers in
  `ledger/hermit/`.
- Whether booting devbig030's installed 6.19.2 kernel restores the capability.
- Whether any test other than `rdrand_rdseed_is_masked` depends on CPUID interception
  *implicitly* — Hermit continues without interception rather than failing, so a test that
  silently relies on masked CPUID would not name this cause. Nothing in the current ledger
  suggests one; `failures=1` in every affected run.
