# Post-draft audit resolution map

Resolution target: `design.md` SHA-256
`2c443f583faf18e2baa54345536b101f5ec1b91ed393788c3a716576c2e89e72`.
Historical `CHANGES REQUIRED` reports remain immutable provenance and target
the superseded hashes recorded inside them.

## Audit C writing findings

| Finding | Resolution in current design |
|---|---|
| C-1 P0 lifecycle source | P0 component and event sequence now require authoritative root start, initial image, final disposition, output closure, task retirement, teardown, and no-leak evidence; comparator synthesis is forbidden. |
| C-2 P0-incompatible A1/A15 | P0 A1 uses invalid-syscall/fixed-buffer subcases; PID/RNG move to P3. A15 keeps six mutations in a phase-independent golden corpus and live mutations only for claimed families. |
| C-3 non-executable test intentions | `narf-hermit-acceptance/v1` defines commands, revisions, statuses, repetitions, timeouts, counts, comparator class, controls, exit disposition, and content-addressed artifacts for A0–A18. |
| C-4 incomplete determinism | The design now has a normative producer-watermark/snapshot transition contract, state dispositions, external-I/O liveness, full signal matrix, exact cooperative time profile, repetitions, and barrier-removal controls. |
| C-5 diagram promoted proposed artifacts | All new artifacts carry `[P]`; user/kernel/controller/harness regions are distinct. |
| C-6 crash ambiguity | Guest signal/core is a semantic disposition; provider/kernel/controller/harness crashes are `infrastructure_error`. |
| C-7 labels | P4 is **Proposed / Deferred**, stock ptrace execution/source evidence is split, loader inference is **Proposed**, and the three-Backend count is **Verified current (source-only)**. |
| C-8 disclosures/platforms | Browser-test, Narf replay absence, SMP-stop uncertainty, and Linux/Narf x86_64/aarch64 cells are explicit. |

## Deterministic Audit D and final Audit F findings

| Finding | Resolution in current design |
|---|---|
| D-C1/F-C1 event-to-commit membership | The scheduler assigns epochs from guest-causal source operations. Every producer supplies a monotonic sequence and synchronous watermark for a scheduler-named producer/operation set; controller arrival and wall-clock cutoffs are forbidden inputs. Target states have explicit queue/pool/pending/error dispositions. |
| D-C2/F-C2 external I/O | Outstanding operations freeze at publication. Scheduler snapshots require watermarks/causal tokens; ordinary work, only-poller, multiple-completion, and completion-only progress cases are specified. Host watchdog expiry is infrastructure-only. |
| D-C3 signals/POSIX | Directedness, masks/dispositions, temporary masks, arrival edges, pending sets, multiplicity, partial transfer, and per-syscall restart are a required matrix. External uncaused signals are refused inputs. |
| D-C4/F-C4 preemption/time | P3 promotion now uses exact `poc-coop-v1`: no async preemption, one one-resource request per semantic turn, fixed epoch, exactly 1 ns per committed turn, zero charge for skipped/poll/transport/teardown, exact deadline fast-forward, exec preservation, and per-task charge. Precise `poc-rcb-v1` is explicitly **Deferred** pending a separate complete reviewed specification. |
| D-C5 replay | Narf record/replay is explicitly **Deferred** and excluded from P0–P3 promotion; future same-provider replay/refusal gates precede cross-kernel comparison. |
| D-C6 lifecycle/parallelism | Clone, vfork, exec, and logical/physical-exit barriers and callback permutations are specified. Independent-process parallelism is **Deferred** behind a separate conflict/commit/effects/replay design and tests. |
| D-C7 assurance | P2/P3 record exact heads, strictness, relaxations, counts, hashes, commands, and comparator; require exhaustive synthetic edge permutations and at least 100 strict repetitions. A15/A16 remain intact. |

## Audit E and Audit G final-review findings

| Finding | Resolution in current design/evidence |
|---|---|
| E-1/F-C1/C2 host-timed epoch | Replaced drain-arrival cutoff with causal producer sequences, synchronous scheduler snapshot, and all-producer watermark closure. Uncaused physical inputs are excluded/refused. |
| E-2 A16 generic refusal | A16a requires `no_result: coverage` and names the missing family/location; A16b requires `no_result: comparison_refused` and names the malformed record. Generic `no_result` is forbidden. |
| E-3/G-1 A2 unresolved exemption | Only pre-characterized subcases can promote and must be `matched`. Research subcases stay `specified`; unresolved/unsupported/nonmatch cannot satisfy a phase gate. |
| E-4 diagram boundary | Linux and Narf guest-user, kernel, external-controller, and harness regions are separately drawn. |
| E-5 current label | The public-Backend count and LiteInst/e9patch interpretations are explicitly **Verified current (source-only)**. |
| G-2 trust denominator | Canonical findings expanded to F-1–F-38; `actionable-claim-inventory.md` maps every material current claim family, including security, platform-runtime, and helper-provider claims. Independent verification and trust gate are rerun against the complete anchor set. |
| G-3 checklist | All evidenced research/design/audit items are checked; staging, commit, clean-state, and exact-final-diff review remain open until actually performed. |
| G-4 audit history | This resolution map and the design audit appendix retain every failed report, its superseded target, and the later exact-head verdicts. |

## Exact-head provenance closure

Audit M (`audit-m-final-code-review-ready.md`, SHA-256
`1452dc6ea164e8e38d22d9a7d38aeb033c5f8e94f69e03f80f62ff725e7fdc92`)
found two bounded defects in the superseded design SHA
`8f28be03df3c61507254d27607a04e3757dbe2c38e379aeb2280fc9c5d44c7e6`:
the risk table asserted mutable-VFS/asynchronous-network behavior outside the
verified denominator, and F-37's final literal extended one line beyond its
declared range. The current design removes the unsupported current-state claim,
and canonical findings SHA
`b9a00bca41937b9e0f9770fdf04042a5dae2d36495d59adae035da57e67bde38`
extends the F-37 range through line 826.

Independent reverification records 38/38 findings and all 67 evidence items as
checked; all 58 range-bearing file references are in-range, with no relocation,
stale reference, or unknown citation. Its report
(`trust-gate-verifier-report.md`, SHA-256
`d813296c8f3adce6666fef94804f9f73b20479ebc2cbabc28e5e88ebed6b14e8`)
targets the current design and findings; canonical verification SHA is
`899c545fad45b83dcb978a4926a80066da2b8e7bb697902697797179e750be49`.
Audit N (`audit-n-final-writing-ready.md`, SHA-256
`b27c5585982f63869537c721b448ff68a8c0ec18f45c713f0d55901f82b8f9d0`)
passed the design prose and identified only this resolution-map retargeting.
Audit L's READY verdict is retained as historical because it targeted the
superseded SHA; the exact current design is re-reviewed separately.

No resolution deletes a test, widens a tolerance, filters a semantic field,
converts a failure to `matched`, or promotes source/test existence to executed
support. Final exact-head writing, deterministic, and adversarial verdicts are
recorded separately after this target is re-reviewed.
