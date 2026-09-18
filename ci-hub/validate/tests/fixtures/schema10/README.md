These are synthetic parser controls, not guest measurements or backend coverage.

The sixteen JSON/JSONL files are exact producer exports from Hermit commit
73146a90f8f1dd468471ffab448b6b7d71f33bf6, tree
0dff6f5354eabd11f8b539161f0d5b40c9ce0467, from
ci/manifest-plan/src/ledger/schema10/tests.rs. The producer ran its real shared
artifact verifier for each case before exporting them. SHA256SUMS.json binds
all sixteen files and the producing native result record; its own SHA256 is
4821718ba4d479b09ea99cee7e5a20ec8750f289b2b047dcc11a43000118a7f7.
The producing run passed 238 library tests and 34 harness tests, with no failures,
ignored cases or filters, in 89.463 seconds wall time. These results establish
fixture provenance; they do not substitute for the parent reader's own checks.

matched has one completed parity match. reference-diverged has a candidate
ordinary match, a reference ordinary divergence, and no cross comparison.
cross-diverged-then-matched retains both outer attempts, with an initial cross
comparison divergence followed by a match. The later match must not erase the
first failure. These cases select one KVM candidate; the ptrace reference is not
an additional selected cell. Both operand invocation vectors include --strict.

ordinary-only selects and records one ordinary KVM comparison with no selected
backend-parity relation. The shared verifier reports full_backend_parity false.
Its ordinary observations remain readable while parity selected, recorded and
green counts remain zero. All sixteen original exports predate the explicit
binding contract: they are authenticated `legacy-unbound` controls and cannot
qualify as selected-attempt-bound receipts. Their measured matches, divergences,
and unavailable comparisons remain historical evidence; absence of the new
capability is not artifact corruption.

New selected-attempt-v1 positive controls live separately under
`selected-attempt-v1/`, with their own producer provenance. No field has been
inserted into these original exports to manufacture bound acceptance.

The full artifact contains fabricated /home/fixture paths to check that raw
report and invocation bytes stay in the artifact while the ledger summary
contains no owner paths. Every retained attempt has an empty environment map.
Tests may change run identities and recalculate the corresponding artifact
hashes in an isolated temporary store. They must not change the preserved source
fixture bytes or weaken the comparison contract.

The earlier twelve-file exports from edd48bf2040e7faa9330a8262e4af03555f30ab7
remain preserved in the original parent checkpoint
2b00b4ac0b6851fcdfda90805d30948023f940a5 and its verified local bundle. They omitted
--strict from synthetic invocation vectors and are historical inputs, not
current qualifying controls. The updated positive cases use fresh producer
exports rather than editing the old raw reports to manufacture acceptance.
