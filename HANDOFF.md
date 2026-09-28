# review-520b — independent adversarial review of reverie PR 520

Task `independent-adversarial-review-of-reverie-520-cpuid-table-fix` is CLOSED.

- Verdict published: https://github.com/rrnewton/reverie/pull/520#issuecomment-5540558669
- Reviewed head: 3bf292b269daa85b6e3b01ef4f435dc89ea0df30 (base ce841d744cc74b1627ac52b42f711b33b1c72a45)
- Verdict: changes requested, two small edits. The CPUID repair itself is correct
  and was independently reproduced; no goalpost moving on any of the seven axes.

## What is in this slot

`reverie-kvm/tests/review520_probe.rs` is untracked reviewer scratch, NOT proposed
for landing. It contains four probes used for the A/B:
`probe_dump_all_xstate_subleaves`, `probe_lazy_puts_only`, `probe_eager_puts_only`,
`probe_host_xstate_leaf`. The tracked tree is byte-identical to 3bf292b2.

## How to rerun the glibc 2.42 A/B without Hermit

Build the probe binary twice (once at PR head, once with only
`reverie-kvm/src/cpuid.rs` reverted to base), then run both inside the pinned
image with the host's loader supplying the harness's own libc:

    podman run --rm --device /dev/kvm \
      -v /tmp/probe-head:/probe-head:ro -v /tmp/probe-base:/probe-base:ro \
      -v /usr/lib64:/hostlib64:ro \
      localhost/hermit-hermetic-validate@sha256:e38c3b2d5cd8a17ed2f99b4a24dd76c9ee63329cdc8c9723e4650ab085fae985 \
      /bin/sh -c 'ln -sf /bin/gcc /usr/bin/gcc;
        /hostlib64/ld-linux-x86-64.so.2 --library-path /hostlib64 /probe-base --nocapture --test-threads=1'

The harness runs on the host's glibc 2.34; the guest C program is compiled by the
image's gcc and so links against glibc 2.42. That is the configuration that
matters, and it needs no cargo inside the container.

Base leg reproduces `GuestException { vector: 13, instruction_pointer: 16862636 }`,
i.e. RIP 0x1014dac, the address the PR reports from Hermit. Head leg is clean.

## Update — withdrawal recorded 2026-09-04

Re-reviewed at head 4fcf9df57dc16f135453d1bea1f8c6cc4b26dcce (one commit on top of
3bf292b2; base unchanged). Changes-requested marker WITHDRAWN, verdict approve:
https://github.com/rrnewton/reverie/pull/520#issuecomment-5540822517

Findings 3, 4 and 5 still stand and were NOT resolved by the withdrawal.

The probe file now has 5 tests; `probe_bind_now_markers_on_an_eager_binary` was
added during the re-review and is what shows that a `DF_BIND_NOW`-only check
would have been inert on this toolchain (an eager binary here has DT_BIND_NOW and
DF_1_NOW but no DT_FLAGS entry at all).

## Probe landed 2026-09-04

The probe, its raw logs, and the rerun recipe are now tracked at
`ai_docs/evidence/reverie-520-cpuid-probe-20260904/` on dev-hermit main,
commit `ea3111e32bc2cddd1e70536bafeabfa864836581`. 26 files, verified by reading
every one back from `origin/main` and re-checking it against `CHECKSUMS.sha256`.

The copy in this slot is now a duplicate, not the only copy. This slot can be
reclaimed without losing anything.
