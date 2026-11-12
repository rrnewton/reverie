Review exact Reverie commit 12d4ce8c0bc426f1ae41416f5b4a699e2c300879 on base c8f4ca9d2e95460e027678ff23f6dec2529d255d. Read complete candidate.patch and its actual committed base/head vm.rs copies plus surrounding runtime/error/executor/failure/Tool code. The final source is identical to accepted v24; review the real production cancellation and join path, not only the new test. Review instructions apply read-only: no source writes, builds, tests, guest runs, package installation or network publication.

The measured original full library failed three unchanged methods: two direct InvalidGuestAddress matches received an added derived cleanup marker, and one expected the old ordinary command after a fatal start-gate failure. Verify only the owned newly-cancelled fatal child's bare RunAborted is omitted; normal cancellation, all aggregates, failed sends/joins/registrations and real causes must remain errors. Parent failure/status/publication and actual consuming hooks must not become success. The direct variant/address/length and all original drain/registry assertions remain. The new eight-case control uses actual owned OS joins and artificial child return errors; do not mistake it for a real Tool-hook failure injection. Check that it distinguishes ordinary/fatal, bare/aggregate, fork/thread ownership and exact non-Clone cause identity.

Read REPORT.md and prior-evidence.json; distinguish the historical 426/1, focused four passes, current pre-correction 450/3, pending final source validation, and separate Hermit work. Do not claim final runtime success from this source packet. Root will bind actual final outcomes separately. Prior arbitrary panic/exec RPC/futex timing and Hermit parity limits remain; do not expand this correction into those mechanisms.

> GOALPOST-MOVING REVIEW RULE
>
> Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.
>
> Treat each of these as an explicit review target:
> - weakening an assertion so a test passes
> - widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
> - renaming or relabelling so a failure reads as a pass
> - deleting a check rather than satisfying it

State findings first, exact source locations and required corrections. If no findings, explicitly state no assertion/tolerance/skip/comparator was weakened, explain the exact fatal command expectation change, give scope limits and verdict for this exact commit. Final publication is root-owned.
