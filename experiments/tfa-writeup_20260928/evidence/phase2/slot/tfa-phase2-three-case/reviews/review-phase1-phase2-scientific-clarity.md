# Scientific-clarity review: phase-one RESULTS + phase-two evidence (read-only)

Scope: `experiments/tfa-kernel71-campaign_20260921/RESULTS.md`; phase-two code
`fbcode/testinfra/tfa/` (README, followup.py, cases.v1.json) and evidence tree
`tfa-phase2-three-case/` (stages/, aggregation/, investigation/, followup/,
stress/, gates/followup-20260925-r7-c1f8817a5ebb/). Path note: the brief's
`/home/newton/work/tfa/fbsource/ignored/tfa-phase2-three-case/` does not exist;
the live tree is the slot path above (prior lane: reviews/review-c1f8817a.md,
approve, bindings/verdict re-judged). No gate weakened; nothing edited/posted
except this artifact.

## Phase one — clear, minor ambiguities only
Understands cold: hypothesis + two-arm bracket (Abstr., §1-2), fixed rule
W>=3, denominators A/W/B/U vs S=201/C=93/T=294/R=512 never conflated, setup
(host/kernel/Hermit+ELF sha256, Buck-outside-Hermit, seeds/replicas/stop,
§3), per-target oracles (§3.4), outcomes (§4), 8 limitations, validators (§7).
Selection/adaptivity (10/30, diagnostics, informed continuations), censoring,
NOT_CHECKED authentication, existence-not-rates all stated — no overclaim.
Ambiguities: (a) §7 "Reproduction" reproduces accounting, not executions —
witness rerun needs `events[].argv` in records; §3.1 argv is templated
(HERMIT_BINARY/SWEEP/EVIDENCE). (b) SUCCESS is a manual deduplicated union
over 25 records/13 roots (each root alone: neither); no aggregate validator
(§8 admits) — table-supported, not machine-produced. (c) HTTPCoro "confirmed"
is a virtual-time threshold, explicitly not a thread race — do not cite it
as race evidence.

## Phase two — evidence sound, not context-free (main finding)
No phase-two RESULTS.md equivalent. A cold reader must assemble README
(containment internals, no science summary), 3 followup-comments,
candidate-reports, and gate SUMMARYs; roles (fail/pass-witness,
discriminator, fresh), SUPPORTED rule, and stress setup are undefined in
the comments themselves. Required: one summary stating hypothesis, cell,
controls (unfixed/patched ELF + revert-controls), n per seed, verdict rule,
limitations — before any external citation of SUPPORTED.

Candidate fixes (all test-only, unposted, candidate-only — not production
fixes): thrift InstrumentationTest (wait active==0), asyncssl
AsyncSSLSocketTest (drain write), httpcoro HTTPDownstreamCoroSessionTests
(NONBLOCK loop). Followup root-cause lines state mechanisms assertively;
underlying reports qualify them (thrift: not instrumented; asyncssl: ACK-
queued inferred, production-drain alternative rejected on sibling precedent
not disproof; httpcoro: no DBG drain log). Cite as investigator hypotheses
+ replay SUPPORTED, not proven causes. Candidate-report validations encode
NOT RUN as status FAIL 0/0, original_*=false — easily misread as failures.

SUPPORTED sample limits (verdict rule itself verified at c1f8817a):
thrift — manifest FAIL seed 1 PASSes unfixed at this path (caveated);
discrimination rests on seeds 3 (FAIL, multi-role) + 8 (FAILx1->PASSx2);
patched never run for seeds 6,7,9,10,11. asyncssl — seed 11 FAILx5+
NONDETERMINISTICx2 -> PASSx3 (seed outcome not fully deterministic);
fresh 15 OTHER in both phases (unrelated :3592) — patched test still
fails another class. httpcoro — cleanest (0: FAILx3->PASSx2) but fresh
5,6 CENSOREDx1 unfixed, 1 interrupted. Everywhere: 1-3 patched replays/
seed, Hermit-only, no native/CI/landed evidence, patched ELF bound only
as different-sha (builder build file), not to a build log in the comment.

Reproducers: stage1-2-relocated initial-comments are exemplary (exact
measured non-/tmp PASS/FAIL commands, "Reproducibility is proven, but
root cause is not."); followup-comments give no rerun command (ledger
sha + host-absolute followup-runs.json paths only).

## 36/40 containment — BLOCKED, do not shorten
36/40 = tfa_production_containment_test suite at c1f8817a run 2 (run 1:
34/40 + 2 host flakes, rerun 2/2, both preserved; gates SUMMARY). The 4
failures are signed Codex 0.156.0 preflight (missing daemon-socket fixes,
host mounts) + secondary chataccd cleanup — environment BLOCKED, rc 32.
It is not 36/40 cases/seeds/fixes, not a pass rate, and validates no
candidate fix (unit suite is separate: tfa_phase2_test 187/187). Any
text citing "36/40" without BLOCKED+preflight is unsupported.

## Verdict
Phase one: approve (minor ambiguities above). Phase two: changes
requested (reporting only) — add the context-free summary and qualify
root-cause/SUPPORTED/36-40 as above; evidence/gates unchanged.
Lane remains open for exact-head read-only review of network-only
replay evidence (none present in tree at this audit).
