---
name: post-facto-review
description: "Land exact-head validated changes before human review while enforcing dual Claude+Codex adversarial review for post-facto-human-review changes."
---

# Post-Facto-Review Mode

## PR Comment Convention

Every PR description and comment created under this workflow MUST start with
the applicable role tag:

- `[impl agent, MODEL]` for implementation agents
- `[adversarial-reviewer agent, MODEL]` for review agents
- `[coordinator, MODEL]` for coordinator agents
- `[Human]` for the human owner

This is the **canonical and currently active** landing discipline. Changes land
as soon as required adversarial review is resolved and the repository's
authoritative exact-head validation verifier is green. Human review happens
*after* landing, and corrections fix
forward. The old pre-land protocol is retained only as an
[archived historical document](../../archived_skills/human-review-first/SKILL.md);
never activate it or apply a pre-land label in the current mode.

## 1. Exact human-review trigger set

Apply `post-facto-human-review` automatically if the PR contains at least one
of these four triggers. Do not omit or remove the label to avoid the elevated
review gate:

1. **New syscall support.** Verify that the in-code determinization audit tags
   are present: `AUTONOMOUS-BOT-IMPLEMENTED` at the new dispatch/classification
   entry and `TODO-HUMAN-REVIEW(PR-id)` at the implementation or determinization
   block.
2. **A Reverie API or core-abstraction change**, including the `Tool`, `Guest`,
   `Backend`, or syscall-interception model.
3. **A new determinization strategy**, rather than a routine implementation of
   an already established strategy.
4. **A core DetCore scheduling change**: anything that affects how programs are
   scheduled, preempted, blocked, awakened, or explored during race search.
   This trigger is always labeled.
   [Hermit PR #1151](https://github.com/rrnewton/hermit/pull/1151), which moved
   slowdown into virtual-time/epoch scheduling, is the canonical good example
   of both this trigger and the determinism rationale reviewers need.

Routine backend-parity work toward the golden ptrace reference implementation
does **not** trigger human review merely because it changes KVM, DBT, SaBRe,
LiteInst, or another non-ptrace backend. Apply the label only when that work also
meets one of the four triggers above; "backend parity change" is not a valid
review rationale by itself.

This technical review is a pre-land quality gate. Human-owner review is not:
the owner still reviews the already-landed change after the fact.

## 2. Dual adversarial-review gate and evidence

Every PR carrying `post-facto-human-review` requires two independent
adversarial reviews before landing: one by a Claude-family reviewer and one by
a Codex-family reviewer. Neither reviewer may be the author; when the author
uses one of those families, use a separate reviewer instance from that family.
Reviewers try to refute correctness, determinism, Linux semantics, the
Reverie/Detcore boundary, security, and the validation claims at the exact PR
head.

Bind both reviews to the exact head SHA in role-tagged comments. Review labels
are caches for routing and activity, not the approval authority:

- `adversarial-review-codexN` and `adversarial-review-claudeN` record review
  round `N`; they do not mean approval.
- `passed-review-codex` and `passed-review-claude` may cache the corresponding
  exact-head approvals. The role-tagged comments and SHA remain authoritative.

Do not land until both model-family reviewers have approved the exact current
head and every finding is resolved. A head change invalidates both approvals:
run both reviewers again and refresh cache labels only after fresh exact-head
approval. Changes-requested findings likewise invalidate the affected approval.

### Closing a PR is a reviewer action, and it has preconditions

A review can require changes. **A review cannot delete work.** Close a PR only
through the registered wrapper, never with raw `gh pr close`:

```
./.orc/plugins/hermit-dev/gh-pr-close --repo rrnewton/hermit <pr> --comment "<body>"
```

It refuses a close that violates any of the three rules below, and it
dereferences the successor rather than trusting the marker — a
`SUPERSEDED-BY:` naming a PR that does not resolve in the same repository is a
refusal, not a pass. Both directions are bracketed by
`.orc/plugins/hermit-dev/gh-pr-close-test.sh`.

1. **No close without a named successor.** A PR is either **updated in place**
   or its closing comment names the replacement as `SUPERSEDED-BY: #<n>`, and
   that successor must already exist when the predecessor closes. The only
   other disposition is `OWNER-AUTHORIZED-CLOSE: <quote the owner instruction>`.
   Updating in place is the default and the preferred outcome; replacement is
   the exception.
2. **A rejection must name the concrete change required.** "Procedurally
   deficient", "does not meet our standard", and "out of scope" are not
   reviews. A reviewer who cannot state the specific change is not ready to
   reject: leave the finding as a review comment and let the author update the
   PR. Rejection is not a disposition a close can carry.
3. **A taskgraph task is not a substitute for a PR.** "I filed a task to track
   the rework" never authorizes a close. The owner's figure for the cohort he
   examined on 2026-08-07: **106 tasks tagged `implemented` → 38 landed → 4
   goal-met.** Independently reproducible today over the whole local graph:
   `tg sql "SELECT COUNT(*) FROM tasks WHERE tags LIKE '%\"implemented\"%'"` =
   **1734 of 5246 tasks**, of which **1665 are already CLOSED** — so the
   `implemented` tag is applied at scale and closure does not establish that
   code landed. A task is an intention; a PR is code, and that gap is the whole
   reason this rule exists.

Origin: [hermit#1635](https://github.com/rrnewton/hermit/pull/1635), the
validate Rust port and a key critical path, was closed by an adversarial
reviewer with a generic procedural rejection and no successor. The owner:
"You cannot just kill a PR without it being replaced by a better
implementation, and it generally should just be UPDATED, not ever replaced."

## 3. Mandatory PR description sections

Every PR description must contain:

- **Summary**.
- **Determinism** — mandatory for every PR; explain why the change is
  deterministic and give the logic or informal proof, not only test results.
  For time-related changes, prove that time remains continuous and
  fine-grained across deterministic execution; a deterministic first sample is
  not a clock model.
- **Linux Semantics** — mandatory for every PR. State the relevant Linux
  kernel/userspace contract, how the implementation preserves it, and every
  intentional deviation. For a non-semantic change, say explicitly why Linux
  behavior is unchanged.
- **Validation** — exact commands, outcomes, limitations, and relaxations. For
  time-related changes, validate continuous evolution across multiple reads,
  deterministic work/events, timers or waits, and relevant fork/exec process
  trees; do not validate only the first observed timestamp.
- **Relationship to gVisor** — required for KVM changes; state the relevant
  comparison or explicitly explain why none applies.
- **Human Review Required** — mandatory whenever
  `post-facto-human-review` is applied. Name the specific numbered trigger(s)
  above; vague prose such as "backend change" is insufficient.

PR #1151 is the canonical good example for trigger 4: its slowdown model is
explained as weighted virtual-time progression with deterministic epochs and
replay evidence, rather than asserted from passing tests alone. New PRs must use
the section names above and identify trigger 4 explicitly.

## 4. Continuous virtual time is sacred

Treat any weakening of continuous, fine-grained guest virtual time as a
landing red flag. Virtual time must remain a coherent process-tree clock whose
evolution follows deterministic guest progress and preserves the promised
Linux clock, timer, timeout, and ordering semantics.

Stop the landing and require the adversarial reviewers to resolve the design
if an implementation:

- special-cases or fabricates only the first clock read;
- pins time to a first-read epoch, resets or reseeds it per syscall or process,
  or gives related processes incoherent clocks;
- advances time only when it is observed, or derives guest-visible progress
  from host wall time;
- makes a demo's first timestamp pass while later reads, timers, sleeps,
  polling deadlines, fork, or exec do not continuously evolve under the same
  model; or
- cites a single sample as validation for a continuously evolving clock.

An immediate pair of reads need not differ when no deterministic progress
occurred. The invariant is coherent fine-grained evolution across meaningful
guest work and events, not artificial movement on every read.

## 5. Labels

- `post-facto-human-review` is the **single** routing label for a PR awaiting
  the human's after-the-fact review. Apply it automatically for the four
  triggers above. It never waits for pre-land human approval, but it does
  activate the elevated adversarial-review gate in section 2.
- Numbered `adversarial-review-{codex,claude}N` labels are activity caches.
  `passed-review-{codex,claude}` labels are also caches; exact-head role-tagged
  review comments from both required model families are the authority.
- `pre-land-human-review` remains defined only as a notional opposite. **Never
  apply it** while the canonical post-facto protocol is active.
- **Never apply, remove, or otherwise alter `human-approved`.** It records an
  actual human approval and is owner-only.
- The obsolete `human-review` and `post-facto-review` labels must not be
  recreated or applied.
- `locally-validated` may be derived only by the semantic verifier from a
  clean, counted, full-profile receipt for the exact current head. It is a
  cache hint, never landing authority by itself.

## 6. New-syscall code markers

New syscall support authored by a bot must carry both narrowly scoped
determinization breadcrumbs:

- `// AUTONOMOUS-BOT-IMPLEMENTED` at the new dispatch/classification entry.
- `// TODO-HUMAN-REVIEW(PR-id)` at the implementation or determinization block.

Verify both markers before labeling or landing trigger 1. They are not blanket
markers for bot-authored code, backend changes, or routine parity fixes. Use
them only at the smallest new-syscall regions, not across untouched code.

## 7. Land when review and exact-head validation are green

Once required adversarial review is resolved and the authoritative gate is
green, land the authorized change without waiting for a human.

- Hermit accepts either exact-head authority: `ci-hub validate-status` for the
  clean, counted local receipt, or `ci-hub hosted-status` for the versioned
  GitHub job policy. The parent policy requires the portable `Regular tests
  (GitHub-managed portable)` job, not an implicit portable+privileged 2/2.
  Missing, partial, stale, cancelled, and skipped evidence is NO_RESULT; a
  genuine red from either authority blocks even if the peer is green.
  This is not end-to-end deployed until
  [`hermit-merge-gate-authority-deployment`](../../../ci-hub/landing/README.md#deployment-obligation-hermit-merge-gate-authority-deployment)
  lands in Hermit; meanwhile its required merge-gate still requires
  portable+privileged and pins the older verifier. Obey that gate.
- Reverie requires its repository-defined exact-head validation authority.
- When one of the four triggers applies, add `post-facto-human-review`, verify
  every required PR section, verify both exact-head reviews, post role-tagged
  evidence, land through `ci-hub/landing/land-pr.sh` (which re-dereferences the
  local-or-hosted policy at the final exact head) using the repository's
  current merge mode, record the exact landed SHA, and rebase dependents in
  dependency order. Do not add the label to a routine non-triggering PR.

## 8. Human reviews after landing

The human reviews landed work after the fact. Corrections use follow-up PRs and
fix forward. Remove a `TODO-HUMAN-REVIEW` marker only when its concern is
addressed.

## No pre-land mode

There is no active pre-land-human-review mode. The archived pre-land skill is
historical context, not an activation path. A future policy change requires a
new owner directive and an update to this canonical skill first.
