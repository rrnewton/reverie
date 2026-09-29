# TFA phase-two controller follow-up audit

Scope: read-only audit of `/home/newton/work/tfa/fbsource` at
`50a9ce52c1be04a99c606c5087a7fffb62c634f5` and preserved artifacts under this
goal directory. The source checkout was clean (`sl status` produced no output).

## 1. HTTPCoro: one fatal event, three source-location renderings

Root cause: the controller counted the bare substring
`HTTPDownstreamCoroSessionTests.cpp:1685` and required a count of one. The
sealed stderr contains one crash, but glog/symbolizer renders the originating
source location in three distinct roles:

1. line 20 is the sole fatal header:
   `F... HTTPDownstreamCoroSessionTests.cpp:1685] Check failed: bodyEvent.hasException()`;
2. lines 48-49 are the matching coroutine frame in the normal signal stack;
3. lines 171-172 are the same coroutine frame in the async stack.

There is exactly one `Check failed: bodyEvent.hasException()` and one
`Signal 6 (SIGABRT)`. Wrapper evidence says return code 134, not timed out, and
the SafeHermit report says `unit_result=core-dump`, `process_signal=6`,
`exit_code=134`, `truncated=false`. Stdout starts exactly one expected gtest:
`HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2`; a fatal
abort correctly leaves no normal gtest footer.

Recommended fail-closed rule: classify structured roles, not a raw substring.

- Require exactly one expected gtest `[ RUN ]`, the `Running 1 test` count, no
  second test start, and no normal terminal result for another test.
- Require exactly one anchored fatal header whose file, line, and expression
  are the expected tuple `(HTTPDownstreamCoroSessionTests.cpp, 1685,
  bodyEvent.hasException())`.
- Require exactly one SIGABRT banner after that header and exact exit evidence:
  wrapper 134/not-timed-out plus fresh bound SafeHermit signal 6/exit 134,
  non-truncated report, and successful strict deterministic-log comparison.
- Permit exactly two additional appearances of that source location only when
  one follows the matching `H2DownstreamSessionTest_ResetRateLimit_Test`
  coroutine frame inside the normal stack and one follows the same matching
  frame after the unique `Check failure async stack trace` delimiter. Thus the
  total location count is exactly three in this witness, with roles
  `{fatal-header, normal-frame, async-frame}`. Reject extra fatal headers,
  signals, stack sections, location occurrences, or a frame for a different
  test/function.

This admits the real footerless witness without accepting an unrelated abort
that merely mentions the source location in a stack.

Evidence:

- `repro/httpcoro/fail/wrapper/stderr.txt`:
  `13eae99ea3da0f0dd55dd622823728f4c98942351b7c009a49a1e06ec15892b8`
- `repro/httpcoro/fail/wrapper/stdout.txt`:
  `64701a7c4b67e570f3333c5c0da2fbdcd7a53921ed1c5c69e47750f1f5f78058`
- `repro/httpcoro/fail/wrapper/metadata.json`:
  `24b6c6f955e246f492b5ede7497769906f8e6ec511c492b464f4e608d85cbfdd`
- `repro/httpcoro/fail/safehermit.json`:
  `45cd2695383a863d1f5d3dbdd4fff3b807fc80f18ca5e6c0920f0db267f2ec2f`

## 2. Thrift: deterministic commit-hook mirror expansion

Root cause: qualification and exact-diff review covered only
`fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp`. The recorded commit argv
also passed only that path after `--`. During `sl commit`, repository commit
finalization ran the normal mirror hook and printed:

`mirrored changes in 'fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp' to 'xplat/thrift/lib/cpp2/test/InstrumentationTest.cpp'`

It then created local commit `2f755a3be88273878ecb6db192153f20e9fb918d`
with exactly those two paths. This was not agent path drift. At parent
`7f239d5b37dfbecf32fed144468da461f998add5`, both files hash to
`99a6c74f02f855dd55ff2e07cd1a20ca281c55d568423ff212262eaf5c2a1795`;
at the commit, both hash to the validation-sealed qualified file hash
`21981f82ac24bf9085d5a8bef17c403778d36a6d52f2d3bf3ce1d3a3bfbf3912`.

Recommended explicit contract, sealed before commit:

- Define the canonical path `P` and its repository-declared mirror `M`
  explicitly; for this case, `P=fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp`
  and `M=xplat/thrift/lib/cpp2/test/InstrumentationTest.cpp`. Do not infer an
  arbitrary second path merely because bytes happen to match.
- Qualification remains against `P`, but the allowed final commit path set is
  exactly `{P, M}`. Require the final parent to remain the qualified base and
  reject every other path.
- Before commit require regular-file type/mode equality and byte equality of
  `P` and `M`. After commit require `sha256(final P) == sha256(final M) ==`
  the qualified file hash, and require the normalized parent-to-final patch for
  `M` to equal the patch for `P` after substituting only the path name.
- Require commit output to identify exactly the sealed `P -> M` mirror pair,
  seal the two-path local diff, and make final submission/review evidence name
  both paths. No runtime requalification is needed for the duplicate only if
  all equivalence checks hold; otherwise reject and requalify/review.

This admits the known automatic mirror while rejecting unrelated identical
files, a stale/divergent mirror, extra hook edits, or a changed primary after
qualification.

Evidence:

- validation seal:
  `e29406a71c04eba1f0032b04c0d1c3d6c9108a6786fb899c7d152148a5dcc059`
- qualified report:
  `d4d52fbfc6b07ff9582c37a3f772304427c22e6c955cd2dfcfd8a0fd050a4be2`
- commit metadata:
  `8bb9a08515e954cc6206391356f927aca6ff3209ea453af5552c2994cdaf924b`
- commit stdout:
  `1850671340f7f8ba4ae3a910b6d0fda3ec25a7a442851a8b441b6573f3c487d3`
- final-path readback:
  `b412b01a032b12fdf0f7c7318d4d4e5105477f4d31e496211daa5bc8655380c2`
- archived two-path draft diff (controller artifact):
  `dc42ca6b271c35620e3b9be2c9b6eb92f5f2498b541871e3ed94e4b5fa1dfe77`

## 3. AsyncSSL: staged image omitted Buck resources

Root cause: staged seed 11 did not reproduce the original missing-ACK failure
because its runtime image was incomplete for this Buck test. The original test
artifact directory contains the executable plus the sibling
`async_ssl_socket_test.resources.json`. That manifest maps seven logical
certificate resources, including `tests-cert.pem` and `tests-key.pem`, to the
source tree. `TestSSLServer::SetUp()` constructs `TestSSLServer`, whose
`getDefaultSSLContext()` calls `find_resource(kTestCert)` and
`find_resource(kTestKey)` before the test body. The internal resource hook uses
the executable's Buck resource manifest. The staged runtime inventory contains
198 ELF/shared-library closure entries and 19 materializations, but zero paths
matching `.resources.json`, `certs/`, or `.pem`. Consequently resource-manifest
loading produced `json parse error on line 0: expected json value`; the SSL
server and client were never initialized, and the four subsequent
`STATE_SUCCEEDED` versus `STATE_INIT` failures are destructor fallout. The
candidate's ACK wait was never executed.

Exact argv comparison:

- Identical semantic controls: Hermit binary and hash, `ptrace`, strict/chaos
  options, epoch, seed/rng/fuzz values, scheduler seed 11, heuristic/timeslices,
  minimal environment with `LC_ALL=C` and `TZ=UTC`, guest workdir
  `/tmp/tfa-phase2/cwd/asyncssl`, timeout 110, exact gtest filter, and strict
  verification.
- Evidence-only path changes: copied safehermit path (same SHA-256), report
  path/name, verify-log directory, and fresh controller-cwd bind.
- Runtime-changing differences: original mounted
  `/home/newton/work/tfa/fbsource/buck-out` and executed the normal Buck artifact
  at `.../__async_ssl_socket_test__/async_ssl_socket_test`; staged validation
  mounted only the isolated worktree's `buck-out` and executed the copied
  `.tfa-runtime/8cc27e1c3dd3b4e23584f120/.../async_ssl_socket_test`. The copied
  runtime held the ELF and DT_NEEDED library tree, but not the sibling resource
  manifest or its referenced certificate files.

Recommended fix: extend runtime closure construction beyond ELF dependencies.
If `<ELF>.resources.json` exists, parse it strictly, reject malformed or unsafe
paths, materialize every referenced regular file into the private runtime image,
and write a sibling staged manifest whose targets resolve entirely inside that
image. Seal/hash the manifest and each resource, include them in pre/post-run
closure verification, and fail before Hermit if any resource is missing or
escapes the sealed root. Preserve the manifest's executable-relative lookup
semantics; copying the original manifest unchanged beside a shallower staged ELF
is insufficient because its relative paths were authored for the original Buck
directory depth. Then rerun original PASS seed 1, original FAIL seed 11, and the
fresh stress grid against the complete staged image.

Evidence:

- original wrapper metadata:
  `c935b2f6c508e68ec991c3c42ac5df44347cfe013a91e0d1443ba1ea946814b9`
- staged wrapper metadata:
  `744ba77ea4157181ccca813330884cf3a70434b7ece7087b394c4ffcc480b3cd`
- staged runtime inventory:
  `857b426441a60442a89ea15a9a83efcde88fb2564e23e61494a2557907790369`
- original sibling resource manifest:
  `1e94574631bf496ba3c7f94989482e272236367d995814c28f2ac14806c2ee9c`
- staged stdout (setup failure):
  `c054a20de40ef97347b5a8c6bce4dc0771cabd3d89c5e2b4877b32cbabfcc093`
- staged stderr:
  `a19511d19e402bb349207a5429431c526b298b350569928e1d585d4b1f3cdf46`
- original missing-ACK stdout:
  `efe9b304115181544cdfed322dbb33960f2a9ae9031ff8ed2430fe607bbbab85`

## Reproduction/audit commands

```sh
sl -R /home/newton/work/tfa/fbsource log -r . -T '{node}\n'
sl -R /home/newton/work/tfa/fbsource status
rg -n -F 'HTTPDownstreamCoroSessionTests.cpp:1685' repro/httpcoro/fail/wrapper/stderr.txt
rg -n -F -e 'bodyEvent.hasException' -e 'Signal 6 (SIGABRT)' repro/httpcoro/fail/wrapper/stderr.txt
sl -R /home/newton/work/tfa/fbsource show --stat -r 2f755a3be88273878ecb6db192153f20e9fb918d
sl cat -r 7f239d5b37dfbecf32fed144468da461f998add5 fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp | sha256sum
sl cat -r 7f239d5b37dfbecf32fed144468da461f998add5 xplat/thrift/lib/cpp2/test/InstrumentationTest.cpp | sha256sum
sl cat -r 2f755a3be88273878ecb6db192153f20e9fb918d fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp | sha256sum
sl cat -r 2f755a3be88273878ecb6db192153f20e9fb918d xplat/thrift/lib/cpp2/test/InstrumentationTest.cpp | sha256sum
diff -u <(jq -r '.argv[]' repro/asyncssl/fail/wrapper/metadata.json) <(jq -r '.argv[]' artifacts/asyncssl/control/validation-attempts/attempt-001/seed-11/wrapper/metadata.json)
jq '{entries_count:(.entries|length), closure_count:(.closure|length), materialized_count:(.materialized|length), resource_related_paths:([..|objects|.path? // empty|select(test("resources\\.json|certs/|\\.pem$"))])}' artifacts/asyncssl/control/validation-attempts/attempt-001/runtime-materialization.json
sha256sum <evidence-files-above>
```

No source, source-control state, comments, diffs, or reviewers were changed.
