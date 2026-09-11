# Hermit open pull-request drain disposition plan

## Scope and measurement

This plan consolidates the complete Hermit census, the 60-item triage, the 13-objection current-main assessment, and the 21-item rebase assessment. The census was measured from **2026-09-11 09:40:23 through 09:40:26 EDT**. GitHub returned all 60 open `rrnewton/hermit` pull requests with `hasNextPage=false`; 35 were non-draft and 25 were drafts. The two content assessments used Hermit main `c7520a6c38731128ec3e0bf802d80b0d1cca80c0`.

This is a timestamped disposition plan, not a live queue. Refresh open state, exact heads, TaskGraph ownership, fetched main, reviews, and validation before acting. The report does not authorize bypassing review, validation, or landing controls.

Source records:

- `fresh-open-pr-census-three-repos-20260911`
- `triage-60-open-hermit-prs-read-only-20260911`
- `assess-13-hermit-objections-against-current-main`
- `assess-21-hermit-rebase-group-worth-reconstructing`

## Reconciled counts

| Survey result | Required disposition | Owner decision now? | Count |
|---|---|---:|---:|
| Landing candidates | Exact-head review and validation, then land | No | 6 |
| Rebase/reconstruction, still wanted | Reconstruct 17 PR requirements as 14 current-main deliverables; land replacements and close replaced originals | No | 17 |
| Rebase/reconstruction, obsolete | Close with the recorded current-main reason | No | 4 |
| Objection met on main | Close as superseded by the stricter current-main mechanism | No | 2 |
| Proved superseded | Close with the recorded content-on-main proof | No | 5 |
| Objection still live | Owner chooses focused repair/adoption or rejection and closure | **Yes** | 11 |
| Adoption or draft survey | Keep pending until hermit-001 reports; do not guess the split | Pending survey | 15 |
| **Total** |  |  | **60** |

Thus 34 items already have an evidence-backed action, 11 need an owner decision, and 15 remain pending the assigned survey. The two met objections are not landing candidates: main solved their concerns by a stricter route than the pull requests proposed, so their old branches are closer to superseded work.

## Ordering to zero open

1. Refresh the 60-PR cohort and claims immediately before mutation. Close the 11 items already proved obsolete, superseded, or replaced by stricter main content.
2. Obtain the 11 owner decisions. The first decision in the landing chain is the repaired form of https://github.com/rrnewton/hermit/pull/2971.
3. Preserve this mandatory chain: the approved parent `dev-hermit` compatibility migration (recorded at local commit `72f16fe` plus its follow-up) must land before https://github.com/rrnewton/hermit/pull/2971; that PR must land before https://github.com/rrnewton/hermit/pull/2976; that PR must land before https://github.com/rrnewton/hermit/pull/2931. Rebase, re-review, and revalidate a later item whenever fetched main differs from its reviewed base.
4. Land the other five landing candidates after normal exact-head review and validation, ordered further only by fresh path overlap or newly discovered dependencies.
5. Reconstruct the 17 wanted old branches as 14 deliverables. Fold https://github.com/rrnewton/hermit/pull/2927 into https://github.com/rrnewton/hermit/pull/2970; fold https://github.com/rrnewton/hermit/pull/2958 into https://github.com/rrnewton/hermit/pull/2969; and take only the unique shutdown delta from https://github.com/rrnewton/hermit/pull/2836. Land https://github.com/rrnewton/hermit/pull/2970 before the parity work in https://github.com/rrnewton/hermit/pull/2945. Close each old PR after its reconstructed requirement lands or the owner explicitly rejects it.
6. Incorporate hermit-001's disposition of the 15 pending adoption/draft items. Two known external constraints must remain visible: https://github.com/rrnewton/hermit/pull/2982 depends on https://github.com/rrnewton/reverie/pull/536, and https://github.com/rrnewton/hermit/pull/2985 needs a replacement or landed equivalent for closed-unmerged https://github.com/rrnewton/reverie/pull/542.
7. Recount GitHub from a fully paginated read. Zero is reached only when every listed URL is merged or closed with its recorded reason; prepared branches and local replacements do not reduce the open count.

## No owner decision required: 34

### Landing candidates: 6

| Pull request | Required disposition | Ordered behind |
|---|---|---|
| https://github.com/rrnewton/hermit/pull/2887 | Complete independent exact-head review and canonical validation, then land. | No dependency recorded by the four surveys. |
| https://github.com/rrnewton/hermit/pull/2931 | Rebuild on fetched main if necessary, preserve the reviewed documentation content, validate, and land. | https://github.com/rrnewton/hermit/pull/2976, which is behind https://github.com/rrnewton/hermit/pull/2971 and the parent compatibility migration. |
| https://github.com/rrnewton/hermit/pull/2963 | Complete exact-head review and validation, then land. | No dependency recorded by the four surveys. |
| https://github.com/rrnewton/hermit/pull/2966 | Preserve its focused measurement review, complete full landing validation, then land. | No dependency recorded by the four surveys. |
| https://github.com/rrnewton/hermit/pull/2968 | Complete exact-head review and validation, then land. | No dependency recorded by the four surveys. |
| https://github.com/rrnewton/hermit/pull/2973 | Reconfirm current ownership, complete exact-head review and validation, then land. | No dependency recorded by the four surveys. |

### Still wanted; reconstruct on current main: 17

| Pull request | Required disposition | Ordered behind |
|---|---|---|
| https://github.com/rrnewton/hermit/pull/2427 | Reconstruct typed `SandboxDenied`/`InfrastructureError` producer authority and retained-output reconciliation; preserve fail-closed disagreement tests; review and validate, then land or close in favor of the replacement. | Current cell-result and pressure schemas. Author lane absent, so this is adoption work. |
| https://github.com/rrnewton/hermit/pull/2694 | Reconstruct inherited-description aliasing and `F_SETFL` containment; fix the stale 24-versus-32 executed-test gate; perform determinism and Linux-semantics review. | Current Detcore file-description model. Author lanes absent, so this is adoption work. |
| https://github.com/rrnewton/hermit/pull/2738 | Port `Selected`/`NotSelected` vocabulary with legacy reading and regenerate current artifacts without changing membership counts. | Current scorecard schema and current 707/188 population. Author lane absent. |
| https://github.com/rrnewton/hermit/pull/2745 | Preserve the historical SaBRe `ppoll` event/syscall comparison in the current approved documentation location and update time-sensitive wording. | Current documentation layout. Author lanes absent. |
| https://github.com/rrnewton/hermit/pull/2747 | Reconstruct the physical-thread-exit barrier through current process-exit, robust-list, and deferred-admission paths; prove deterministic callback ordering and `CLONE_CHILD_CLEARTID` semantics. | Current scheduler; full deterministic-scheduling and Linux/POSIX review. Author lane absent. |
| https://github.com/rrnewton/hermit/pull/2765 | Port only the IPC chaos promotion, retain 32 seeds and `min_distinct=16`, rerun on current main, and regenerate plan/cells/scorecard. | Fresh current-main evidence. No current author lane identified. |
| https://github.com/rrnewton/hermit/pull/2797 | Port the typed pass/product/infrastructure/no-result residual to current schema 7 with precedence and mixed-attempt tests intact. | Current result schema. Author lane absent. |
| https://github.com/rrnewton/hermit/pull/2810 | Port mode-specific result binding and rebuild its closed consumer on the current schema. | Current ledger and scorecard mode rules. Author lane absent. |
| https://github.com/rrnewton/hermit/pull/2822 | Port only the typed termination consumer/wire path, retain schema-1 reading, and refuse missing or inconsistent current rows. | Current `StepOutcome`/`FailureClass` API. Author lane absent. |
| https://github.com/rrnewton/hermit/pull/2836 | Extract only the unique two-commit SaBRe shutdown-report buffering delta; do not replay its obsolete concurrency/build stack or Reverie pin. | Current scheduler/tool/backend paths; full deterministic-scheduling and Linux/POSIX review. Author lane absent. |
| https://github.com/rrnewton/hermit/pull/2925 | Port bounded implicit failed-log-pair retention with atomic pair retirement and reader locking; leave explicit retained evidence unbounded. | Confirm the proposed 64-pair implicit-cache policy with ownership. Author lane absent. |
| https://github.com/rrnewton/hermit/pull/2927 | Preserve its immutable-DAG invariants and tests by folding them into the later one-DAG reconstruction; close this older PR when the combined replacement lands. | https://github.com/rrnewton/hermit/pull/2970. Author lane absent. |
| https://github.com/rrnewton/hermit/pull/2945 | Port strict ptrace-reference comparison and current scorecard evidence with current backend eligibility. | Land the combined one-DAG reconstruction in https://github.com/rrnewton/hermit/pull/2970 first. Current mega-lander assignment was not established. |
| https://github.com/rrnewton/hermit/pull/2948 | Requalify every LiteInst candidate on current main; do not replay the stale three-run promotion data; keep comparator, retry, and selection rules unchanged. | Fresh current-main evidence. Author lane absent. |
| https://github.com/rrnewton/hermit/pull/2958 | Fold its evidence, retention, restart, no-clobber, and backend-refusal preparation into the later harness-managed cutover; close this prerequisite PR when the combined replacement lands. | https://github.com/rrnewton/hermit/pull/2969 and current Reverie API review. |
| https://github.com/rrnewton/hermit/pull/2969 | Rebuild the harness-managed two-run cutover on current pressure/history schemas while preserving DBT evidence and explicit SaBRe/DBT exclusions. | Include https://github.com/rrnewton/hermit/pull/2958; re-audit the current Reverie API. |
| https://github.com/rrnewton/hermit/pull/2970 | Regenerate one immutable DAG from current typed source and integrate current Rust-runner and override guards. | Include the useful invariants from https://github.com/rrnewton/hermit/pull/2927; precedes https://github.com/rrnewton/hermit/pull/2945. |

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

## Owner decision required: 11 live objections

For each row the owner must choose either the named focused repair/adoption followed by fresh review and validation, or rejection and closure. A mergeable GitHub state is not clearance.

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

The 60 primary table rows above are mutually exclusive: 34 without an owner decision, 11 requiring an owner decision, and 15 pending survey, for a total of 60.
