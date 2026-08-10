---
name: hermit-linux
description: "Task charter for QEMU/Linux integration, kernel testing, deterministic VM boot and snapshot-resume, Linux record/replay, and SCX/sched_ext scheduler coverage. Load when dispatching Linux VM, kernel, QEMU snapshot, or sched_ext work to a numeric agent."
---

# hermit-linux - Linux VM and kernel workstream

## Purpose

Advance Hermit's ability to run a real Linux guest under QEMU reproducibly.
Own the path from cold boot through deterministic snapshot-resume, substantive
userspace and kernel workloads, Linux record/replay, and `sched_ext` (SCX)
scheduler tests. Each task must either raise a named current-main gate or
root-cause one specific blocker with replayable evidence.

## What this agent owns

- QEMU/Linux cold-boot and VM-state snapshot-resume coverage under Hermit,
  including strict verification and record/replay diagnosis.
- Reproducible kernel, initramfs, root filesystem, and userspace test fixtures;
  kernel configuration and boot-command-line control; and guest-side oracles.
- SCX/sched_ext scheduler bring-up and deterministic workload coverage,
  including kernel configuration, BPF/toolchain inputs, scheduler identity,
  and workload outcome evidence.
- Durable Linux VM experiment manifests and textual results under the parent
  `experiments/` tree only when the task explicitly names and owns that path.

## Constraints

- **Keep gates distinct.** Report cold boot, snapshot creation, snapshot
  resume, strict verification, and record/replay as separate outcomes. A VM
  that boots twice is not automatically a deterministic snapshot-resume or
  replay result. Bind every claim to the exact Hermit and Reverie SHAs,
  backend, mode, QEMU version, kernel/config, initramfs/rootfs hashes, command
  line, host facts, seed, and observed oracle.
- **Do not hide strict failures with a relaxed run.** A fail-open or non-strict
  control may isolate a blocker, but label it diagnostic and preserve the
  literal strict failure. Use the project's L0/L1/L2 rubric and state every
  relaxation.
- Strict verification compares exit status, stdout, stderr, and complete INFO
  logs byte-for-byte. Do not normalize away numeric VM or scheduler evidence.
- **Make VM state repeatable.** Keep the kernel and base disk immutable. Create
  a fresh per-run qcow2 overlay or copy because QEMU `loadvm` can update image
  metadata. Pin machine type, vCPU count, accelerator, clock/icount settings,
  device layout, firmware, kernel command line, and snapshot point. Compare
  guest oracles and Hermit logs across resumes from the same captured state.
- **Make kernel and SCX inputs reconstructable.** Record the kernel source SHA,
  config (including `CONFIG_SCHED_CLASS_EXT` where applicable), compiler and
  BPF toolchain versions, SCX scheduler source SHA/name, workload, and guest
  output. Do not claim scheduler determinism from boot success alone.
- **Large artifacts stay out of Git.** Never commit kernel images, initramfses,
  root filesystems, qcow2 images, VM snapshots, BPF objects, traces, or build
  trees. Store them in ignored/external storage and commit a text manifest with
  producing commands, versions, paths, sizes, and SHA-256 checksums. Durable
  question/method/results belong in `experiments/<name>_YYYYMMDD/`.
- Use the roadmap at
  `experiments/linux-vm-roadmap_20260726/README.md` as historical evidence,
  then remeasure current main rather than repeating a stale pass count.

## Worktree assignment

The coordinator assigns this workstream to a numeric agent in a generic
**`worktrees/slotNN/{hermit,reverie,liteinst2}`** slot and registers it with
`scripts/allocate-worktree.rs --agent <neutral-agent-id> --slot slotNN --task <task-id>
--product all --purpose "<one-line>"`. Create a Reverie branch only when a real
lower-level change is required; otherwise leave that child and LiteInst2
detached at their parent gitlinks. Follow the cross-repository change protocol
in `AGENTS.md`. Parent experiment manifests require explicit task ownership and
a separate parent commit; product code and tests remain in their owning
repository. Never build or edit in a
primary checkout, and never share writable kernel, QEMU, or Cargo build
directories between slots. See
`ai_docs/transient/2026-07-27-worktree-management-map.md` for the full protocol.

## Related

- Linux roadmap: `experiments/linux-vm-roadmap_20260726/README.md`.
- Performance methodology: [hermit-opt](../hermit-opt/SKILL.md).
- KVM backend boundaries: [hermit-kvm](../hermit-kvm/SKILL.md).
- Claim auditing: [backend-reality-reviewer](../backend-reality-reviewer/SKILL.md).
- Reports: [progress-rubric](../progress-rubric/SKILL.md).
- Landing discipline: [post-facto-review](../post-facto-review/SKILL.md).
