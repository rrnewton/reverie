Background: this increment implements immediate KVM epoll_pwait2 readiness for
proven guest-created eventfd registrations. It deliberately leaves temporary
masks, blocking waits and untracked/escaped descriptor histories unsupported.

Tracking: https://github.com/rrnewton/reverie/issues/905
Base: 43fce717f1c7710b7ef7345e4146821422a3aaa6
Slot: kvm-epoll-pwait2-20261003, generation 1.

Design sources:
- /tmp/kvm-epollpwait2-design-c87ygpse/REPORT.md
- /tmp/kvm-epollpwait2-addendum-iej5w3tI/ADDENDUM.md

Source-only phase. No builds, probes or tests have run. Cargo remains on the
parent's hold. Neither this report nor the design reports are review approval.

TODO:
- Implement shared provenance, logical binding retirement and lifecycle hooks.
- Implement checked zero-time polling and exact kernel copyout.
- Add strict unit/lifecycle/native copyout tests and causal mutant recipes.
- Integrate the separately assigned native/direct/Tool fixture and mandatory
  KVM completion controls.
- Format and inspect the complete diff, then report readiness for qualification.

The DSR guide was read. No scheduler, guest signal-delivery algorithm, timeout,
or record/replay format changes are in scope; no new complete DSR approval is
claimed. Hermit's separate replay defect remains
https://github.com/rrnewton/hermit/issues/3574 .
