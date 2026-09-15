# KVM branches before landing, 2026-09-15

This record preserves the public branch tips before the planned rebase landings.
It records source identity, not a claim that either change has landed.

| Repository | Branch | Recorded tip | Pull request |
| --- | --- | --- | --- |
| `rrnewton/hermit` | `codex/kvm-parity-20260914` | `e340d7f9160f5eb5ca052adf2ce05708b0a3790b` | https://github.com/rrnewton/hermit/pull/3011 |
| `rrnewton/reverie` | `astra-reverie-vectors` | `5f2ccf2167116248968f0abfd7cbf6cbb47280ea` | Private continuation of https://github.com/rrnewton/reverie/pull/538; its public head is unchanged |

Hermit's recorded tree is `aa43c21fbac1467ede19d6791efd7802d57aadf4`.
The change prevents inherited child time from being charged twice, preserves
that accounting through exec, includes the clock type in SaBRe's RPC layout
fingerprint, and repairs two inherited Clippy errors using type aliases.
At this head, 756 library tests and the complete child-time RPC test passed.
Full managed validation is running; no complete receipt is claimed.

Reverie's recorded tree is `7ff8bc8d1e214f4d538e2b05241ef58e45fb54ec`.
Import repair `5e861c51dd04bac3e04556a7ba89a1f3d73173c7` and the separate
standard-descriptor repair at the recorded tip address distinct review findings.
The latter retains KVM's original stdout/stderr read policy while preserving
guest-owned replacements and duplicated-descriptor routing. It passed 673
package tests with one existing ignored doctest. Independent review approves
the repair's scope, without clearing the inherited stack or claiming Linux
descriptor parity. Public pull-request head
`dd8930e66ad895b40620f2fcd8299d5ad38a082a` remains unchanged.

The active task owner is `dev-hermit-14` on both
`kvm-lane-to-full-determinism-and-parity` and
`kvm-vector-import-repair-astra-reverie-vectors`. TaskGraph notes retain progress
and evidence under those tasks. The owner authorizes speculative soft-green
landing with rebase; a full receipt is not a prerequisite. The overall goal of
100 percent KVM determinism and parity remains active and is not achieved by
these changes. Existing strict signal-delivery failures remain failures.
