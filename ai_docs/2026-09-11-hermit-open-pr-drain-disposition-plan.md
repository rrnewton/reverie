# Hermit open pull-request drain disposition plan

## Scope and measurement

This plan consolidates the complete Hermit census, the 60-item triage, the 13-objection current-main assessment, the 21-item rebase assessment, and the subsequent attempts to prepare every wanted reconstruction. The census was measured from **2026-09-11 09:40:23 through 09:40:26 EDT**. GitHub returned all 60 open `rrnewton/hermit` pull requests with `hasNextPage=false`; 35 were non-draft and 25 were drafts. A complete refresh at **2026-09-11 11:34:50 through 11:34:51 EDT** returned the same counts. The content assessments and reconstruction attempts used Hermit main `c7520a6c38731128ec3e0bf802d80b0d1cca80c0`, which was unchanged at the refresh.

This is a timestamped disposition plan, not a live queue. Refresh open state, exact heads, TaskGraph ownership, fetched main, reviews, and validation before acting. The report does not authorize bypassing review, validation, or landing controls.

Source records:

- `fresh-open-pr-census-three-repos-20260911`
- `triage-60-open-hermit-prs-read-only-20260911`
- `assess-13-hermit-objections-against-current-main`
- `assess-21-hermit-rebase-group-worth-reconstructing`
- `prepare-three-cheapest-hermit-reconstructions`
- `prepare-next-hermit-reconstructions-including-one-deeply-behind`
- `complete-the-remaining-wanted-hermit-reconstructions`

## Reconciled counts

| Survey result | Required disposition | Owner decision now? | Count |
|---|---|---:|---:|
| Landing candidates | Exact-head review and validation, then land | No | 6 |
| Wanted reconstruction, prepared | Send the seven append-only heads through independent exact-head review and validation | No | 7 |
| Wanted reconstruction, changed semantics | Owner chooses the current behavior before seven replacements can be implemented | **Yes** | 7 |
| Wanted reconstruction, validation evidence required | Owner chooses whether to remeasure two promotions on exact current HEAD or close them | **Yes** | 2 |
| Wanted reconstruction, consolidated | Close the older one-DAG PR after its requirements are accepted in the prepared replacement | No | 1 |
| Rebase/reconstruction, obsolete | Close with the recorded current-main reason | No | 4 |
| Objection met on main | Close as superseded by the stricter current-main mechanism | No | 2 |
| Proved superseded | Close with the recorded content-on-main proof | No | 5 |
| Objection still live | Owner chooses focused repair/adoption or rejection and closure | **Yes** | 11 |
| Adoption or draft survey | Keep pending until hermit-001 reports; do not guess the split | Pending survey | 15 |
| **Total** |  |  | **60** |

Thus 25 items have an evidence-backed action without a new owner decision, 20 need an owner decision, and 15 remain pending the assigned survey. The 20 comprise 11 live review objections, seven reconstruction stops where current behavior must be chosen, and two proposed promotions that need a remeasurement decision. The two met objections are not landing candidates: main solved their concerns by a stricter route than the pull requests proposed, so their old branches are closer to superseded work.

## Ordering to zero open

1. Refresh the 60-PR cohort and claims immediately before mutation. Close the 11 items already proved obsolete, superseded, or replaced by stricter main content.
2. Obtain the 20 owner decisions. The first decision in the landing chain is the repaired form of https://github.com/rrnewton/hermit/pull/2971.
3. Preserve this mandatory chain: the approved parent `dev-hermit` compatibility migration (recorded at local commit `72f16fe` plus its follow-up) must land before https://github.com/rrnewton/hermit/pull/2971; that PR must land before https://github.com/rrnewton/hermit/pull/2976; that PR must land before https://github.com/rrnewton/hermit/pull/2931. Rebase, re-review, and revalidate a later item whenever fetched main differs from its reviewed base.
4. Land the other five landing candidates after normal exact-head review and validation, ordered further only by fresh path overlap or newly discovered dependencies.
5. Send the seven prepared append-only heads through independent exact-head review and validation. Do not infer approval from a successful replay or build. Land https://github.com/rrnewton/hermit/pull/2970 before the dormant parity work from https://github.com/rrnewton/hermit/pull/2945; correct the disclosed 32-versus-24 test-count defect before https://github.com/rrnewton/hermit/pull/2694 can land; and require a full scheduler verdict and SaBRe evidence for https://github.com/rrnewton/hermit/pull/2836.
6. Resolve the seven changed-semantics stops and the two validation-evidence stops below. Each needs either an owner-authorized reconstruction/remeasurement or a substantive closure; no replay branch exists.
7. Close https://github.com/rrnewton/hermit/pull/2927 after the useful one-DAG requirements are accepted in https://github.com/rrnewton/hermit/pull/2970. Do not replay its stale graph separately.
8. Incorporate hermit-001's disposition of the 15 pending adoption/draft items. Two known external constraints must remain visible: https://github.com/rrnewton/hermit/pull/2982 depends on https://github.com/rrnewton/reverie/pull/536, and https://github.com/rrnewton/hermit/pull/2985 needs a replacement or landed equivalent for closed-unmerged https://github.com/rrnewton/reverie/pull/542.
9. Recount GitHub from a fully paginated read. Zero is reached only when every listed URL is merged or closed with its recorded reason; prepared branches and local replacements do not reduce the open count.

## Per-pull-request dispositions

### Landing candidates: 6

| Pull request | Required disposition | Ordered behind |
|---|---|---|
| https://github.com/rrnewton/hermit/pull/2887 | Complete independent exact-head review and canonical validation, then land. | No dependency recorded by the four surveys. |
| https://github.com/rrnewton/hermit/pull/2931 | Rebuild on fetched main if necessary, preserve the reviewed documentation content, validate, and land. | https://github.com/rrnewton/hermit/pull/2976, which is behind https://github.com/rrnewton/hermit/pull/2971 and the parent compatibility migration. |
| https://github.com/rrnewton/hermit/pull/2963 | Complete exact-head review and validation, then land. | No dependency recorded by the four surveys. |
| https://github.com/rrnewton/hermit/pull/2966 | Preserve its focused measurement review, complete full landing validation, then land. | No dependency recorded by the four surveys. |
| https://github.com/rrnewton/hermit/pull/2968 | Complete exact-head review and validation, then land. | No dependency recorded by the four surveys. |
| https://github.com/rrnewton/hermit/pull/2973 | Reconfirm current ownership, complete exact-head review and validation, then land. | No dependency recorded by the four surveys. |

### Wanted reconstruction outcomes: 17

All 17 wanted reconstructions were attempted: seven have prepared branches, ten stopped, and none remain unattempted. The stops split into seven current-semantics decisions, two exact-head validation-evidence decisions, and one consolidation. “Prepared” means only that an append-only branch was read back and the recorded build/check completed; it is not independent review, approval, or landing clearance.

| Pull request | Outcome and required disposition | Prepared artifact or stop boundary | Source preservation | Build/check and elapsed time | Ordered behind |
|---|---|---|---|---|---|
| https://github.com/rrnewton/hermit/pull/2427 | **Stopped — current semantics.** Choose how typed sandbox/infrastructure classifications compose with current terminal/clean-pass, retry, product-result, and probe-disabled accounting before reconstruction. | No replay branch; the pressure-test paths conflict at the result-schema boundary. | Source `23b9610d63fa1b809c17ec1224f0b4071cb00dfc`; rescue `refs/rescue/20260911-hermit-pr-2427-source-23b9610d`. | Stopped before build; 104 seconds. | Current cell-result and pressure schemas. |
| https://github.com/rrnewton/hermit/pull/2694 | **Prepared, not approved.** Correct the disclosed gate from 24 to 32 executed `run_kvm_` tests, then perform independent determinism/Linux-semantics review and validation. | Branch `codex/reconstruct-pr-2694-20260911`; head `ab41620514c9d27630e8ecf1323693c8cc8caba2`; current `open_file_id` retained during a conflict-free replay. | Source `446667cf0765629e231093852790a913c26f9c9c`; rescue `refs/rescue/20260911-hermit-pr-2694-source-446667cf`. | Build passed in 41 seconds; 114 seconds total. | Current Detcore file-description model. |
| https://github.com/rrnewton/hermit/pull/2738 | **Stopped — current semantics.** Choose the schema-7 representation and legacy-reader policy before porting `Selected`/`NotSelected` and regenerating artifacts. | No replay branch; all four paths conflict, including stale schema-6 population against current 707/188/4849 counts and new categories. | Source `76b4293c7137917e4d58371aac1ce6fd5abdfcb2`; rescue `refs/rescue/20260911-hermit-pr-2738-source-76b4293c`. | Stopped before build; 124 seconds. | Current scorecard vocabulary, schema, and population. |
| https://github.com/rrnewton/hermit/pull/2745 | **Prepared, not approved.** Independently review the historical SaBRe `ppoll` comparison before landing or closing the source in favor of it. | Branch `codex/reconstruct-pr-2745-20260911`; head `58fc78f0320083823d4bff24e68cdb3e242f582f`; conflict-free, byte-identical 175-line historical document. | Source `22a0058bbe05ddc5b472df1f98527470af69b296`; rescue `refs/rescue/20260911-hermit-pr-2745-source-22a0058b`. | Initial build failed with status 101 because submodules were uninitialized; after exact-pin initialization, the build passed in 42 seconds; 221 seconds total. | Current documentation layout. |
| https://github.com/rrnewton/hermit/pull/2747 | **Stopped — current semantics.** Repair async child-TID wake admission and cover fatal-signal and other teardown paths before a scheduler reconstruction. | No pushed branch. A conflict-free local forensic replay at `d7f01b95a472f590ea021e191472f72cb24a60a5` was neither built nor preserved as a review head because the semantic gaps remained. | Source `76b078dd5e64e5a3bdfec8d6d187db0f1094dca5`; rescue `refs/rescue/20260911-hermit-pr-2747-source-76b078dd`. | Semantic review stopped preparation; 497 seconds. | Full deterministic-scheduling and Linux/POSIX review. |
| https://github.com/rrnewton/hermit/pull/2765 | **Stopped — exact-head validation evidence.** Decide whether to rerun all 32 seeds on current main and regenerate the promotion or close it; do not import the old generated evidence. | No replay branch. The current cell is disabled after 32/32 passes because it observed 6 distinct outcomes against `min_distinct=16`. | Source `b17210d070a07dad378108da20834210c3c7c09c`; rescue `refs/rescue/20260911-hermit-pr-2765-source-b17210d0`. | Remeasurement required; 137 seconds of assessment. | Current exact-head plan, cell, scorecard, and population. |
| https://github.com/rrnewton/hermit/pull/2797 | **Stopped — current semantics.** Choose how the old residual categories map to current `FailureClass`, terminal attempts, retry authority, and denominators before reconstruction. | No replay branch; the old `NodeClassification` model conflicts with current result authority. | Source `e4c3af56cfe2a942fe5512ac3f3986f8a8551e12`; rescue `refs/rescue/20260911-hermit-pr-2797-source-e4c3af56`. | Stopped before build; 204 seconds. | Current result schema. |
| https://github.com/rrnewton/hermit/pull/2810 | **Prepared, not approved.** Independently review and validate the explicit mode policy: replay false, verify/chaos true, and unknown modes refused. | Branch `codex/reconstruct-pr-2810-20260911`; head `ee74a88b4d2783903343ae005ba0008c45f72b36`; no conflicts. | Source `755f7e19589d516a08eff108d2e75407874b20d6`; rescue `refs/rescue/20260911-hermit-pr-2810-source-755f7e19`. | Build passed in 41 seconds; 166 seconds total. | Current ledger and scorecard mode rules. |
| https://github.com/rrnewton/hermit/pull/2822 | **Stopped — current semantics.** Choose the typed termination wire mapping and schema-1 compatibility against current failure, retry, completeness, test-result, and tool authority. | No replay branch; the old `StepOutcome` representation conflicts with current `FailureClass` and `RetryClass`. | Source `cec5d53e1dcaa94003eec4dda192f313bba6879b`; rescue `refs/rescue/20260911-hermit-pr-2822-source-cec5d53e`. | Stopped before build; 135 seconds. | Current termination-result API. |
| https://github.com/rrnewton/hermit/pull/2836 | **Prepared, not approved.** Require independent scheduler review and SaBRe guest stress evidence; the BTreeSet drain converges, but the first loop that observes a host report remains host-timed. | Branch `codex/reconstruct-pr-2836-20260911`; head `10ae08f829ca97145c1a0641d7b3bd6176f79d6f`; exact conflict-free two-commit shutdown delta. | Source `1ccb9b41c4a48bc378b06aab04fc3bfb1cf6078e`; rescue `refs/rescue/20260911-hermit-pr-2836-source-1ccb9b41`. | Initial build failed with status 101 because submodules were uninitialized; after exact-pin initialization, a 39.71-second cold compile did not capture shell status, then the authoritative warm build passed in 0.190 seconds; 6 focused and 655 full Detcore library tests, format, and diff checks passed; 354 seconds total. | Full deterministic-scheduling and Linux/POSIX review. |
| https://github.com/rrnewton/hermit/pull/2925 | **Prepared, not approved.** Independently review the merged retention semantics and confirm the 64-pair implicit-cache policy with ownership. | Branch `codex/reconstruct-pr-2925-20260911`; head `86e020741d8bb0ae64c29febf53e111a93a72e27`; one `backends.rs` conflict preserved typed retention and the DBT envelope while adding failed-log retention. | Source `d63465ef6ec4f027f2088ddd41248c8c0beb01b4`; source PR remains open at this exact head; no separate rescue was created in the earlier batch. | Build passed in 42 seconds; 153 seconds total. | Current backend evidence ownership. |
| https://github.com/rrnewton/hermit/pull/2927 | **Stopped — consolidated.** Close only after the useful immutable-DAG requirements are independently accepted in the prepared replacement for https://github.com/rrnewton/hermit/pull/2970. | No independent replay branch; its stale 405-step, 7-label graph overlaps 38 paths, while the prepared replacement has the canonical 1,382-step, 17-label graph. | Source `12b7951ef5fd57765065383f15f8a7b3390de0d7`; rescue `refs/rescue/20260911-hermit-pr-2927-source-12b7951e`. | Consolidation assessment; 202 seconds. | https://github.com/rrnewton/hermit/pull/2970. |
| https://github.com/rrnewton/hermit/pull/2945 | **Prepared, not approved.** Independently review strict ptrace-reference comparison after the one-DAG replacement lands; the payload is otherwise dormant. | Branch `codex/reconstruct-pr-2945-20260911`; head `68e138fc437027fdbfe846302d1cc932409b9e29`; `record_envelope` conflict preserved the current shared type and new variant, with mechanical `JsonVerdict` to `LogDiffVerdict` adaptation. | Source `e5ff17ecd0c5d758cd50a45ac0b52a6d0f3df71b`; source PR remains open at this exact head; no separate rescue was created in the earlier batch. | First build failed with status 101 after 37 seconds; corrected build passed in 7 seconds; 180 seconds total. | https://github.com/rrnewton/hermit/pull/2970. |
| https://github.com/rrnewton/hermit/pull/2948 | **Stopped — exact-head validation evidence.** Decide whether to requalify all LiteInst candidates on current main and regenerate or close; do not replay the old three-run promotion data. | No replay branch. The old evidence promotes 234 cells while current main has 28 enabled-green cells. | Source `1c2cc4d14faacbb108fd625a4d1060705bb7be71`; source PR remains open at this exact head; no separate rescue was created in the earlier batch. | Remeasurement required; 50 seconds of assessment. | Current comparator, retry, selection rules, and population. |
| https://github.com/rrnewton/hermit/pull/2958 | **Stopped — current semantics.** Decide how its evidence, retention, restart, no-clobber, and backend-refusal requirements fold into the current harness cutover for https://github.com/rrnewton/hermit/pull/2969. | No replay branch; comparator, canonical-verdict, runner-schema, and DBT-transport ownership conflict with current main. | Source `f18b76cc464a3f8f7704f2d7abdbf53baff157e0`; source PR remains open at this exact head; no separate rescue was created in the earlier batch. | Stopped before build; 151 seconds. | https://github.com/rrnewton/hermit/pull/2969 and current Reverie API review. |
| https://github.com/rrnewton/hermit/pull/2969 | **Stopped — current semantics.** Supply and choose the missing current semantic prerequisite, including the retained requirements from https://github.com/rrnewton/hermit/pull/2958, before rebuilding the cutover. | No prepared branch. Its unique commit replayed mechanically, but the build failed on 16 missing `run_evidence` references inherited from a non-main base stack. | Source `e7af5cd8e5a80a0b74be5f44f26b33d4798c2697`; source PR remains open at this exact head; no separate rescue was created in the earlier batch. | Build failed with status 101 after 40 seconds; 264 seconds total. | https://github.com/rrnewton/hermit/pull/2958 and current Reverie API review. |
| https://github.com/rrnewton/hermit/pull/2970 | **Prepared, not approved.** Independently review and validate the canonical one-DAG generator and the folded requirements from https://github.com/rrnewton/hermit/pull/2927. | Branch `codex/reconstruct-pr-2970-20260911`; head `d45a1f49511ba208bc8157da5eb7dcc41b176a4e`; one modify/delete conflict kept the current deletion of `portable.json`. | Source `b0ea4580a11d381f3b1fd176a80974019697fd39`; rescue `refs/rescue/20260911-hermit-pr-2970-source-b0ea4580`. | Build passed in 40 seconds; canonical DAG generator check passed in 26 seconds; 234 seconds total. | Precedes https://github.com/rrnewton/hermit/pull/2945. |

The preparation work took 3,290 seconds in aggregate: 1,422 seconds for the seven prepared heads and 1,868 seconds for the ten stops. Five source heads—https://github.com/rrnewton/hermit/pull/2925, https://github.com/rrnewton/hermit/pull/2945, https://github.com/rrnewton/hermit/pull/2948, https://github.com/rrnewton/hermit/pull/2958, and https://github.com/rrnewton/hermit/pull/2969—remain preserved by their open pull requests at the exact heads recorded above but did not receive separate rescue refs in the earlier preparation batch.

There is no generic promotion-time age threshold. The stops for https://github.com/rrnewton/hermit/pull/2765 and https://github.com/rrnewton/hermit/pull/2948 are judgments about evidence applicability: a full validation requires a clean canonical pass at the exact HEAD, and changes in revision, schema, configuration, population, or rules require remeasurement. Age alone neither invalidates applicable evidence nor makes inapplicable evidence valid.

### Close as obsolete after the rebase assessment: 4

| Pull request | Required disposition | Ordered behind |
|---|---|---|
| https://github.com/rrnewton/hermit/pull/2783 | Close. Main has its useful preparation/runtime checks; the residual scheduler change would hide a load-bearing INFO diagnostic, and the binary archive/absolute-path manifest should not be replayed. | None. |
| https://github.com/rrnewton/hermit/pull/2825 | Close as superseded by the explicit measured width-8 concurrency policy. Do not claim the original width-20 pressure bar passed unchanged. | None. |
| https://github.com/rrnewton/hermit/pull/2833 | Close. Main's all-target build plus content-addressed artifact barrier is the stronger replacement. | None. |
| https://github.com/rrnewton/hermit/pull/2909 | Close. Main records all five KVM cells enabled, green, measured-and-passed with six observations. | None. |

### Close because stricter main content meets the objection: 2

| Pull request | Required disposition | Ordered behind |
|---|---|---|
| https://github.com/rrnewton/hermit/pull/1689 | Close as superseded. Main rejects ordinary DBT log paths, strips inherited log configuration, creates protected per-run evidence, fails closed on incomplete evidence, and retains the broad all-INFO comparator. | None. |
| https://github.com/rrnewton/hermit/pull/2400 | Close as superseded. The replacement scorecard includes never-measured and measured-no-verdict rows in a reconciled cross-tab and makes absent/one-sample evidence uncheckable rather than passing. | None. |

### Close because content is already superseded: 5

| Pull request | Required disposition | Ordered behind |
|---|---|---|
| https://github.com/rrnewton/hermit/pull/2800 | Close. Merged replacement https://github.com/rrnewton/hermit/pull/2888 supplies the outer-DAG plan/runtime work without the stale stack. | None. |
| https://github.com/rrnewton/hermit/pull/2936 | Close. Main pins a Reverie descendant seven commits beyond this pin across manifests and budget guards. | None. |
| https://github.com/rrnewton/hermit/pull/2949 | Close. Main's agent-utils pin is an 11-commit descendant of this pin. | None. |
| https://github.com/rrnewton/hermit/pull/2952 | Close. Main's agent-utils pin is an eight-commit descendant of this pin. | None. |
| https://github.com/rrnewton/hermit/pull/2955 | Close. Main's agent-utils pin is a five-commit descendant of this pin. | None. |

## Owner decision required: 20

Nine wanted reconstruction stops in the table above require decisions: seven need a choice of current semantics and two need a choice to remeasure or close. The following 11 live objections also require the owner to choose either the named focused repair/adoption followed by fresh review and validation, or rejection and closure. A mergeable GitHub state is not clearance.

| Pull request | Decision required | Ordered behind |
|---|---|---|
| https://github.com/rrnewton/hermit/pull/2216 | Authorize a mutation guard that fails when production review bindings are deleted or survive only as decoy strings, or reject/close. | Current merge-gate workflow and checker. |
| https://github.com/rrnewton/hermit/pull/2302 | Authorize a deterministic physical-exit barrier covering KVM as well as SaBRe, with full scheduler/Linux review, or reject/close. | Current pending-physical-exit design. |
| https://github.com/rrnewton/hermit/pull/2368 | Authorize completion of one coherent, linking Buck2/Reverie build without split dependency sources, or reject/close. | A publishable/current Reverie source and current Buck2 tree. |
| https://github.com/rrnewton/hermit/pull/2408 | Authorize re-deriving and preserving the four measured cases against the current 710-cell plan, or reject/close. | Current plan and removal of obsolete KVM-special-case hunks. |
| https://github.com/rrnewton/hermit/pull/2425 | Authorize a correct SaBRe `EINTR`/`SA_RESTART` implementation with deterministic scheduling and Linux/POSIX review, or reject/close. | Current signal and scheduler semantics. |
| https://github.com/rrnewton/hermit/pull/2771 | Authorize consolidating the still-needed inherited-stdio containment requirement into the safe reconstruction of https://github.com/rrnewton/hermit/pull/2694, then close this unsafe partial implementation; otherwise reject/close directly. | https://github.com/rrnewton/hermit/pull/2694 if repaired. |
| https://github.com/rrnewton/hermit/pull/2904 | Authorize a current exact-head Demo 08 gate with fixed criteria and stable checkout inputs, or reject/close. | Restore or replace the absent Demo 08 workload first. |
| https://github.com/rrnewton/hermit/pull/2907 | Authorize repair of the host-timing-dependent virtual-clock behavior plus a real negative control for the causal claim, or reject/close. | Current scheduler and comparator semantics. |
| https://github.com/rrnewton/hermit/pull/2922 | Authorize the missing durable typed producer, immutable receipt, fresh-clone recomputation, and fail-closed authority path, or reject/close. | Current schema-9 reader and current validation publication path. |
| https://github.com/rrnewton/hermit/pull/2934 | Authorize a replacement Demo review gate that rejects real failure banners and ambiguous reviewer fields, or reject/close. | Current workflow and Makefile integration. |
| https://github.com/rrnewton/hermit/pull/2971 | Authorize publication/review/landing of the repaired Rust-only runner boundary and workflow override audit, or reject/close. | **Parent compatibility migration first**; if landed, this PR precedes https://github.com/rrnewton/hermit/pull/2976 and https://github.com/rrnewton/hermit/pull/2931. |

## Pending hermit-001 adoption/draft survey: 15

These rows remain pending exactly as directed. The required disposition today is to preserve them unchanged and wait for hermit-001's evidence-backed split; neither “land” nor “close” is inferred here.

| Pull request | Current pending reason | Ordered behind |
|---|---|---|
| https://github.com/rrnewton/hermit/pull/2880 | Adoption survey; prior review exists, but the recorded landing hold and qualifying receipt remain. | Qualifying receipt and survey disposition. |
| https://github.com/rrnewton/hermit/pull/2930 | Adoption survey; documentation review exists, but validation refused before execution. | Successful validation and survey disposition. |
| https://github.com/rrnewton/hermit/pull/2976 | Adoption survey; reviewed head is preserved, but qualifying validation did not admit. | https://github.com/rrnewton/hermit/pull/2971, which is behind the parent compatibility migration; then precedes https://github.com/rrnewton/hermit/pull/2931. |
| https://github.com/rrnewton/hermit/pull/2978 | Adoption survey; no exact-head review evidence was found. | Survey disposition, then exact-head review if retained. |
| https://github.com/rrnewton/hermit/pull/2979 | Adoption survey; approval binds an older head. | Survey disposition, then current-head review and validation if retained. |
| https://github.com/rrnewton/hermit/pull/2980 | Adoption survey; focused objection is cleared, but canonical validation is missing. | Survey disposition and canonical validation if retained. |
| https://github.com/rrnewton/hermit/pull/2956 | Draft survey; exact-head review approved, but full validation is absent. | Survey disposition; author must offer it before landing. |
| https://github.com/rrnewton/hermit/pull/2964 | Draft survey; exact-head approval exists, but full validation is absent. | Survey disposition; author must offer it before landing. |
| https://github.com/rrnewton/hermit/pull/2975 | Draft survey; no review or comment evidence. | Survey disposition. |
| https://github.com/rrnewton/hermit/pull/2981 | Draft survey; no review or comment evidence. | Survey disposition. |
| https://github.com/rrnewton/hermit/pull/2982 | Draft survey with an unmerged Reverie dependency. | https://github.com/rrnewton/reverie/pull/536, then survey disposition. |
| https://github.com/rrnewton/hermit/pull/2984 | Draft survey; no review or comment evidence. | Survey disposition. |
| https://github.com/rrnewton/hermit/pull/2985 | Draft survey whose required Reverie PR is closed unmerged. | A replacement or landed equivalent for https://github.com/rrnewton/reverie/pull/542, then survey disposition. |
| https://github.com/rrnewton/hermit/pull/2986 | Draft survey; the named lane was live at census time, but no review/comment evidence existed. | Survey disposition. |
| https://github.com/rrnewton/hermit/pull/2987 | Draft survey; no review/comment evidence existed. | Survey disposition. |

## Verification criteria for completing the drain

- Refresh the cohort before action; record additions or already-closed rows rather than silently changing this denominator.
- For every merge, bind review and validation to the exact head and fetched base, then verify landed file content from freshly fetched `origin/main`.
- For every closure, record the content reason. Age, title, conflict state, or patch identity is not proof of supersession.
- Do not replay stale generated scorecards, pressure evidence, or old selection data. Recompute them on current main without weakening assertions, thresholds, comparators, populations, or failure classifications.
- Apply full deterministic-scheduling and Linux/POSIX review to the scheduler, signal, exit, and file-status changes identified above; re-audit current Reverie APIs where named.
- Recount all open Hermit pull requests with complete pagination. The terminal check is zero open URLs, not zero items in a local landing wave.

The 60 primary table rows above are mutually exclusive: 25 without a new owner decision, 20 requiring an owner decision, and 15 pending survey, for a total of 60.
