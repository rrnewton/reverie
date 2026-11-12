Review target: uncommitted matrix preview based on Hermit 14d63ed54b7284b7f8bc29d44c7610a809815621; complete two-file patch d41233ccd085987ff5545a4b1f41860c62610c8cfc20a8eb5aaa45ef43da1a30. This is source review, not final committed-head or execution approval.

Findings:

- P2: changed/tests/backend-parity/run_matrix.py:1736–1740 unconditionally prefixes every passed KVM verification row with “Guest-visible verification only (stdout+exit compared; internal trace not compared)”. The new active canonical requirement can only supply a positive canonical/bitwise result, so the durable reason contradicts its earned tier and count columns. Keep historical weaker/no-evidence wording, but do not add that prefix to a bitwise row. Add a real-writer control covering canonical and historical rows under current and legacy headers.

The active one-reader requirement, historical default decoder, typed infrastructure ERROR handling, expected guest status and unchanged minima did not expose another blocker in this read. The contradictory scorecard wording was accepted by root as a concrete correction and is addressed in the separately preserved v2 proposal. It remains a finding on v1; this report does not retroactively approve those bytes.

Goalpost-moving assessment: no assertion, tolerance, expected tier, selection, bound or comparator was weakened. The active canonical requirement is stronger. Old flag/tier-coupling and requested-policy assertions change because the composed M2 producer makes canonical comparison the default; lower historical-tier assertions remain. No failure is promoted to a pass. The finding is an inaccurate current reason string, not evidence of a passing runtime result.

Verification: complete candidate diff, relevant complete execution/reader/writer paths, full changed tier tests, baseline scorecard helpers and caller searches were read. READBACK-v1.json authenticates immutable base copies, changed hashes, static Python parsing, and exact in-memory patch application. No product, test, guest, formatter or network operation ran.

Verdict: changes requested on v1. See REPORT-v2.md for the separately bound correction.
