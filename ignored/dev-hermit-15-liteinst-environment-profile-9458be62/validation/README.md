# LiteInst counter-guarded getenv validation

This packet follows the single candidate-05 execution. Candidate-05 was not
rerun. A byte-identical copy of its preserved log is `candidate-05.log`; it has SHA-256
`0e4c38c191dc8048e6b621780808027178bb68a6adca064fca7b5666021d1096`
and failed before the first private function call because the old environment
validator accepted only an invented `rax -> r12` direct load. The authenticated
host libc instead uses a counter-guarded `r15 -> rbp` profile. Its physical
partition was valid: 2,376 records, 238 physical statuses, 236 successful
resumes, 2 explicit dispositions, zero ordinary/cleanup/after-close losses, and
no violations.

The production fix admits that profile only when all of the following match:

- full provider SHA-256 `d932cb6bc88da10cc709d5b7ecda57a1387b6571331689dba88023b4c0517776`;
- all 165 `getenv` bytes;
- `getenv`, `__environ`, GOT, counter, and allocation-list RVAs;
- the pre-existing exact symbol/version/relocation/load-geometry checks.

The legacy direct profile is unchanged. Every one-byte mutation of the 165-byte
profile, a different provider digest, and every metadata/bookkeeping mutation
is refused. The bookkeeping words must now also be non-overlapping.

Final passing evidence:

- `counter-guarded-profile-test.log`: exact-profile test passed.
- `environment-tests-fixed.log`: 9 passed, including COPY/interposition and all
  alias-import controls.
- `after-loader-tests.log`: 25 passed.
- `ptrace-all-target-check.log`: all-target check passed.
- `liteinst-after-loader-check.log`: LiteInst after-loader check passed.
- `real-provider-staged-partition-release-test.log`: the real pinned provider
  and candidate-05 staging artifacts passed the release-profile no-tracee phase
  partition test.

Retained diagnostic failures are not final validation claims:

- `environment-tests.log` and the three `graph-import-*-diagnostic.log` files
  exposed that the synthetic relocation named dynsym index 1 while its record
  was incorrectly written at index 0. Production correctly skipped the empty
  index-1 symbol. The fixture now uses one `SYMBOL_INDEX` for both the record
  address and `r_info`; the original COPY/import assertions were retained and
  pass.
- `copy-diagnostic.log` is the same pre-fix fixture diagnosis.
- `real-provider-staged-partition-test.log` was deliberately refused by the
  test's release-profile gate. The otherwise identical release-profile command
  is the passing final control above.

`product-source-sha256.txt` covers all 36 tracked-modified and untracked product
files. `tracked-diff.binary` remains the 19-file tracked snapshot; its SHA-256
is `283057ae74d583399e99cc5352ae56c289f28824d758529884f87a98e5b8362f`
with 7,597 insertions and 757 deletions. No Hermit invocation was made.

`final-commands.md` records the exact argv, flags, environment, working
directory, and log/status mapping. `final-artifact-binding-sha256.txt` and
`final-artifact-binding-stat.txt` bind the source snapshot, staged inputs,
provider files, candidate result, and final validation logs.
