# Provenance

Evidence from the independent adversarial review of
`https://github.com/rrnewton/reverie/pull/520` ("Fix indexed xstate CPUID entries
in reverie-kvm"), carried out on 2026-09-04 by agent `review-520b` under task
`independent-adversarial-review-of-reverie-520-cpuid-table-fix`.

The probe in this directory is the evidence behind an assertion that is now in
reverie's tree. It is recorded here so the check and its justification have the
same lifetime.

## Revisions

| role | SHA |
|---|---|
| reverie base (`origin/main` throughout the review) | `ce841d744cc74b1627ac52b42f711b33b1c72a45` |
| PR 520 head, first review pass | `3bf292b269daa85b6e3b01ef4f435dc89ea0df30` |
| PR 520 head, re-review and withdrawal | `4fcf9df57dc16f135453d1bea1f8c6cc4b26dcce` |

`3bf292b2` is an ancestor of `4fcf9df5`; the base did not move between the two
passes.

## Published verdicts

- Changes requested at `3bf292b2`:
  `https://github.com/rrnewton/reverie/pull/520#issuecomment-5540558669`
- Withdrawal and approval at `4fcf9df5`:
  `https://github.com/rrnewton/reverie/pull/520#issuecomment-5540822517`

## Environment

- Host: devbig014, AMD EPYC 9D85 158-Core, 316 `cpuid_fault` flags in
  `/proc/cpuinfo`, `/dev/kvm` present and openable read-write from an ordinary
  agent sandbox.
- Host libc: GNU libc 2.34. Host compiler: gcc 11.5.0 20240719 (Red Hat
  11.5.0-15).
- Rust: cargo 1.99.0-nightly (3efb1f477 2026-07-17), rustc 1.99.0-nightly
  (26ae60a9e 2026-07-28); reverie pins `nightly-2026-07-29`.
- Container image, matched by digest to the one PR 520 names:
  `localhost/hermit-hermetic-validate@sha256:e38c3b2d5cd8a17ed2f99b4a24dd76c9ee63329cdc8c9723e4650ab085fae985`
- Guest libc inside that image: GNU libc 2.42, interpreter
  `/nix/store/0d8g8n0a11v6f5m2h416ajyxmnkwc3md-glibc-2.42-67/lib/ld-linux-x86-64.so.2`.

## Files

- `review520_probe.rs` — the probe, five tests, written to sit at
  `reverie-kvm/tests/review520_probe.rs`. It uses only reverie-kvm's public API
  (`KvmBackend::install_static_elf_with_context` and
  `KvmBackend::run_static_elf_captured`) and compiles unchanged at all three
  revisions above.
- `RERUN.md` — how to reproduce every measurement.
- `RESULTS.md` — what was observed, with the numbers.
- `logs/` — the captured output the results were read from.
- `CHECKSUMS.sha256` — over every file here except itself.

## Scope and limits

Single host, single kernel, single CPU vendor. The Hermit-side figures in PR 520
(the `backend-parity-c/cpuid-probe` cell, the 89 | 89 compared INFO messages,
`BitwiseInfoV1`, the `hello-nostdlib` control) were NOT reproduced; no Hermit was
run at any point. This evidence reaches the same fault at the same address from
reverie alone, which corroborates the mechanism and is not a check of those
figures.
