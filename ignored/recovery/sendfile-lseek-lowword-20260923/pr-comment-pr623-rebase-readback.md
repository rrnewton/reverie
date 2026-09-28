[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

Rebased the corrected PR onto current main `96598dc07490ec15411845d27ed5bfd5c0a34076` after PR 623 landed. PR 623 changed only `reverie-ptrace/*`; there was no source or test overlap, and the two KVM files remain byte-identical to the prior frozen correction.

New exact head: `748321a70995f1e7c1d5385de09f620f4becd685`

- full diff SHA-256: `af4e2e7f20c4262b3e0cc4eca4ada9382c55a2660da555bcca985564ca538d9c`
- correction-only SHA-256: `30777bf3ee93ac725b48e10f55e68f0110713e5ace560feb6efa45beca5a4b41`

Exact-head gates are green: fmt, diff check, all-target clippy with `-D warnings`, three focused unit cells, required real KVM (native once, direct twice, Tool twice), full library parallel 819/819, and full library serial 819/819.

Eight non-equivalent mutants are killed by both unit and KVM cells: three restored strict decoders, three bit-31-masking decoders, late output validation, and missing early writability validation. Removing only the explicit negative branch is correctly classified as equivalent because the stronger general output lookup still returns `EBADF`; it is not counted as a kill.

Three fresh internal exact-artifact reviews found no correctness defect or goalpost moving. The old `79f139bd...` target is non-final. This rebased head is frozen and remains held for a new independent Claude-family exact-head review relayed by the primary coordinator.
