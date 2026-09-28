Review target: rrnewton/agent-utils 0ac5c1834bdeb1fa85f092d81dff02cb348bea57..197187069010f8b3f78b990046d53a4b403c028e

Findings:

- High: `py/pr_landing_planner/landing_context.py:158-164,263-274` and `rs/pr-landing-planner/src/context.rs:247-253,307-320` replace the complete body of every parsed `RETIRES` comment with only its lane, head, and target id before hashing. That removes more than the transient disclosure and optional `BY` identity: it also removes unrelated prose that the review authority assessed. A focused mutation held the stable comment id, immutable author, permission, state, and all timestamps fixed, changed trailing text from `No remaining concern.` to `Do not land: the race remains.`, and produced the same digest (`a96c360f04009a66ae4333a6103a342e08a70809821aecf5b0e0e2dc25a42533`). Since `apply_landing_context` accepts an objection-resolution decision by exact head plus this digest, that edit does not invalidate the decision. This contradicts `rs/pr-landing-planner/src/model.rs:289-290`, which says body text is included because edits can create or remove an objection, and it misses the same-timestamp body-mutation case already tested for ordinary events. Preserve the complete non-identity body text in the digest while normalizing only the disclosure and optional `BY` identity; add Python, Rust, and differential controls showing identity-only changes keep the digest while any other body change changes it at the same timestamps.

Consumer enumeration:

- Production evidence producers: Python `githubhost.py` and `fakehost.py`; Rust `host.rs` and `fixture.rs`.
- Core consumers: Python `collect.py`, `landing_context.py`, `graph.py`, and `emit.py`; Rust `collect.rs`, `context.rs`, `graph.rs`, and `emit.rs`. The Python and Rust CLIs reach those paths through their host, collection, planning, and rendering entry points.
- Contract and validation consumers: `cross/differential.py`, the two changed Python test files, the inline Rust tests, `USER_GUIDE.template.md`, and both rendered guides. The Python package guide and Rust embedded guide are symlinks to those rendered files.
- Checked client consumer: dev-hermit `ci-hub/health/pr_status.py` invokes the planner's status command and reads its summary and PR JSON; it does not separately interpret the changed author or retirement-body internals.

The intended split is otherwise preserved. Exact PR and review heads, stable event ids, event state, creation/update chronology, GitHub event-author permission and permission threshold, stale or inactive retirement refusal, exact-head review receipts, and policy-label uniqueness remain checked. Missing authors and permission failures retain the snapshot without granting retirement authority. Multiple `agent:` labels now produce no assignment and no authority. OS process, cgroup, and path-safety checks are outside this planner slice and no such code is changed.

Goalpost-moving assessment:

- Assertions weakened: no surviving non-identity assertion was weakened; removed author/disclosure assertions match the requested identity removal, but there is no negative assertion for other text in a `RETIRES` comment.
- Tolerance widened, exemption added, case skipped, or comparator relaxed: yes — the review-evidence digest ignores the whole `RETIRES` comment body rather than only identity text. This is the blocking finding above.
- Failure renamed or relabelled as a pass: no.
- Check deleted instead of satisfied: no for the checks meant to survive; identity checks were removed or re-keyed as requested.

Verification: 34 focused Python tests passed; 48 Rust package tests passed; the Python/Rust differential passed 128 checks; the focused body mutation above independently exposed the missing case. I did not repeat the implementor's full validation, so the passing test commands share this tree and command class with that evidence; the new mutation is the independent evidence.

Verdict: changes requested.

CHANGES-REQUESTED-AT: codex 197187069010f8b3f78b990046d53a4b403c028e
