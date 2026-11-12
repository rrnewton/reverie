# Dynamic load-instance validation

Candidate-06 remains the one and only execution of that artifact. This packet
contains source-only validation for its `target range is not readable` root
cause and for the subsequent adversarial-review blocker. No candidate was
retried and no marker was generated.

The prior raw-or-rebased resolver authenticated permissions, mapping identity,
and a 32 MiB bias window but did not prove that a same-inode mapping belonged to
the `link_map` node's load instance. A duplicate mapping could therefore be the
only valid Direct or Rebased candidate.

The corrected observer binds each file-backed node's in-memory ELF header and
program headers to `l_addr` and `l_ld`, requires one exact `PT_DYNAMIC`, and
stores every file-backed `PT_LOAD` with its runtime span, file offset, and ELF
permissions. A metadata range is valid only when its full nonzero extent is in
one `PF_R` load and every covering current mapping is readable, private,
non-writable, identity-matched, and file-offset-contiguous. `PT_DYNAMIC.p_filesz`
bounds the scan. The vDSO additionally binds to `AT_SYSINFO_EHDR` and the exact
`l_ld` VMA. Environment `GLOB_DAT` slots retain bias-relative `r_offset`
semantics but must lie in that node's own readable+writable file-backed load.

Fresh results on the bound source snapshot:

- environment module: 14 passed, 0 failed;
- complete target-loader filter: 41 passed, 0 failed;
- after-loader filter: 25 passed, 0 failed;
- formatting: passed;
- reverie-ptrace all-target check: passed;
- reverie-liteinst after-loader feature check: passed.

New controls cover harmless and legitimate same-inode duplicate instances,
Direct-only and Rebased-only foreign-instance pointers, genuine dual-valid
ambiguity, exact map offsets, split RELRO VMAs, PF_R, bounded `DT_NULL`,
`DT_REL`, `DT_JMPREL`, mixed representation modes, zero/range overflow,
ET_EXEC, nonzero-bias PIE, ordinary DSOs, auxv-bound vDSO geometry, and a
foreign-instance relocation slot. Assertions, comparators, tolerances, and
failure classifications were not relaxed.

`product-source-sha256.txt` binds all 36 product files. Its exact byte-stream
SHA-256 is `49f19d8e793bba7d86f837999999779bd0789095ea9b23627310f93920764ae6`.
The tracked diff remains byte-identical at
`283057ae74d583399e99cc5352ae56c289f28824d758529884f87a98e5b8362f`.

Two fresh read-only adversarial reviews independently verified the exact hashes
and all 36 manifest entries. Both found no blocking acceptance flaw and
approved one fresh isolated candidate staging only, not integration or landing.
Neither review found goalpost movement. Their shared nonblocking residual is a
conservative false rejection for unusual valid ELFs whose header/program-header
bytes are reachable only through page-rounded prefixes or nonstandard mappings;
the fixed candidate's inspected offset-zero header loads do not use that layout.
