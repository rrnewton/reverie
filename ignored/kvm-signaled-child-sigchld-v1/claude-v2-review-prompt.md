You are the independent Claude-family read-only adversarial reviewer for a critical Reverie KVM signal-boundary change. This is a FRESH exact-head review after a prior reviewer found documentation and coverage follow-ups. Do not modify any file, run any command, access GitHub/network, or trust any prior verdict. Do not read claude-review-output.jsonl. Your available tools are intentionally limited to Read, Grep, and Glob.

Repository root: /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918
Base: 78203cd45751cba5f86f1e7ad5c545aceb29c017
Head: afd9ddc64d21967c6b1a9e203ffcd06964a4128a
Tree: 613abcec9ac50416dc36873340ed8c3547f1210a

Intended behavior: accept coherent process-directed terminal SIGCHLD events for CLD_EXITED, CLD_KILLED, and CLD_DUMPED at both initial publication and Tool-filter return; preserve the same child fields through signalfd; reject incoherent class/status combinations before publication. Do not infer full KVM parity or Hermit consumer success from this component.

Read every byte of these primary inputs before giving a verdict:

1. ignored/kvm-signaled-child-sigchld-v1/exact-afd9ddc.patch
2. ignored/kvm-signaled-child-sigchld-v1/claude-v2-inputs.sha256
3. reverie-kvm/src/executor.rs around all patched validation, publication, Tool-return, signalfd, default-disposition, and terminating-status paths
4. reverie-kvm/src/child_exit_signal_tests.rs
5. reverie/src/guest.rs around the changed public contract
6. reverie-kvm/README.md around bounded signal delivery
7. ai_docs/kvm-child-exit-signals.md
8. reverie-kvm/src/process_signal_publication.rs and relevant producer/lifecycle paths
9. ignored/kvm-signaled-child-sigchld-v1/exact-afd9ddc-focused.status and its referenced log
10. ignored/kvm-signaled-child-sigchld-v1/exact-afd9ddc-lib-serial.status and its referenced log
11. ignored/kvm-signaled-child-sigchld-v1/exact-afd9ddc-clippy-all-targets.status and its referenced log
12. ignored/kvm-signaled-child-sigchld-v1/exact-afd9ddc-static.status
13. ignored/kvm-signaled-child-sigchld-v1/REMOTE-CI-CLASSIFICATION.md

Review the complete five-file patch, not merely the follow-up delta. Confirm specifically whether all earlier issues are resolved: both contract documents accurately name the three classes and class-specific status domains; `CLD_KILLED + SIGSEGV` exercises receiver/Tool-return and signalfd encoding; the validator fallback fails closed; the included test file is directly rustfmt-clean. Recheck Linux ABI/semantics, producer/receiver consistency, process ownership/coalescing/lifecycle behavior, error classes, and determinism from scratch. Treat missing or misleading documentation as a finding. Distinguish blocking from nonblocking findings explicitly.

GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it

Compare every changed test, threshold, allowlist, skip, label, classification, and gate with the base revision. Determine whether the implementation now meets the original requirement or whether the requirement was changed to admit the implementation. Call out either result explicitly, including when no lowering of the bar is present.

Required report structure:

Review target: Reverie <base>..<exact head>, tree <tree>

Findings:
- <severity>: <file:line> — <problem, impact, and required change>
- If none, say exactly that.

Determinism and Linux/POSIX assessment:
- State what changes and what does not.

Goalpost-moving assessment:
- Assertions weakened: yes/no — evidence
- Tolerance widened, exemption added, case skipped, or comparator relaxed: yes/no — evidence
- Failure renamed or relabelled as a pass: yes/no — evidence
- Check deleted instead of satisfied: yes/no — evidence

Verification and limits:
- List inputs inspected, exact evidence credited, and missing evidence.

Verdict: APPROVE or CHANGES REQUESTED, bound to the exact head and tree. Findings come first; do not hide a blocker in prose.
