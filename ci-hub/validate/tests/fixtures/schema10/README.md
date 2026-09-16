These are synthetic parser controls, not guest measurements or backend coverage.

The twelve JSON/JSONL files are exact producer exports from Hermit commit
edd48bf2040e7faa9330a8262e4af03555f30ab7, from
ci/manifest-plan/src/ledger/schema10/tests.rs. The producer ran its real shared
artifact verifier for each case before exporting them. SHA256SUMS.json binds
all twelve files; its own SHA256 is
87199552c47d880656625e19b8bee54fc3f5b6e89c88462365955c74ced07b5a.

matched has one completed parity match. reference-diverged has a candidate
ordinary match, a reference ordinary divergence, and no cross comparison.
cross-diverged-then-matched retains both outer attempts, with an initial cross
comparison divergence followed by a match. The later match must not erase the
first failure. All cases select one KVM candidate; the ptrace reference is not
an additional selected cell.

The full artifact contains fabricated /home/fixture paths to check that raw
report and invocation bytes stay in the artifact while the ledger summary
contains no owner paths. Tests may change run identities and recalculate the
corresponding artifact hashes in an isolated temporary store. They must not
change the preserved source fixture bytes or weaken the comparison contract.
