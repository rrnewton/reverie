# Cross-backend syscall-counter validation

`validate-cross-backend.rs` runs the same `reverie-tool-sysctr` tool, linked
into `reverie-sysctr-ptrace` / `reverie-sysctr-kvm` / `reverie-sysctr-dbi` by
`reverie-multibackend-tools`, against controlled guest programs and checks that
the backends agree on syscall counts to the extent their architectures allow.

This is the executable form of task `impl-sysctr-cross-backend`: proving the
backends are *correct* Reverie implementations (they observe and aggregate the
right syscalls across a process tree), not just crates that compile.

## Run it

```bash
# ptrace + kvm binaries are built by reverie-multibackend-tools (default features)
cargo build -p reverie-multibackend-tools

# point the harness at the binaries (or set REVERIE_SYSCTR_BINDIR)
reverie-multibackend-tools/validation/validate-cross-backend.rs \
    --bin-dir target/debug --repeats 5
```

Exit status is non-zero if any hard gate fails.

## The guests

The KVM tool runner loads a **raw-machine-code static ELF** at a fixed address
and starts at the entry point; it does not run a libc program through an ELF
interpreter. So the harness emits raw-code static ELFs (byte-for-byte the layout
in `reverie-kvm/tests/static_elf.rs`) whose `p_vaddr - p_offset` is page-aligned,
which means **the same image is also directly `execve`-able by the Linux kernel**
and therefore runs under the ptrace backend too. Because the code is
hand-assembled, the syscall count is analytically known — agreement is checked
against a closed form, not merely between backends.

| guest        | code                                             | guest-issued syscalls | processes |
| ------------ | ------------------------------------------------ | --------------------- | --------- |
| `single_N`   | `N × getpid; exit_group`                          | `N + 1`               | 1         |
| `fork_K`     | parent forks `K` children, each `4 × getpid; exit_group`, parent `wait4`s each then `exit_group` | `K*4 + 3K + 1` | `K + 1` |

## What is (and is not) asserted

These binaries are the **bare Reverie backends plus a counting tool, not
`hermit --strict` / Detcore**. Bitwise-identical counts across runs is a Detcore
determinism property (L2); it is not expected at this layer. The harness
therefore gates on what is architecturally guaranteed here:

* **Single-process, hard gate.** `kvm == guest-issued syscalls` exactly, and
  `ptrace == kvm + 1`, stable across every repeat. The `+1` is the launching
  `execve` that ptrace observes and KVM has no equivalent for (KVM boots at the
  entry point, so there is no `execve` syscall in the guest).
* **Fork-tree, hard gate (ptrace).** aggregated `process_count == K + 1` on every
  repeat — direct proof the shared global state aggregates across the whole tree
  instead of fragmenting per-process.
* **Fork-tree, soft check (ptrace).** aggregated total is `>= analytic` and within
  a small tolerance above it. `wait4`/SIGCHLD restart races add an occasional
  syscall at the bare-ptrace layer, so an exact total is not gated (Detcore is
  what would make it exact).
* **KVM fork — informational.** the `run_static_elf_with_tool` adapter runs a
  single process and does not surface a fork tree to the tool, so KVM reports
  `across 1 process` for a fork guest and cannot witness fork-tree aggregation
  today.
* **DBI — skipped with reason.** DBI has no runtime tool selection: the tool must
  be baked into a DynamoRIO native client (`REVERIE_DBI_CLIENT`), which is not
  built for sysctr. Even with a client, DBI global state fragments across `fork`
  until the coordinator-RPC fix (`impl-dbi-global-state-fix`) lands, so DBI
  fork-tree counts would be per-process, not aggregated.

## Observed results

Backend: as named. Log level: default. Relaxations: none. Host: `/dev/kvm`
present; DBI env unset. Binaries: `reverie-multibackend-tools` debug build
(from slot140, uncommitted at time of measurement). `--repeats 10`, reproduced 3×.

```
[1] single-process agreement
  single N=3 : ptrace total=5  procs=1 ; kvm total=4  ; ptrace==kvm+1 ✓
  single N=5 : ptrace total=7  procs=1 ; kvm total=6  ; ptrace==kvm+1 ✓
  single N=10: ptrace total=12 procs=1 ; kvm total=11 ; ptrace==kvm+1 ✓
[2] fork-tree aggregation, ptrace
  fork K=1 : procs=2 (exact) ; total [9..9]   analytic 9
  fork K=2 : procs=3 (exact) ; total [16..16] analytic 16
  fork K=4 : procs=5 (exact) ; total [30..32] analytic 30  (wait4/SIGCHLD wobble)
[3] kvm fork guest: total=4 across 1 process  (single-process adapter; not gated)
[4] dbi: skipped (no per-tool client; global state fragments across fork)

6/6 hard checks passed
```

### Interpretation

* ptrace and KVM **agree exactly** on the syscalls a guest issues after entry;
  the only systematic difference is the launch `execve`, which is a genuine
  backend-semantics difference, not a bug.
* ptrace **correctly aggregates a fork tree** into one global tally (process
  count is exact and stable); KVM cannot be a fork witness with the current
  single-process adapter; DBI is blocked on the global-state fix.

## Follow-ups this validation pins

1. **DBI fork-tree aggregation** is untestable until a sysctr native client
   exists *and* `impl-dbi-global-state-fix` (coordinator RPC) lands. Re-run this
   harness with `DYNAMORIO_HOME`/`REVERIE_DBI_CLIENT` set; the `fork_K` gate
   (`process_count == K+1`) is exactly the regression test for that fix.
2. **KVM fork-tree** would become witnessable if a multi-process KVM tool runner
   drives `handle_syscall_event` for forked children; today only the VM-level
   test (`static_elf_forks_execs_and_waits_for_child`) exercises fork.
3. **Exact fork totals** (removing the `wait4` wobble) is a Detcore/`--strict`
   property; validate it through `hermit run --strict` once the tool is wired
   into a hermit backend, not at this bare-Reverie layer.
