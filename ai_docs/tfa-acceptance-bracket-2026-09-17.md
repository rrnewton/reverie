# Test Flakiness Analyzer acceptance bracket

**Decision date:** 2026-09-17
**Scope:** TFA experiments on the Hermit ptrace backend
**Task:** `tfa-define-acceptance-bracket`

This contract is fixed before the TFA candidate experiments begin. It defines
what one example is, what evidence makes that example work, how inconclusive
evidence is represented, and what the campaign's two stopping conditions mean.
Changing this contract after observing campaign results requires a dated
amendment; it must not retroactively improve an already measured verdict.

The prerequisite source audit at Hermit
`79da523137dbac400faca6130593361b58a01468` established that `--chaos` is a
seeded deterministic scheduling mechanism and composes with `--strict` without
a relaxation. It also established that `--seed` is a shared fallback for guest,
scheduler, and fuzz randomness. A counting bracket therefore sweeps --sched-seed
and pins --rng-seed and --fuzz-seed. It may instead sweep --fuzz-seed only when
--fuzz-futexes or --chaos-target-races is fixed on and that stream drives a
declared scheduler choice. Guest-RNG and other seed sweeps may be recorded, but
they cannot count toward W.

## Units and identity

A **target** is one canonical, repository-qualified Buck test target, after
resolving aliases. The initial population is the 30 distinct targets in
[`tfa-candidate-sourcing-2026-09-17.md`](tfa-candidate-sourcing-2026-09-17.md).
The target label is the counting key.

TestInfra IDs, exact test-case filters, build modes, shards, Hermit revisions,
scheduler configurations, seeds, runs, retries, and service-directory aliases
do not create additional targets. Two exact cases from one Buck target still
count as one target. Re-running or reclassifying a target replaces its current
verdict and preserves its history; it never increments the target count.

An **execution** is one fresh invocation of the selected target under Hermit. A
single `hermit run --verify` command creates two executions and may satisfy the
two-execution repeat requirement if both complete and the retained evidence
shows the required comparison. A seed discovery execution can count as one of
the two only when it used the exact frozen tuple and retained all required
evidence.

A **material attempt** exists when either:

1. at least one qualifying Hermit execution starts for the target; or
2. a concrete build or launch attempt is made and its command and result are
   retained, even if no guest execution starts.

Metadata lookup, candidate ranking, source inspection, or merely assigning a
target is not an attempt. One material target contributes exactly one to the
attempted-target denominator, irrespective of how many executions it causes.
Causal evidence decides whether a pre-guest failure is BLOCKED-WITH-NAMED-CAUSE
or NOT-DETERMINABLE; it does not decide whether the attempt occurred.

## Frozen cell and execution tuple

Before looking at seed outcomes, record one **cell** containing:

- canonical target and exact selected test case;
- build mode and exact target/test source revisions;
- test executable digest and Hermit executable digest/revision;
- complete guest argv, environment, mounts, immutable inputs, working
  directory, and target-oracle definition;
- ptrace backend, explicit --strict, explicit --chaos, all scheduler and
  timeslice parameters, PMU capability/effective timer state, and the name and
  domain of one allowed schedule-affecting seed coordinate: --sched-seed, or
  --fuzz-seed only when its associated scheduling mode is fixed on and its
  choices are declared; every other seed root is fixed, and --rng-seed is always
  fixed;
- host capability facts that can change execution, including kernel and CPU;
- the complete authorized cell/route set for this target, fixed before any
  attempt;
- bounds and the paths/digests of raw stdout, stderr, result, and matcher
  artifacts.

An **execution tuple** is the frozen cell plus one concrete value of the swept
seed coordinate. The PASS and FAIL arms must share the cell and use two distinct
execution tuples. Repeats within one arm must hold the complete execution tuple
fixed. The arms may differ only in the predeclared seed value plus artifact
output paths. Changing the test, binary, build mode, assertion, comparator,
timeout, backend, non-swept seed, scheduler heuristic, timeslice, environment,
input, or relaxation state creates a different cell and cannot complete the
bracket.

Only ptrace executions with explicit `--strict --chaos` qualify. Results using
`--no-sequentialize-threads`, `--no-deterministic-io`,
`--allow-unsupported-syscalls`, `--strace-only`, `--namespace-only`, or any
equivalent weakening do not qualify. PMU fallback and every other effective
configuration change must be recorded; an undeclared fallback invalidates the
cell rather than silently changing it.

## The target oracle and repeat matcher

The target's own unmodified test oracle determines PASS and FAIL. Define that
oracle before inspecting seed results.

- **PASS arm:** the exact selected test demonstrably executed, was not skipped
  or filtered to zero tests, and satisfied its original success oracle.
- **FAIL arm:** that same test demonstrably executed and reached a predeclared,
  target-owned failure class: for example its assertion, sanitizer finding, or
  other original test failure signal.

A Hermit panic, unsupported syscall refusal, runner/setup error, dependency
failure, output truncation, unattributed outer timeout, OOM/SIGKILL, or another
test's failure is not a FAIL arm. It may establish a blocker, but it cannot
establish the flaky behavior. A timeout qualifies only when the predeclared
target oracle includes that timeout class and retained evidence distinguishes
the target's behavior from a Hermit, harness, host, or outer-watchdog failure.
Exit zero alone is not a PASS unless the target oracle and nonzero test
execution are also established.

The **repeat matcher** requires both executions to complete, produce the same
target verdict and failure class, and match Hermit's canonical INFO-level
scheduler evidence. The preferred path is `--verify --verify-strict` with
`verified=true`, `bitwise_parity=true`, comparison `BitwiseInfoV1`, and
`relaxations=none`. If the built-in verifier cannot be used safely, an
equivalent retained comparison must cover exit/signal, stdout, stderr, and a
nonempty canonical INFO scheduler log under the same policy. A matching
`--verify-allow=both` result establishes repeatability only; the target oracle,
not the matcher, decides whether that result is PASS or FAIL.

## WORKING: the two-directional bracket

A target is **WORKING** if and only if all of these are true:

1. One frozen cell has two distinct execution tuples for its predeclared allowed
   schedule-affecting seed coordinate.
2. The PASS tuple has at least two qualifying executions, and every execution
   is target PASS and satisfies the repeat matcher.
3. The FAIL tuple has at least two qualifying executions, and every execution
   is the same target-owned FAIL class and satisfies the repeat matcher.
4. Retained evidence binds every execution to the cell and proves both the
   target oracle and the within-seed comparison.

This is deliberately two-directional. A reproducible refusal or failure alone
cannot distinguish schedule control from a harness that fails everything. The
PASS arm proves the same harness can accept the target. A reproducible PASS
alone shows compatibility but does not reproduce the flake.

Any oracle or matcher disagreement within an exact execution tuple invalidates
that witness. Do not majority-vote it into a deterministic arm. More working
cells or more repeats do not let one target contribute more than one WORKING
example.

## Exhaustive per-target outcomes

Every materially attempted target receives exactly one current outcome. Apply
these rules in order.

### 1. WORKING

Use WORKING exactly when the complete two-directional bracket above exists.
Record the PASS and FAIL witness seeds, both sets of repeat artifacts, the
frozen cell, and the target oracle.

### 2. BLOCKED-WITH-NAMED-CAUSE

Use **BLOCKED-WITH-NAMED-CAUSE** only when a specific, causally sufficient
boundary prevents completion of every authorized bracket route currently in
scope. Record:

- component and stage: build, launch, Hermit execution, target execution,
  oracle, or repeat;
- the exact diagnostic, errno, signal, unsupported operation, or other
  discriminating evidence;
- the causal argument connecting that evidence to the blocked arm;
- which cells and fallback routes were inspected; and
- the campaign owner's recorded `within-scope` or `outside-scope` repair
  disposition and rationale.

An error string without causal attribution, an unattributed timeout, or failure
to find the second arm in a seed budget is not a named cause. If one cell is
blocked but another authorized route remains unexplained, classify the target
as NOT-DETERMINABLE rather than generalizing the narrower blocker.

A repair is `within-scope` when it is a bounded Hermit compatibility change that
preserves ptrace, strictness, the target, and its oracle. Dropping strictness,
changing or disabling the flaky test, weakening its oracle, switching backend,
or building a major external-service or scheduler subsystem is
`outside-scope`. Host access or capacity is not terminal until available
routing options have been checked. Only the campaign owner's recorded
adjudication makes this field final; absent that decision, the repair remains
pending and prevents a NEGATIVE RESULT.

### 3. NOT-DETERMINABLE

Use **NOT-DETERMINABLE** for every remaining case. Its record must name what was
inspected, the commands/configurations and artifacts available, and the exact
fact that remains missing or ambiguous. Typical cases are:

- only PASS or only FAIL was observed after the declared seed budget;
- a witness tuple disagreed across repeats;
- output was missing, truncated, stale, or could not be bound to the target;
- the selected test may not have run, or the target oracle is ambiguous;
- external state or multiple changing seed/configuration axes confounded the
  result; or
- the investigation stopped before a named causal blocker was established.

NOT-DETERMINABLE is not BLOCKED and is not evidence that Hermit reproduced or
failed to reproduce the flake. It counts once in the attempted-target
denominator after a material attempt, but it never counts in the WORKING or
named-blocker numerator.

## Counting and campaign termination

Let `A`, `W`, `B`, and `U` be the numbers of distinct canonical targets whose
current outcomes are attempted, WORKING, BLOCKED-WITH-NAMED-CAUSE, and
NOT-DETERMINABLE. The accounting invariant is:

```text
A = W + B + U
```

The unit of every term is **targets**. Never substitute seeds, parameter tuples,
executions, retries, result rows, TestInfra IDs, or service lanes. Ten thousand
seeds on one target are `A=1`; thirty configurations of one target are `A=1`.

The campaign has exactly two terminal conclusions:

- **SUCCESS:** `W >= 3` distinct targets.
- **NEGATIVE RESULT:** `A >= 30`, `W = 0`, every record is terminal, every named
  blocker has a campaign-owner `outside-scope` disposition, and every
  NOT-DETERMINABLE target exhausted its predeclared measurement budget with no
  pending in-scope measurement that could resolve it.

An under-budget target, an unresolved evidence gap, or a `within-scope` or
unadjudicated repair that has not been tried prevents the negative conclusion.
NOT-DETERMINABLE targets still appear in `A` because they were materially
attempted, but they support only the bounded statement “no working bracket was
observed under the recorded budget.” They must never be rewritten as blockers
or failures to make the negative result look cleaner.

The final report must give `W/A`, `B/A`, and `U/A` with counts and target names.
A negative report says only that zero WORKING examples were observed among the
recorded targets and budgets; it must not say that Hermit cannot reproduce flaky
tests in general.

## Fixed-property checks versus varying-behavior samples

Within one exact execution tuple, reproducibility is a direct fixed-property
check. The minimum is two qualifying guest executions per arm: the observation
and one repeat. One strict `--verify` invocation may supply those two
executions. Any additional same-seed repeats are redundancy or matcher checks,
not independent race samples and not a larger sample size.

Searching different seeds observes varying behavior. Before the sweep, record:

- the seed population and selection/order rule;
- the required number `N` of valid, distinct seed tuples and a separate maximum
  attempted-seed budget;
- whether the purpose is existence discovery or rate estimation;
- the expected rare-arm rate and its provenance; and
- the stop rule and miss probability or confidence level.

A **valid seed tuple** completed and produced an unambiguous target PASS or
target FAIL. Setup/Hermit/infrastructure errors and ambiguous results are
censored, do not enter `N`, and must be reported separately as
`censored/attempted`. For a powered sweep, replace a censored tuple using the
predeclared seed-order rule until `N` valid tuples or the attempted-seed cap is
reached. Reaching the cap first makes the target NOT-DETERMINABLE.

If valid independent seed outcomes are modeled as Bernoulli with FAIL
probability `p`, the probability of observing both arms in `N` seeds is

```text
1 - p^N - (1 - p)^N.
```

The campaign's default maximum miss probability is 5%. Choose the smallest `N`
for which `p^N + (1 - p)^N <= 0.05`. This gives `N=6` for `p=0.5`, `N=86` for
`p=0.0345`, and `N=74` for `p=0.96`. A different bound requires a dated,
pre-experiment campaign-owner decision. An empirical native failure rate k/n may
be used only as a clearly labelled planning prior for Hermit seed outcomes;
equivalence is not implied. PFS is a probability of flaky state, not a failure
probability, and must never be substituted for p; it ranks candidates only.
If independence or a defensible p is unavailable, predeclare a finite seed
population or a minimum detectable rare-arm rate and state that the result is
bounded to that design.

Stopping as soon as both arms appear is valid for the existence claim needed by
WORKING, followed by the fixed-tuple repeats. It is not a valid way to estimate
a failure rate. To report a rate, finish the predeclared `N`, report
`failures/N` and `passes/N` (which sum to `N`), report censored/attempted
separately, and give an interval and the seed-selection assumptions. Do not add
same-seed confirmation executions to `N`.

## Cold classification cases

These cases are part of the contract. A reader applying the rules above must
produce these results without author intent. “One frozen cell” below includes
all required provenance and repeat-matcher evidence.

| Observation | Verdict and count |
| --- | --- |
| Seed 7 is PASS twice and seed 19 is the same target-owned FAIL twice, with one frozen strict ptrace cell | WORKING; `A=1`, `W=1` |
| PASS seed repeats, but a candidate FAIL seed was observed only once | NOT-DETERMINABLE until the FAIL witness repeats; `A=1`, `U=1` |
| The same exact execution tuple is PASS once and FAIL once | NOT-DETERMINABLE; the tuple is not a deterministic witness; `A=1`, `U=1` |
| One hundred valid distinct seed tuples all PASS and the powered budget is exhausted | NOT-DETERMINABLE, not WORKING or BLOCKED; `A=1`, `U=1` |
| All authorized routes were inspected; each reproducibly stops at the same proved unsupported syscall before the selected test executes, and the campaign owner recorded a repair disposition | BLOCKED-WITH-NAMED-CAUSE; never a FAIL arm; A=1, W=0, B=1, U=0 |
| Setup exits nonzero, times out, or runs zero selected tests without a causal diagnosis | NOT-DETERMINABLE; never a FAIL arm; `A=1`, `U=1` |
| PASS and FAIL are each repeatable, but the FAIL arm uses `--no-sequentialize-threads` | NOT-DETERMINABLE from the submitted evidence; the relaxed arm is disqualified; `A=1`, `U=1` |
| Thirty parameter cells on one target each satisfy the two-arm bracket | WORKING once, never 30 examples; `A=1`, `W=1` |
| Thirty targets have terminal named blockers, zero are WORKING, and every blocker has an `outside-scope` owner disposition | NEGATIVE RESULT: `A=30`, `W=0`, `B=30`, `U=0` |
| Twenty-nine targets have terminal named blockers with outside-scope dispositions, and one target is NOT-DETERMINABLE and still under budget | Neither terminal condition has fired; A=30, W=0, B=29, U=1 |
| Three distinct targets each satisfy both repeated arms | SUCCESS; A=3, W=3, B=0, U=0 |

## Required reporting sentence

Every progress and final summary must instantiate this form:

> At `<artifact/revision and time>`, `<W>/<A>` distinct attempted Buck targets
> are WORKING, `<B>/<A>` are BLOCKED-WITH-NAMED-CAUSE, and `<U>/<A>` are
> NOT-DETERMINABLE. The sweep contains `<S>` distinct valid seed tuples,
> `<C>/<T>` censored/attempted seed tuples, and `<R>` guest executions; none of
> `S`, `C`, `T`, or `R` is the target denominator. Campaign conclusion:
> `<SUCCESS | NEGATIVE RESULT | neither>`, because `<exact rule>`.

This contract defines reproduction only. Root-cause analysis, flaky-test repair,
Hermit analyzer development, and flakiness-model changes remain out of scope.
