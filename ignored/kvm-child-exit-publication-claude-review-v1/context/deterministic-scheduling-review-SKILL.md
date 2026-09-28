---
name: deterministic-scheduling-review
description: "Review nontrivial Hermit scheduler designs and changes for soundness of the determinism guarantee and compliance with Linux and POSIX semantics. Use for scheduler ordering, blocking, wakeup, signal, timer, run-queue, virtual-time, record/replay, or parallel-execution changes; this is a review procedure, not implementation authorization."
---

# Deterministic scheduling deep review

## Purpose

Try to refute a scheduler change on two independent axes:

1. Soundness of the determinism guarantee.
2. Compliance with Linux and POSIX semantics.

A native-looking result is not sufficient evidence of determinism. Repeatability
is not sufficient if the implementation achieves it by exposing behavior Linux
does not permit. A design argument is not sufficient unless the current code and
tests implement and exercise the claimed ordering.

This is a review skill. Do not implement the change unless the user separately
authorizes implementation.

## Ground before reading the proposal

Read these sources in this exact order. The order is part of the review: it makes
the existing model and product contract shape the questions asked of the change,
rather than letting the proposal define its own standard of correctness.

1. Read the complete ASPLOS 2020 paper *Reproducible Containers*, including its
   artifact appendix. Use the published paper identified by DOI
   `10.1145/3373376.3378519` or an author-hosted byte-for-byte copy. If it is not
   on disk, fetch the full paper through the host's supported internet route and
   keep any downloaded PDF outside the repository. Do not substitute an abstract,
   citation, prior review, or summary.
2. Read `hermit/detcore/src/scheduler.rs` completely, including its tests, at the
   revision being used as the proposal's base. Follow repository instructions
   before reading code. Do not limit this pass to named functions or reported
   line numbers: scheduler invariants are distributed across request creation,
   blocked pools, drain points, turn selection, commit, wakeup, re-enqueueing,
   virtual-time advancement, teardown, diagnostics, and tests.
3. Read `PROJECT_VISION.md` and `ai_docs/hermit-v2-roadmap.md`. Treat the
   roadmap's capability definition as current and its dated measurements as
   history. Read any newer document that those files designate as their
   replacement.
4. Only after steps 1-3, read the proposal and, if one exists, its complete diff.

If a required source remains unavailable, report that limitation and stop before
the next item. Never replace a missing primary source with a summary.

## What each source establishes

The paper defines the contract the scheduler serves:

- A run is a pure function of the container configuration and initial filesystem
  state, subject only to explicitly bounded external failure.
- Determinism means each read observes the same value on repeated runs;
  reproducibility extends that result across supported machines.
- DetTrace exports the Linux/POSIX interface by choosing one permitted behavior
  where the interface allows several.
- System calls are serialized, potentially blocking calls become nonblocking
  probes, and fair revisiting supplies progress without dependency tracking.
- The paper's prototype did not support guest-to-guest signals. A later signal
  design is an extension requiring its own proof, not behavior validated by the
  paper.

The complete scheduler supplies the implementation model that comments near the
change cannot supply:

- where a turn is selected, skipped, committed, and re-enqueued;
- which threads are in the run queue and which are held in each blocked pool;
- where asynchronous observations are converted into deterministic scheduler
  state;
- which run-queue mutations are deferred to `step2`, how their order is made
  canonical, and what causal or explicit barrier fixes membership in each drain;
- where one-resource request assumptions are asserted or used for
  classification;
- how polling retries, external IO, signal wakeups, virtual time, record/replay,
  teardown, and backend-specific continuation paths interact;
- which invariants have positive and negative test coverage.

The vision documents establish what a local repair must not narrow: real,
multithreaded, signal-heavy Linux programs; strict deterministic execution;
faithful record/replay; schedule exploration; continuous fine-grained virtual
time; and parity across in-scope backends. A dated count or milestone status is
not a product requirement.

Prior design notes, issue descriptions, summaries, comments near one function,
and a reproducer that matches one native run are supporting evidence only. None
replaces the sources above.

## Establish the review target

Record the repository, base revision, exact proposal or head revision, affected
backends, intended guest-visible behavior, and claimed validation. Identify the
complete event path from the guest operation through Reverie and Detcore to the
scheduler request, blocked state, wakeup, response, syscall completion, and
record/replay representation. Read every production and test file on that path.

Write down before evaluating the proposed mechanism:

- the guest-visible inputs that are allowed to affect the result;
- the deterministic point at which the event becomes eligible to affect the
  schedule;
- every state the target thread can occupy when the event arrives;
- the resource request before and after the transition;
- the run-queue and blocked-pool membership before and after it;
- the effect on scheduler turns and continuous virtual time;
- the behavior required from every in-scope backend.

## Axis 1: soundness of the determinism guarantee

Require an argument over all relevant event orders, not only the reported
reproducer.

1. Identify every host-timed input: signal arrival, RPC and lock acquisition,
   kernel readiness, physical exit, external IO completion, and backend callback
   order. For each one, show where it stops influencing guest-visible ordering.
2. Require both canonical order and deterministic membership. Sorting events
   already present at a drain does not help if host timing decides which events
   reach that drain. Look for a guest-causal handoff or an explicit scheduler
   barrier that fixes membership.
3. Enumerate target states, including running, tentatively selected, queued with
   a filled request, polling, each blocked pool, executing external IO, exiting,
   and already signaled. For every state, require exactly one transition or an
   explicit justified no-op.
4. Trace request replacement and merging. If a change creates a request with
   more than one resource, audit every consumer and assertion, including turn
   classification, external-continuation readiness, signal extraction, logging,
   replay, and cleanup. Do not accept a local removal of the one-resource check
   as proof that combinations have defined semantics.
5. Check for lost, duplicated, or reordered events across the interval between
   physical occurrence and scheduler commitment. Include repeated signals and a
   target that changes state before the deterministic drain.
6. Check that skipped turns remain non-committing, committed turns advance the
   right state once, and polling or wakeup changes do not make virtual time
   host-timing-dependent. Reject fixes that obtain parity by rounding, freezing,
   resetting, or otherwise reducing continuous fine-grained virtual time.
7. Check liveness as well as repeatability. Fairness claims must still hold when
   the queue contains only pollers, when ordinary guest work remains runnable,
   and when external completion or a signal is the only possible source of
   progress.
8. Check record/replay and every in-scope backend separately. A deterministic
   ptrace result does not establish behavior for an asynchronous backend, and
   log filtering must not hide guest-visible divergence.

## Axis 2: Linux and POSIX semantics

Use the relevant Linux man pages, POSIX text, and kernel behavior as primary
references. Distinguish what POSIX requires, what Linux specifies, and what is
merely one observed native schedule.

For signals and blocking operations, answer at least these questions:

- Is the signal process-directed or thread-directed, and is target selection
  faithful to the applicable API and signal mask?
- Is the signal ignored, blocked, caught, or subject to its default action at
  the point the proposal wakes the target?
- Are standard signals coalesced and real-time signals queued as Linux requires?
- If an IO operation has already transferred data, does the guest observe the
  partial byte count rather than an invented `EINTR`?
- If no data was transferred, does interruption produce the correct `EINTR`,
  handler execution, or restart behavior under `SA_RESTART` and the syscall's
  Linux restart rules?
- Does the design preserve kernel atomicity around temporary signal masks and
  wait operations?
- Can the same physical signal be delivered twice, consumed without delivery,
  or left pending after the scheduler reports it handled?
- What happens if the target exits, changes its mask or disposition, completes
  the syscall, or moves between scheduler states before the wake is committed?

Reject a design that makes the reproducer resemble one native run while changing
allowed return values, partial effects, target selection, signal multiplicity,
mask behavior, handler ordering, or restart behavior.

## Evidence required

Require tests at the lowest useful layer and an end-to-end guest test. Tests must
cover the state transitions and timing boundaries identified above, with both a
case that must wake and a nearby case that must not. For an asynchronous event,
exercise arrival before and after request publication, before and after queue
selection, while blocked outside the queue, and after completion or exit where
those states are reachable.

Run repeated strict verification at the required assurance level and name the
backend, exact command, exact revision, log level, and relaxations. A native run
is useful for Linux behavior comparison but is not determinism evidence. A unit
test that directly constructs an otherwise unreachable scheduler state is not
end-to-end evidence. Inspect test, assertion, comparator, allowlist, skip, and
logging changes for a lowered bar.

For virtual time, compare repeated reads across threads and exec boundaries and,
for backend parity, compare the full trajectory rather than one initial value.

## Verdict

Report findings first, with file and line evidence, impact, and the change needed
to resolve each issue. Then state separate conclusions for:

- determinism guarantee;
- Linux and POSIX semantics;
- evidence and goalpost-moving;
- residual risks and untested backends or states.

Approve only when both review axes are supported by the implementation and by
tests at the exact reviewed head. If either axis is unresolved, request changes
even when the original reproducer passes.
