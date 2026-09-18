# Test Flakiness Analyzer campaign records

This directory is the durable, machine-checkable layout for the TFA flaky-test
campaign. It contains no experiment result yet. Creating a service directory or
a `PLANNED` record is not a material attempt and does not add to the target
denominator.

## Current result and authorities

At this schema-only revision, 0/0 distinct attempted Buck targets are WORKING,
0/0 are BLOCKED-WITH-NAMED-CAUSE, and 0/0 are NOT-DETERMINABLE. The sweep has
S=0 valid discovery tuples, C/T=0/0 censored/attempted discovery tuples, and
R=0 guest-started Hermit executions. Campaign conclusion: neither.

The classification authority is dev-hermit commit
`9d7f603c43618715b9245c74acb3e164c17d2ae8`, path
`ai_docs/tfa-acceptance-bracket-2026-09-17.md`, blob
`4a53f0c25623ad6371ee86b4556adada1218745d`. The fixed 30-target registry is
derived from commit `ee6562142e3796bb56a1dd0e28b77c1c16a78cc2`, path
`ai_docs/tfa-candidate-sourcing-2026-09-17.md`, blob
`ff01b534352e15a5685b8a41032973d46c089274`.

## Layout and counting identity

```text
tfa-flaky-tests_20260917/
|-- README.md
|-- metadata.json
|-- cohort.json
|-- experiment.schema.json
|-- services/
|   |-- cachelib/
|   |-- folly/
|   |-- logdevice/
|   |-- manifold/
|   |-- mcrouter/
|   |-- mysql/
|   |-- proxygen/
|   |-- thrift/
|   |-- warm_storage/
|   `-- zippydb/
|       `-- <target-slug>--<target-sha256-prefix>/experiment.json
`-- tools/
    |-- test_index_integration.py
    |-- validate.py
    `-- test_validate.py
```

The service is the first path component after `fbcode//`. Per-service
directories are navigation only. The sole counting key is the canonical,
repository-qualified Buck label in `identity.canonical_target`. TestInfra IDs,
selected cases, directories, build modes, cells, seeds, executions, and retries
never create additional examples.

`cohort.json` is registry version 2. Its required metadata is
`schema_version`, `source`, `counting_key`, `expected_target_count`,
`test_selector_convention`, `build_mode_selector_convention`,
`row_digest_convention`, `targets_digest_convention`, `targets_sha256`,
and `targets`. Each target carries `service`, `canonical_target`,
`target_key_sha256`, `target_directory`, `testinfra_id`, `test_name`,
`source_label_qualifier`, `test_selector`, `build_mode_selector`,
`invoked_target`, `standalone_argv`, `candidate_ranking`,
`source_row_line`, and `source_row_sha256`. The validator requires the
published targets digest and exact row identity; a record cannot silently
switch test case, selector, mode, invocation, or ranking data.

Create the target directory and `experiment.json` before any material attempt.
The workflow requires the PLANNED record to be committed before execution. A
later MATERIAL record's `plan_receipt` names that strict-ancestor `commit`,
identical record `path`, and Git `blob`; the validator reads the prior object and
requires it to be the exact same identity, ranking, contract, and plan with all
observation arrays empty. This records Git object order, not authenticated
wall-clock execution order; publication review must establish that the workflow
was followed.

A target record has two lifecycle states:

- `PLANNED`: `cells`, `events`, `samples`, `matchers`, and
  `outcome_history` are empty and `current_outcome` is null.
- `MATERIAL`: at least one retained build, launch, or Hermit execution event
  exists. The target contributes exactly one to A and has exactly one current
  `WORKING`, `BLOCKED-WITH-NAMED-CAUSE`, or `NOT-DETERMINABLE` outcome.

Reclassification appends to `outcome_history` and advances
`current_outcome`; it never increments A.

## Version 2 required-field ledger

`experiment.schema.json` is the structural authority and rejects unknown
fields. The following ledger names every required field and states what its
group protects. Variant rows apply only when that variant is selected.

### Record, identity, registry ranking, and artifacts

| Object | Required fields | Purpose |
| --- | --- | --- |
| record | `schema_version`, `record_type`, `contract`, `identity`, `candidate_ranking`, `plan`, `plan_receipt`, `attempt_state`, `cells`, `events`, `samples`, `matchers`, `outcome_history`, `current_outcome` | Selects schema version 2, records a plan commit before the MATERIAL record commit, and prevents evidence or any exhaustive outcome partition from disappearing. |
| `contract` | `commit`, `path`, `blob` | Pins both the readable acceptance rule and its immutable content. |
| `plan_receipt` | Null for PLANNED; otherwise `commit`, `path`, `blob` | References the exact empty PLANNED record at a strict-ancestor commit. |
| `identity` | `experiment_id`, `service`, `canonical_target`, `target_key_sha256`, `target_directory`, `testinfra_id`, `test_name`, `source_label_qualifier`, `test_selector`, `build_mode_selector`, `invoked_target`, `standalone_argv`, `alias_resolution` | Binds the exact registry case and invocation to one canonical target. |
| `test_selector` | `kind`, `flag`, `value` | Distinguishes `EXACT_FILTER`, `NAMED_TEST`, and `WHOLE_TARGET`; optional `documentation` explains a whole-target selection. |
| `alias_resolution` | `input_target`, `resolved_target`, `method`, `source_revision`, `evidence` | Proves canonicalization at a named source revision. |
| `candidate_ranking` | `pfs`, `native_failure_rate` | Keeps candidate selection evidence separate from Hermit outcomes. |
| `pfs` | `value`, `displayed_percent`, `partition`, `metric`, `role`, `may_parameterize_seed_budget` | Fixes PFS as dated `ranking_only` evidence and forbids using it as the seed failure probability. |
| `native_failure_rate` | `failures`, `executed`, `rate`, `rate_fraction`, `displayed_percent`, `window`, `limit`, `final_only`, `purposes`, `pass_statuses`, `failure_statuses`, `excluded_statuses`, `source`, `role` | Preserves the exact numerator, denominator, status population, provenance, and `planning_prior_only` role. |
| artifact | `artifact_id`, `kind`, `storage`, `locator`, `sha256`, `bytes`, `capture_limit_bytes`, `truncated`, `media_type` | Makes every retained object unique, typed, bounded, integrity-checkable, and honest about truncation and durability. |

Artifact `kind` is one of `STDOUT`, `STDERR`, `RESULT`,
`CANONICAL_INFO`, `SAFEHERMIT_REPORT`, `ADJUDICATION`, `DIAGNOSTIC`, or
`OTHER`.
Storage is `TRACKED`, `EXTERNAL_DURABLE`, or `LOCAL_IGNORED`.


### Sealed plan and sampling design

| Object | Required fields | Purpose |
| --- | --- | --- |
| `plan` | `plan_id`, `plan_sha256`, `sealed_at`, `sealed_by`, `campaign_owner_identity`, `authorized_routes`, `target_oracle`, `measurement_design`, `resource_bounds`, `artifact_policy` | Seals every route, authority, oracle, budget, and resource choice before observations. |
| authorized route | `route_id`, `route_sha256`, `fallback_of`, `authorized_cell_ids`, `canonical_target`, `test_selector`, `build_mode_selector`, `source_revision`, `build_argv`, `launch_argv`, `repeat_matcher`, `environment`, `notes` | Commits the complete route/cell set and prevents target, selector, source, command, matcher, or fallback drift. |
| built-in `repeat_matcher` | `kind`, `comparison` | Seals strict `BitwiseInfoV1` verification before any result exists. |
| equivalent `repeat_matcher` | `kind`, `comparator_argv`, `comparator_artifact` | Seals the fallback comparator command and its complete tracked bytes before any result exists. |
| `target_oracle` | `oracle_id`, `defined_before_runs`, `unmodified`, `selected_test_executed_predicate`, `pass_predicate`, `fail_classes`, `timeout_policy` | Separates the unchanged target oracle from Hermit, harness, host, and outer-watchdog failures. |
| target fail class | `failure_class_id`, `target_owned`, `predicate` | Allows only predeclared target-owned behavior into the FAIL arm. |
| `timeout_policy` | `target_timeout_can_fail`, `target_timeout_predicate`, `target_timeout_failure_class_id`, `outer_timeout_is_censored` | Makes a qualifying target timeout explicit and prevents an outer timeout from being relabelled as target FAIL. |
| `measurement_design` | `purpose`, `swept_coordinate`, `fixed_seed_roots`, `seed_population`, `selection_order`, `without_replacement`, `design_basis`, `miss_probability_override`, `required_valid_seed_tuples`, `max_attempted_seed_tuples`, `stop_rule`, `repeat_min_executions_per_arm`, `repeat_attempt_cap`, `independence_assumption` | Fixes the varying-behavior sample design and fixed-tuple repeat budget before any run. |
| `swept_coordinate` | `flag`, `domain`, `scheduler_modes` | Allows only one declared schedule-affecting coordinate and records any fuzz scheduler modes. |
| `fixed_seed_roots` | `--seed`, `--rng-seed`, `--sched-seed`, `--fuzz-seed` | Shows that every non-swept random stream, especially guest RNG, remains pinned. |
| `seed_population` | `kind`, `values` | Supplies the finite, explicit set from which discovery tuples may be drawn. |
| `selection_order` | `kind`, `seed_values` | Makes replacement of censored tuples deterministic and auditable as a prefix. |
| rate-bound `design_basis` | `kind`, `expected_hermit_fail_probability`, `provenance_kind`, `provenance`, `equivalence_to_hermit`, `max_miss_probability` | Makes the assumed Hermit rate and miss bound explicit without claiming equivalence to native history. |
| finite-population `design_basis` | `kind`, `population_definition`, `population_size`, `selection_rule` | Defines a complete bounded population when a defensible rate is unavailable. |
| non-default `miss_probability_override` | `max_miss_probability`, `owner_identity`, `decided_at`, `authority`, `rationale` | Requires a dated pre-seal campaign-owner decision to depart from the default 0.05 bound. |
| `resource_bounds` | `wall_seconds`, `memory_bytes`, `swap_bytes`, `stderr_bytes`, `retained_log_bytes`, `disk_guard` | Bounds runtime and retained output so host exhaustion cannot masquerade as evidence. |
| `artifact_policy` | `scratch_root`, `terminal_evidence_must_be_durable`, `digest_algorithm` | Separates scratch files from durable terminal evidence and fixes SHA-256 as the integrity rule. |

For a `RATE_BOUND`, the validator derives the smallest N satisfying
`p^N + (1-p)^N <= max_miss_probability`, bounded by the explicit population;
a population too short for the bound is refused instead of hanging or silently
lowering N. With no override, the maximum miss probability is exactly 0.05.
A `FINITE_POPULATION` uses the complete explicit population. In either case,
`max_attempted_seed_tuples >= required_valid_seed_tuples` and cannot exceed
the sealed order.

`route_sha256`, `plan_sha256`, and `cell_sha256` are canonical compact-JSON
digests with only their own digest field omitted. A cell and every event repeat
the plan and route digests; a Hermit event also repeats the cell digest as
`effective_config_sha256`. These bindings make cross-target transplantation
and post-seal edits fail validation.

### Cells, events, samples, and matchers

| Object | Required fields | Purpose |
| --- | --- | --- |
| cell | `cell_id`, `route_id`, `route_sha256`, `plan_sha256`, `frozen_at`, `cell_sha256`, `canonical_target`, `test_selector`, `build_mode_selector`, `target_source_revision`, `hermit_source_revision`, `hermit_binary_sha256`, `test_artifact_sha256`, `guest`, `hermit`, `timing`, `pmu`, `host`, `oracle_id`, `resource_bounds` | Freezes one authorized target/Hermit/configuration cell and all identity-bearing revisions and hashes. |
| `guest` | `argv`, `environment`, `mounts`, `immutable_inputs`, `working_directory` | Captures guest inputs that could otherwise distinguish PASS and FAIL arms. |
| `hermit` | `backend`, `strict`, `chaos`, `relaxations`, `argv`, `scheduler_heuristic`, `swept_coordinate`, `fixed_seed_roots` | Proves ptrace, explicit strict/chaos operation, exact scheduler flags, fixed seeds, and absence of weakening. |
| `timing` | `requested_max_timeslice`, `effective_max_timeslice` | Separates the requested CLI value from a recorded PMU fallback. |
| `pmu` | `available`, `effective_timer`, `fallback_recorded` | Records whether timer preemption changed at runtime rather than hiding a different cell. |
| `host` | `hostname`, `kernel`, `architecture`, `cpu`, `capabilities` | Captures host facts that can change effective execution. |
| event | `event_id`, `kind`, `plan_sha256`, `route_id`, `route_sha256`, `canonical_target`, `test_selector`, `build_mode_selector`, `source_revision`, `cell_id`, `cell_sha256`, `measurement_role`, `started_at`, `finished_at`, `argv`, `result`, `invocation_id`, `replica_index`, `seed`, `effective_config_sha256`, `artifacts` | Retains each BUILD, LAUNCH, or HERMIT_EXECUTION command and binds it to identity, time, configuration, and evidence. Execution-only fields are null for build/launch events. |
| `result` | `exit_code`, `signal`, `timed_out`, `timeout_origin`, `guest_started`, `selected_test_executed_count`, `target_oracle`, `failure_class_id`, `harness_status`, `diagnostic` | Keeps process, timeout, harness, and target outcomes separate and proves that the selected test ran. |
| event or sample `seed` | `flag`, `value` | Identifies the sole swept coordinate and concrete tuple value. |
| sample | `sample_id`, `cell_id`, `seed`, `selection_index`, `status`, `target_outcome`, `failure_class_id`, `censor_reason`, `execution_event_ids` | Represents one unique discovery tuple, its prefix position, and every same-tuple execution. |
| matcher | `matcher_id`, `kind`, `execution_event_ids`, `complete`, `same_target_outcome`, `same_failure_class`, `canonical_info_artifact`, `details` | Binds repeat evidence to one tuple and requires target/class and canonical schedule agreement. |
| BUILTIN_VERIFY `details` | `verified`, `bitwise_parity`, `comparison`, `relaxations` | Requires strict `BitwiseInfoV1` evidence rather than a weaker success label. |
| EQUIVALENT `details` | `comparator_argv`, `comparator_sha256`, `exit_signal_match`, `stdout_match`, `stderr_match`, `canonical_info_match`, `relaxations` | Must match the pre-run sealed comparator bytes/command and be at least as discriminating as built-in verification. |

A route and cell use a template `launch_argv` with one `{SWEEP}` value and
one `{SAFEHERMIT_REPORT}` path. Each Hermit event substitutes a concrete
allowed seed and its own report locator. The command must be a supported
`bin/safehermit ... run ... -- <guest argv>` invocation. The option segment,
not guest arguments after `--`, must contain exactly one ptrace backend,
`--strict`, `--chaos`, every fixed seed, the one swept seed, declared
scheduler modes, a supported scheduler heuristic, an unsigned 64-bit requested
max timeslice, exact safehermit wall/byte/disk bounds, and no forbidden
relaxation (including the `--net`, `--lite`, and `--core-only` aliases).
BUILTIN_VERIFY routes require exactly
`--verify --verify-strict --verify-allow=both`; EQUIVALENT routes prohibit those
flags and bind the retained comparator in the sealed route instead.

Every HERMIT_EXECUTION has exactly one `SAFEHERMIT_REPORT` artifact and a
concrete `--sh-report` path. The complete tracked report must contain one value
for every normal safehermit identity, bound, and terminal field. The validator
matches `binary_sha256` to `cell.hermit_binary_sha256`; requires the wall,
cgroup memory/swap, disk, and stderr-byte bounds to equal the sealed resource
values; rejects missing systemd authority and any true/unknown truncation;
bounds `bytes_written`; and reconciles `runner_exit_code`, final `exit_code`, and
any `process_signal` with the event result. This refuses a one-line report that
merely repeats the expected hash.


## Discovery, confirmation, and durable evidence

A sample is one unique `(cell_id, swept flag, swept value)` tuple. Its
`selection_index` must follow the sealed `seed_values` prefix. Exactly one
referenced event has `measurement_role=DISCOVERY`; later executions of that
same tuple are `CONFIRMATION`. Every Hermit event belongs to exactly one
sample, and that sample lists every Hermit event for the tuple. Duplicate
samples, unassigned executions, and contradictory same-tuple outcomes are
refused.

The counters therefore have different units:

- S is the number of VALID discovery tuples. Same-seed confirmations never
  increase S.
- C is the number of CENSORED discovery tuples; T=S+C is all attempted
  discovery tuples.
- R is the number of HERMIT_EXECUTION events whose `guest_started` is true,
  including confirmation replicas.
- A, W, B, and U count distinct MATERIAL canonical targets, never runs or seeds.

A censored sample has null target outcome and failure class plus a nonempty
`censor_reason`. It cannot hide a set of qualifying, agreeing executions.
A valid sample has one unambiguous PASS or predeclared FAIL class across all its
events. Confirmation disagreement invalidates the tuple; there is no majority
vote.

For `TRACKED` evidence, the locator must be a safe relative regular path under
the campaign root, contain no symlink escape, and have identical blobs in the
working tree, Git index, and current `HEAD`. A staged-only artifact is not
durable. A MATERIAL `experiment.json` must likewise be committed and identical
to `HEAD`; remote publication is verified separately when the campaign commit
lands. The declared `bytes` and `sha256` must match the committed bytes and stay within
`capture_limit_bytes`. `EXTERNAL_DURABLE` locators require an allowed
durable URI scheme. `LOCAL_IGNORED` may support diagnosis while work is in
progress but is never sufficient by itself for terminal or WORKING evidence.
WORKING evidence is stricter: STDOUT, STDERR, RESULT, SAFEHERMIT_REPORT, and
CANONICAL_INFO must all be TRACKED. RESULT JSON repeats the event identity,
argv, seed, and complete result object; canonical-INFO JSON repeats the matcher
identity, execution IDs, comparator verdict, and relaxations. The validator
parses both payloads and requires exact agreement instead of trusting labels.

### Trust boundary

The validator is an offline structural and internal-consistency checker, not an
execution-attestation or identity-authentication system. An author who can
fabricate both a record and its referenced bytes can fabricate a self-consistent
Git history. The validator therefore reports
`evidence_authentication=NOT_CHECKED` even when every schema and consistency
check passes. Its W/B/U counts and campaign conclusion are the claims made by
those committed records; they are not publishable measurements until an
independent evidence review checks the producer, retained raw evidence, plan
timing, and campaign-owner decision through the repository's authenticated
publication path. No field inside the same self-authored record can supply that
independence.

The validator does require `cohort.json`, `experiment.schema.json`, and
`metadata.json` to be identical in the worktree, Git index, and current `HEAD`.
This prevents an uncommitted owner or contract edit from changing a verdict.

Each WORKING execution needs complete, non-truncated durable `STDOUT`,
`STDERR`, `RESULT`, and `SAFEHERMIT_REPORT` artifacts. The RESULT and
SAFEHERMIT_REPORT must be nonempty. Each arm also needs a nonempty durable
`CANONICAL_INFO` matcher artifact. A truncated durable diagnostic may support
BLOCKED or NOT-DETERMINABLE, but never a WORKING witness.

## Outcome ledger and ordered classification

| Object | Required fields | Purpose |
| --- | --- | --- |
| every outcome | `classification_id`, `classified_at`, `supersedes`, `evidence_refs` | Makes classification timed, append-only, and evidence-bound. |
| WORKING outcome | `classification_id`, `classified_at`, `supersedes`, `evidence_refs`, `kind`, `pass_witness`, `fail_witness` | Requires both directions of the bracket. |
| each witness | `sample_id`, `matcher_id` | Selects the exact sample and repeat comparison for one arm. |
| BLOCKED outcome | `classification_id`, `classified_at`, `supersedes`, `evidence_refs`, `kind`, `cause` | Requires one specific causal boundary rather than a generic refusal. |
| `cause` | `cause_id`, `component`, `stage`, `diagnostic`, `causal_argument`, `route_evidence`, `campaign_owner_adjudication` | Binds a named cause to failed retained attempts on every authorized route and to owner scope authority. |
| blocker `diagnostic` | `kind`, `code`, `message` | Types the boundary and ties its exact code/message to retained event results. |
| `route_evidence` entry | `route_id`, `cell_ids`, `evidence_event_ids` | States exactly which authorized routes, cells, and retained attempts support a classification. |
| `campaign_owner_adjudication` | `owner_identity`, `decided_at`, `authority`, `disposition`, `rationale`, `evidence_artifact_id` | Records a dated, committed claimed owner decision whose identity must equal the committed campaign metadata; independent publication review authenticates it. |
| NOT-DETERMINABLE outcome | `classification_id`, `classified_at`, `supersedes`, `evidence_refs`, `kind`, `route_evidence`, `inspected_commands`, `inspected_artifacts`, `missing_or_ambiguous_facts`, `measurement_budget_exhausted`, `pending_in_scope_measurements` | Records the inspected subset, exact unresolved facts, and whether any in-scope measurement remains. |

Apply outcomes in this order:

1. WORKING requires one cell, distinct allowed PASS and FAIL seeds, at least two
   qualifying agreeing executions in each sample, complete artifact sets, and
   a matcher for each arm. Once this bracket exists, B or U is rejected. An
   existence-discovery plan may stop early; it need not consume N or every
   fallback route after W is proved.
2. BLOCKED-WITH-NAMED-CAUSE requires failed, durable attempts covering every
   authorized route and its authorized cell IDs. Cause stage, diagnostic kind
   and message, event kind/result, evidence time, and owner adjudication must
   agree. Each cited event's committed DIAGNOSTIC bytes must equal its result,
   and committed ADJUDICATION JSON must equal the owner decision. Only
   OUTSIDE_SCOPE supports a campaign negative conclusion.
3. NOT-DETERMINABLE covers every remaining material attempt. A non-exhausted U
   may report a nonempty inspected route subset and must name pending in-scope
   work. An exhausted U must cover every authorized route/cell, have no pending
   work, and derive exhaustion for every authorized cell from either N valid
   discovery tuples or the attempted-tuple cap. If both target arms were seen,
   unfinished same-tuple confirmations also prevent exhaustion until a witness
   or the repeat cap resolves them. Its `evidence_refs`, `inspected_commands`,
   and `inspected_artifacts` must bind to the exact events cited by
   `route_evidence`, not merely to other record data.

## Paper walkthrough: Cachelib EventTracker

This section is a paper classification, not measured evidence. No command below
was run.

The registry entry for
`fbcode//cachelib/common/tests:event-tracker-test` has service `cachelib`,
TestInfra ID `562950242844554`, selected test
`EventTrackerTest.NvmCacheWithEventTracker`, selector kind `EXACT_FILTER`
with flag `--exact`, build-mode selector `@fbcode//mode/dev`, invoked target
`fbcode//cachelib/common/tests:event-tracker-test`, target digest
`4b6700e9a3a0850f45178e2c684e6347cc9402500a1c265e8835f9b6c8551d1b`,
and directory
`services/cachelib/event-tracker-test--4b6700e9a3a0`.

Its PFS 0.2299 remains ranking-only. Native history is 1/29 =
0.034482758620689655. If the campaign owner uses that native rate only as a
non-equivalent planning prior, the default 5% miss bound requires N=86 valid
distinct discovery seeds. Before any attempt, the plan must explicitly list at
least that population and its complete order, choose an attempted cap, seal all
authorized routes/cells, and fix the oracle, bounds, scheduler modes, all
non-swept seed roots, and artifact policy. The registry row does not make those
choices.

For a hypothetical bracket, one frozen ptrace/strict/chaos cell sweeps only
`--sched-seed`. Its template goes through
`bin/safehermit --sh-report {SAFEHERMIT_REPORT} run ... -- <guest>`.
For seed 7:

- replica 1 is the sole DISCOVERY event for that tuple;
- replica 2 is CONFIRMATION, uses the same `invocation_id` with distinct
  `replica_index`, and keeps the entire tuple fixed;
- both events are target PASS, use concrete per-event report locators, and retain
  committed, byte-identical STDOUT, STDERR, nonempty RESULT, and nonempty
  SAFEHERMIT_REPORT artifacts; and
- the report proves the frozen Hermit binary SHA-256 while the matcher retains
  canonical INFO and the strict built-in verify result.

Seed 19 is represented the same way but both executions reach the same
predeclared target-owned FAIL class. The two samples are the only discovery
tuples; their two confirmation events do not create more samples.

That evidence permits WORKING with A=1, W=1, B=0, U=0, S=2, C/T=0/2, and R=4.
If instead all 86 valid discovery seeds PASS and every authorized cell budget
is exhausted with no pending measurement, the target is NOT-DETERMINABLE, not
blocked. If a matched retained unsupported-operation diagnostic stops every
authorized route and the campaign owner records OUTSIDE_SCOPE after the
evidence, the target is BLOCKED-WITH-NAMED-CAUSE.

## Validation and reporting

Run from the repository root:

```sh
python3 experiments/tfa-flaky-tests_20260917/tools/validate.py
python3 experiments/tfa-flaky-tests_20260917/tools/validate.py --print-plan-digest PATH/experiment.json
python3 -m unittest discover -s experiments/tfa-flaky-tests_20260917/tools -p 'test_*.py'
```

The validator checks the exact 30-target registry, service/path/hash identity,
canonical plan/route/cell bindings, explicit argv and seed semantics, artifact
commit ancestry and payload integrity, sample ownership, matcher evidence,
ordered outcomes, and
derived counters. It also labels those counters `evidence_authentication=NOT_CHECKED`.
This empty schema root is not a measurement. The first target record must
regenerate the derived `experiments/INDEX.md`; until then `build-index.py`
mechanically excludes the `SCHEMA_ONLY_NO_EXPERIMENTS_RUN` root rather than
inventing a measurement row. Any `experiment.json`, including a PLANNED or
malformed record, overrides a stale schema-only status and is indexed.

Use this required summary form:

> At `<artifact/revision and time>`, `<W>/<A>` distinct attempted Buck
> targets are WORKING, `<B>/<A>` are BLOCKED-WITH-NAMED-CAUSE, and
> `<U>/<A>` are NOT-DETERMINABLE. The sweep contains `<S>` distinct valid
> discovery tuples, `<C>/<T>` censored/attempted discovery tuples, and
> `<R>` guest-started Hermit executions; none of S, C, T, or R is the target
> denominator. Campaign conclusion: `<SUCCESS | NEGATIVE RESULT | neither>`,
> because `<exact rule>`. Evidence authentication: `NOT_CHECKED`; independent
> publication review `<review receipt>` is required before reporting this result.

SUCCESS requires W>=3 distinct targets. NEGATIVE RESULT requires A>=30, W=0,
all records terminal, every blocker owner-adjudicated OUTSIDE_SCOPE, and every U
derived exhausted with no pending in-scope measurement. Otherwise the
conclusion is neither.
