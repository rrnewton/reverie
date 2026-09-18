# Runtime dynamic-pointer validation

Candidate-06 was executed exactly once and was not retried. Its log SHA-256 is
`0b246433403064d493adf0443a0a4ea0306b79a7f437e1d0c964bdb238da9066`.
It passed the exact pinned `getenv` profile, then the initial environment
observation failed before any private function call because runtime dynamic
pointer tags were always rebased. Ordinary loaded DSOs expose absolute `d_ptr`
values; vDSO retains relative values. The failed run retained a valid physical
partition: 2,376 records, 238 physical statuses, 236 successful resumes, 2
explicit dispositions, zero lost/after-close records, and no violations.

The fix resolves every pointer-valued dynamic tag from the deduplicated set
`{raw, bias + raw}`. The entire known range must be readable, private,
non-writable, inside the node's bounded image window, and have the same mapping
identity as `l_ld`. Inode-zero images must remain in the exact `l_ld` VMA.
Exactly one candidate must qualify, and every relevant tag in one node must use
one representation mode. Relocation-record `r_offset` rebasing remains
unchanged. Environment data retains its VM_READ requirement; ptrace access to
PROT_NONE is not an alternative.

Final passing evidence:

- `dynamic-pointer-tests-expanded.log`: 2 focused tests passed.
- `environment-tests.log`: 11 environment tests passed.
- `target-loader-tests.log`: 38 target-loader tests passed.
- `after-loader-tests.log`: 25 after-loader tests passed.
- `fmt-check.log`, `ptrace-all-target-check.log`, and
  `liteinst-after-loader-check.log`: passed.
- `real-provider-staged-partition-test.log`: release-profile real-provider
  configuration control passed without starting a tracee.

The focused controls cover relative and absolute tag representations,
bias-zero deduplication, mixed representations, two valid candidates, wrong
identity, writable/shared/non-readable mappings, ranges crossing into another
image, inode-zero vDSO VMA confinement, and checked-add overflow. The synthetic
main/libc dynamic mappings now model post-relocation RELRO permissions.

`dynamic-pointer-test.log` is a retained compile diagnostic from the first
draft (`DynamicImage` unnecessarily derived equality while `LinkNode` did not).
`dynamic-pointer-test-fixed.log` records the corrected initial positive test;
the later expanded and module-wide logs are the final claims.

Exact commands are in `final-commands.md`. `product-source-sha256.txt` covers
all 36 tracked-modified and untracked product files. The 19-file tracked diff is
unchanged at SHA-256
`283057ae74d583399e99cc5352ae56c289f28824d758529884f87a98e5b8362f`
(7,597 insertions, 757 deletions). No Hermit invocation was made.
