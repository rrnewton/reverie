# AGENTS.md Policy Rationale (read on demand)

Companion to the parent `AGENTS.md`/`CLAUDE.md`. That file carries the **executable predicates**;
this file carries the **rationale, worked examples, and reference tables** an agent reads only when it
needs the *why* behind a rule. Splitting them keeps the spawn-time policy file under its 40,000-char
limit (every agent pays that file at spawn; this one is read on demand). When a predicate in `AGENTS.md`
points here, this is the long form.

Current Hermit CI mode, validation-evidence qualification, hard/soft-green
landability, and the local-versus-GitHub boundary live only in skill
`hermit-validation-authority`. This rationale retains the general Proxy Binding principle
and examples; it does not independently set validation policy.

---

## Proxy Binding Review Axis — full rationale

**Proxy Binding** is the mandatory adversarial-review axis: **what binds this check to the fact it claims,
and can I observe that binding rather than infer it?** Authenticating **who** emitted evidence proves
origin, not causation. A predicate such as `marker-present && mismatch-present` merely ANDs two independent
facts. Causal binding requires evidence that can exist only when the claimed condition caused the reported
outcome, such as a typed first-cause result.

**One verifier per authority, called by every consumer.** An **evidence authority** is the source whose
contents can make a load-bearing claim true; a label, comment, status, or copied field is only a cache or
reference to that source. Give each authority one semantic verifier that dereferences the source, validates
the qualified value, and is called by every gate, labeler, lander, status view, and closure path that acts on
the claim. Do not collapse different authorities behind one generic-looking semantic check: a validation
receipt and a Git commit require different proofs. They may share transport/digest primitives, not truth
conditions. A verifier is not deployed merely because it exists. Mark an authority covered only after (1) a
counted qualifying positive passes, (2) a well-shaped but nonexistent/tampered/otherwise nonqualifying
negative is refused, and (3) a call-site audit shows that every consumer invokes the verifier rather than
reimplementing or bypassing it.

### Load-Bearing Authority Registry

This registry covers authorities that currently decide validation, review, landing, dependency currency, or
workflow-policy outcomes. Add a row before introducing another load-bearing authority or consumer; update a
row only from exact code and tests, never from a PR description.

| Authority and qualified value | Canonical dereferencing verifier | Coverage and remaining hole |
| --- | --- | --- |
| **Local validation ledger:** hard-green evidence and its exact Hermit SHA. Policy selection and soft-green composition are defined only by skill `hermit-validation-authority`. | `ci-hub validate-status` / `ci-hub/lib/validate_status.rs`; the lander calls it through `ci-hub/landing/local-validation-eligibility.sh`. | **COVERED on canonical producer and label paths.** Hermit's Rust driver appends the ledger row first, then delegates label publication to `ci-hub apply-local-label --ledger`; that consumer re-derives the receipt from the ledger. Labels remain caches, not ledger evidence. |
| **Published validation receipt:** immutable receipt commit and path on the receipt branch, branch ancestry, recomputed receipt and log digests, and the validation identity selected by `hermit-validation-authority`. | Producer: `ci-hub/validation/publish_receipt.py`. Consumer: the parent-pinned `ci-hub/validation/verify_receipt.sh`, never PR-controlled verifier code. | **PARTIAL.** The canonical `land-pr.sh` dereferences the final pushed head immediately before merge; its fixture brackets legitimate and malformed receipts. The Hermit merge gate and landing planner still accept a bare `locally-validated` label, and #1593 adds another shape-only parser. Replace every such consumer with the parent verifier. |
| **Historical Git provenance:** the claimed commit exists in the named repository and the measurement/artifact is bound to that commit, not merely written beside a 40-hex string. | **None.** The minimum identity primitive is a fresh fetch plus `git cat-file -e <sha>^{commit}` in the intended repository; measurement causation additionally needs a commit-bound artifact/receipt. | **MISSING.** Hermit #1546 validates provenance fields and full-SHA syntax without dereferencing the object or a measurement artifact. Commit existence alone proves identity, not that the measurement came from it. |
| **Adversarial/human review:** reviewer lane, verdict, PR number, and exact reviewed head in a durable receipt produced by that lane. | **None.** `scripts/core-review-protocol-lint.sh` checks cache labels only. | **MISSING.** Push-time label invalidation narrows staleness but does not prove who reviewed what; `passed-review-*`, numbered review labels, and `human-approved` are assertions until a lane-specific exact-head receipt is dereferenced. |
| **Landing authorization:** a durable coordinator/owner decision names the exact repository, PR, and head allowed to land, with no unresolved hold. | **None.** `land-pr.sh` accepts command-line PR/branch arguments and does not dereference a task authorization record. | **MISSING.** Agent assignment, a ready label, or invoking the lander is not authorization. A future verifier must consume an exact-head authorization receipt; keep this human/coordinator gate explicit until then. |
| **Landed PR identity and task closure:** GitHub's `mergeCommit.oid` is reachable from a freshly fetched target branch. | `ci-hub verify-landing`; `ci-hub/closure/verified_close.py` calls it. | **PARTIAL.** `ci-hub/landing/land-pr.sh` reimplements the proof. Route it through `verify-landing`; never substitute PR-head ancestry, `MERGED`, or `mergeStateStatus`. |
| **GH Actions result:** hosted workflow/job evidence; current validation-authority selection is defined only by skill `hermit-validation-authority`. | Exact-head GitHub API run/job lookup plus the canonical result classifier in `ci-hub/check_outcome.py`. | **INFRASTRUCTURE PRESENT; POLICY EXTERNAL.** Health/history readers classify observational and historical records; whether a result has current validation authority is selected only by skill `hermit-validation-authority`. |
| **Workflow-policy version:** the required context was emitted by the trusted current workflow definition, not an older PR-branch YAML with weaker rules. | Hermit #1579's versioned context/blob check and ruleset reconciler. | **MISSING on main.** Until the versioned gate lands and every required-context consumer switches, stale branch YAML can emit a current-looking green. |
| **Live dependency currency:** every tracked manifest and lockfile pin equals the freshly resolved canonical remote ref. | `scripts/check-reverie-pin.rs` from Hermit #1591: live `git ls-remote`, tracked `Cargo.toml` + `Cargo.lock` scan, exact equality. | **PARTIAL until #1591 lands and all paths call it.** The reviewed verifier has an exact-tip positive and real ancestor-behind negative; local validate, both DAGs, hosted aggregate, merge gate, and receipt production are the required consumers. |
| **Workspace ownership:** `worktree-state.json`, the managed `ACTIVE.md` row, registered worktrees, and actual checked-out branches/detached SHAs agree. | `scripts/check-worktree-registry.rs`; its fixture accepts 2/2 correct rows and refuses planted content drift while preserving a 1/1 correct control. | **PARTIAL.** `allocate-worktree.rs --check-only` calls it, but normal allocation, `release-worktree.rs`, and `worktree-gc.sh` do not all run the verifier before acting. They must call it rather than trusting or independently parsing one surface. |
| **Mechanism overlap:** every open PR changing the same load-bearing mechanism is in the coordinator's exact-head review set, including mechanisms an author failed to tag. | **None for semantic completeness.** `ci-hub pr-status` groups declared `mechanism:<slug>` labels; it cannot prove that the declarations cover the code's actual mechanisms or that two intentions are compatible. | **MISSING; human semantic authority required.** The label grouper is a discovery aid, not a verifier. Record coordinator review against the exact PR/head set, and retain file/symbol review for untagged overlap; do not describe label absence as proof of independence. |
| **Merge conflicts and final mergeability:** fetched PR/base objects are the objects analyzed, pairwise conflicts come from real Git merges, and the final merge command succeeds against the current target. | Planning: `scripts/pr_conflict_graph.py` fetches base/head refs, checks API-head equality, and runs `git merge-tree`. Landing: the actual `gh pr merge --rebase` command is the final arbiter. | **COVERED on canonical planning/landing paths.** `mergeable` and `mergeStateStatus` remain hints only. New planners must call the real merge-tree path; new landers must not promote those hints to authority. |

The generative cure is: **carry the condition with the value.** A value measured under conditions it does
not record is a proxy, whether that value is a string, flag, status, hash, or number. Store `{ jobs: 32,
bytes: N }`, not a bare memory cap `N`; bind landing to `mergeCommit.oid` ancestry on freshly fetched main,
not a PR head or `MERGED` flag. A bare
value and a qualified value often read identically as facts, so inspection cannot reveal that the
qualification is missing. Reviewers must ask what conditions made the value true, whether those conditions
travel with it, and whether they are still current at the decision point.

Validation-specific receipt fields, coverage rules, exact-SHA evidence
identity, and hard/soft-green landability live only in skill
`hermit-validation-authority`. Their presence here would create a second policy copy.

Verification must bracket guarded behavior from both sides. **Negative:** plant the violating case and
confirm **refusal** (proves the mechanism is not permissive). **Positive:** plant the genuine qualifying
case and confirm it **fires** (proves the mechanism is not inert). Neither alone is verification: a guard
that refuses everything passes every negative test. State the counts on both sides. PR #1468 is the model:
9 cells / 18 executions remained eligible with zero fallback and zero trusted-native sites, while the
`random-device` negative was rejected with 66 trusted-native sites.

Do **not** plant an artifact that is itself an authorization. Hand-adding a merge/review/validation label,
dispatching a workflow that can auto-merge, or arming another live gate tests by creating the hazard. Exercise
the consumer with an inert fixture, a dry-run/read-only mode, or an isolated test repository; the negative
control must be incapable of authorizing the action whose refusal it tests.

A check fails when it keys on a correlated proxy without an observable identity, causal, coverage, or
provenance link to the claimed condition. Reviewers name the claimed fact, the observed evidence, the
conditions under which it was measured, and the binding between them; passing tests do not supply a missing
binding. The current twelve worked examples are:

1. A `locally-validated` label with no exact-head ledger record: the label is a cache, not the source of truth.
2. A merge gate that authorizes on bare label presence without reading the ledger.
3. `workflow_dispatch` running the PR branch's older YAML, allowing a weaker historical gate to emit the same green.
4. `is-ancestor <PR head>` encoding a merge-commit model under rebase-merge, where the PR head is never ancestral to replayed main (it undercounted landings by 33); use `mergeCommit.oid` after a fresh fetch.
5. A pin checker walking `Cargo.toml` but not tracked `Cargo.lock`, reporting consistency over an incomplete file set.
6. A green result with no executed count: success is not bound to any work having run.
7. `filtered == 0` used as completeness although 693 filters are legitimate and an incomplete discovery set can also report zero.
8. A `parity%` derived from piped-stdout SHA-256 but presented as full INFO + detlog-stack + detlog-heap parity.
9. `ACTIVE.md` naming a branch the slot does not hold, while reconciliation passes by comparing row counts rather than row contents.
10. `--cgroups` accepted by a CLI but producing no cgroup behavior or typed acknowledgement.
11. A cancelled run classified as red: a no-result is rendered as a result.
12. Dispatch boilerplate listing `commit` as destructive, causing agents to withhold the durable handoff the protocol requires.

Earlier marker-substring, error-string, partial-backend, rendered-SIGPIPE, and unqualified-memory-cap cases are
the same class. In each case ask: **what binds this signal to the fact it claims, and can I observe that
binding rather than infer it?**

### Mechanical enforcement is deliberately split by layer

- **Source/config lint:** reject representations whose missing qualification is syntactically observable.
  Of the twelve examples above, only **3/12** are source/config-lintable without pretending to understand
  runtime semantics: #2 can forbid a label-presence authorization branch, #4 can forbid PR-head ancestry at
  the typed landed-identity boundary, and #12 can reject `commit` in the destructive-operation list of the
  dispatch template that owns it. The existing Rust error-string proxy lint covers **0/12** of this new
  catalogue; it correctly covers an earlier syntactic instance and must not claim more. These checks prove
  only that a known bad representation is absent, not that the replacement binding is truthful or sufficient.
- **Runtime/result checks:** require one ledger record carrying run ID, exact SHA, durable log, profile,
  discovered/selected/executed/filtered counts, failures, and declared coverage obligations; mechanically
  reject full green unless the profile is full, execution is nonzero, coverage obligations are satisfied,
  and failures are zero. This layer catches #1, #3, #6, #9, #10, and #11 with ledger/provenance/content/
  behavioral/classifier checks. Require `mergeCommit.oid` ancestry after a fresh fetch behind landed. A
  planted stale `Cargo.lock` fixture can regression-test the known half of #5, but it cannot prove that every
  future relevant file is in the checker's universe. These are evidence validators and contract tests, not
  source lint.
- **Semantic review:** determine whether a marker is causally bound, a file/backend/gate registry is
  complete (#5), coverage obligations actually define the intended suite (#7), and a parity artifact covers
  the full claimed trace (#8). It must also establish workflow/registry freshness, behavioral currency, and
  causal validity even where a mechanical detector exists. Perfect counts over an incomplete discovery set
  remain a proxy. No general lint can infer these facts. Do not stretch a syntactic lint to claim coverage of
  them; a lint claiming all twelve would itself fail Proxy Binding.

---

## Establish What You Have Before Acting On It — full rationale

A **coordinator** rule for how an observation becomes filed work or a reported conclusion. Both failure modes
below are the same mistake — acting on a claim or a quantity before establishing what it actually is.
Verifying first costs minutes; acting on the wrong thing costs the implementation and the rollback.

**A note is unverified until the coordinator checks it.** A note is one agent's point-in-time belief, not
established fact. Do not launder a note into a task premise by rewriting "X appears to be Y" into "X is Y,
fix it." When a premise originates from a note (or any second-hand observation) rather than the
coordinator's own direct verification, the task description must **attribute the premise to its source**,
**mark it UNVERIFIED** in those words, and **make "verify the premise" the explicit first step, with
"premise refuted" a valid, valuable outcome** — the refutation is a deliverable. A correctly-hedged
observation ("code-inferred, unmeasured") becomes wrong only when the coordinator drops the hedge. (Full
list: note `task-premises-from-notes-must-be-marked-unverified`.)

**A number is unqualified until the coordinator states what it measures.** A number can be arithmetically
correct yet measure the wrong thing; the trap is reaching for the first available quantity, usually a
**proxy**. Before acting, establish **what it measures** (the quantity the decision needs, or a proxy?), its
**unit** (a count is not a rate; an aggregate is not a per-unit; a load average is not a utilisation; a
source tree is not a shipping artifact), and its **denominator/comparison base**. When a ratio looks
surprising, interrogate the denominator before filing work against the numerator.

### Hermit landing hierarchy — evidence, authorization, enforcement

**Follow skill `hermit-validation-authority` for validation authority.**

Parent `AGENTS.md` is the fleet publication authority because it owns checkout,
review, landing, and direct-main permissions. GitHub rulesets enforce deletion,
non-fast-forward, and linear-history safety. A mechanism that can push and a
server that accepts a push do not authorize an agent to make it.

This is the same most-recent-wins rule that already reversed the DBT rename deferral, lifted the temporary
do-not-merge default, and replaced Reverie-pin tip equality with ancestry plus monotonicity. Keeping an older
sentence because it is versioned would reintroduce policies the owner explicitly superseded.

Normal Hermit publication remains a reviewed feature PR through the tracked
lander. Validation green is evidence, not free-standing permission to push
`main`. Direct-main authority exists only in the named Red-Tip Direct Land and
Gate-Definition Rotation scopes.

The owner also settled review routing on 2026-08-10, verbatim:

> FOR SINGLE-REVIEW PRs CROSS-AGENT REVIEW IS PREFERRED (NOT MANDATORY, PENDING API OUTAGES ETC ETC). FOR CRITICAL CHANGES AS I DEFINED THEM DOUBLE-REVIEW PRs IT'S GOOD TO HAVE TWO DIFFERENT AGENTS (CLAUDE, CODEX, OR METAMATE IF NEEDED).

Critical here means Hermit syscall handling, DetCore scheduling, or a Reverie API change. This supersedes the
temporary Codex-only direction, whose reason was model rate limiting and whose owner-confirmed lifetime was 12
hours. Preserving that direction as history is correct; continuing to dispatch by it is not. More generally, a
temporary directive records why it exists and when it expires or must be reviewed, so the reason cannot disappear
while the restriction silently becomes permanent.

### Neutral agent ids and task trailers

An agent id is a durable join key, not a job description. Agents are routinely reassigned, so a name such as
`codex-hostload` becomes a false historical claim as soon as that process takes different work. New general
agents therefore use purpose-free sequential ids (`codex-1`, `claude-1`, and so on). Existing live agents are
not renamed because their current ids already join processes, TaskGraph notes, and commits; this is a
forward-only convention.

Commits still need actor attribution, so the final role/agent/machine line remains. Purpose moves to repeatable
`Task: <stable-task-id>` trailers immediately above it. Repeated task trailers cover the rare explicitly folded
commit without forcing either the stable agent id or stable task id to pretend to be the other.

Remediation cannot promote a validation verdict into permission to mutate
`main`. A failed or unknown validation result is loud and actionable but does
not authorize an automatic revert. A revert requires a durable owner-authored
authorization bound to the exact candidate revert SHA, current-main SHA, and
failed-land identity. Parent `AGENTS.md` owns fleet publication and remediation
permission. The older hosted-red remediation prose in `ci-hub/README.md` remained nonconforming after
`ci-hub/remediation/protocol.py` was corrected at `53c2bedec401`; this revision reconciles the prose rather than
deleting its history.

For gate changes, prefer **versioned overlap**: add a new immutable workflow/context while the registered old
version remains byte-identical, let the old version judge the bootstrap PR, register and observe the new version
from `main`, then retire the old version later. Same-file direct rotation is the narrow fallback. Its consumer
refuses nine classes: (1) absent/malformed/expired owner record; (2) repo/PR mismatch; (3) moved base or non-FF
head; (4) stale PR/checkout head; (5) old/new workflow-blob mismatch; (6) changed-path mismatch; (7) missing,
stale, or malformed dual approval or a current rejection; (8) absent exact-head counted local PASS; and (9)
anything other than the expected registered-definition-only self-refusal. The owner record binds repo, PR,
base, head, old/new blobs, complete paths, reason, expiry, and signer. The consumer re-reads under `land-lock`,
then composes through independently verifying normal non-admin
`land-pr.sh --gate-rotation --no-rebase`, whose mutation remains
`gh pr merge --rebase --match-head-commit`. There is no second mutation entry point or raw main refspec. The
load-bearing sequence is exactly `collect_outer -> verify_outer -> enter_land_lock -> verify_final ->
normal_non_admin_merge -> refresh_main -> prove_ancestry -> read_main_gate_blob -> patch_registration ->
verify_registration`. Refresh and ancestry proof between merge and registration make "register only after the
push" mechanical rather than remembered. Registration failure is visible and leaves the old gate fail-closed;
register-only recovery repeats authorization, ancestry, and main-blob checks without merging again. This is the
single supported client composition, not server-side enforcement: live rulesets still admit raw fast-forwards,
so discipline outside it remains convention unless the owner adds a repository-boundary control.

The Hermit `docs/MERGE_QUEUE.md` sentence saying an exact-head green branch may
direct-fast-forward remains mechanism history, not current authorization.
Current validation and hosted-path decisions live only in skill
`hermit-validation-authority`; direct-main permission remains limited by parent policy.

### Red-Tip Direct Land — worked example

The **Red-Tip Direct Land** carve-out (AGENTS.md, *Hermit Git And Pull Request Workflow*) is the sharpest case
of this rule, because the premise arrives inside the authorization itself. The dispatch says "the tip is red,
land directly" — and that is a claim. The carve-out waives PR review, so an invocation on a false premise
pushes an unreviewed commit straight to `main` for a red that does not exist. Establishing the red first is
what keeps the exception safe to grant at all.

**It worked, on 2026-08-08.** Agent `main-green` was authorized mid-task to land a `test.liteinst_strict` fix
directly to `rrnewton/hermit:main`, urgently, on the stated premise that the tip was red and its fix was the
single thing on the critical path. It declined, and was right to, on four separate grounds:

1. **The premise was measurably false.** An admitted clean-tree commit-anchored portable run at tip
   `f65f74462931` gave 47/47 nodes, exit 0, with `test.liteinst_strict` PASS 23/23. The tip was GREEN.
2. **There was no red-tip defect to fix.** Both commits in the range were provably inert (35 lines of workflow
   YAML; 6 comment lines plus one lint attribute), and the identical 900s hang was on record days before
   either commit existed.
3. **The authorization was scoped, and what it found fell outside the scope.** The grant covered "the fix for
   the red tip and nothing else… if you find a second unrelated problem, file it, do not fold it in." What it
   actually found *was* that second thing — a pre-existing ~1.8%/run flaky hang plus a diagnosability gap.
   Landing that direct-to-main would have been exactly the drive-by the ruling excludes.
4. **The change would not have un-redded anything.** It makes a flake fail fast with evidence instead of
   silently eating a 900s budget; when the flake next strikes the node is still red, just red in seconds.

It then offered to push immediately if the owner still wanted it knowing all four points — "a correction of
the premise, not a refusal." That is the disposition the exception depends on: **treat an authorization as
scoped, not as permission-in-general.** An agent that reads a direct-land grant as general license to bypass
review is the failure this carve-out would otherwise invite, and refuting the premise is a deliverable, not
an obstruction.

The counter-case is equally on record and is why the exception exists: `green-baseline` followed the written
policy exactly, rebased onto newest-green as instructed, and hit an admission-containment refusal that made
the whole PR drain structurally impossible during a genuine red-tip window. The escape hatch existed the whole
time and the documentation did not. **An undocumented exception is operationally indistinguishable from a
deadlock** — and an unverified one is indistinguishable from a licence.

---

## Verify A Mechanism By The Running Thing, Not Its Config — full rationale

**Do not verify that a mechanism governs a process by reading its configuration, its flag, or its exit code.
Find the running thing and ask what is actually holding it.** A flag can be a deprecated no-op; an exit code
can come from a different wrapper; both mislead, sometimes in opposite directions, and two layers of
inference can be wrong at once. The direct observation settles in one command what the inference got wrong.

Canonical instance (cgroup boxing): take a live PID and walk the cgroup tree to find which scope contains
it — `find /sys/fs/cgroup/... -name cgroup.procs` and grep for the PID, then print the containing path. A
result like `safe.slice/safe-ci.slice/safe-ci-<n>.scope/step-test.<node>/cgroup.procs` holding the PID
proves per-DAG-node boxing is real and active — where reading a (deprecated) flag and an (unrelated) exit
code had claimed the opposite. The same move generalises: check which commit a validate record is keyed to,
which workflow file a dispatch actually ran, and whether a required label has a signer that exists.

---

## Record Every Measurement Immediately — full rationale

**Before you measure, check whether the number already exists** — search `experiments/`, task notes, and the
ci-hub history store first; re-deriving a known quantity wastes fleet time and risks measuring it differently
than the value on record.

**Any measurement you take goes into a task note immediately — even one taken incidentally.** Numbers
measured in passing don't feel like deliverables, so they die in a pane at the next recycle; that loss is
expensive (a passing max-`cc1plus`-RSS reading was the whole explanation for an OOM blocking every ready PR,
but lived only in a pane). If you measured it, write it down with its units, its context, and **how you
obtained it — sampled versus recorded matters enormously**: a polled aggregate and a cgroup-recorded peak
are different numbers, and mislabelling a sampled aggregate as a peak has refuted true findings (polling
misses spikes by construction). Bind the number per *Establish What You Have*: what it measures, its unit,
its denominator.

---

## Running validate — systemd-run producer path, full rationale

**Pre-anchor preflight.** A Hermit PR head that does **not** descend from hermit `bfb0a9ef` (commit-anchoring,
2026-08-03 18:43 UTC) runs its **own older** `validate.sh`, which emits the anchor fields
(`commit_anchored`/`selection_mode`) **NULL**; the consumer `is_clean_full_pass`
(`ci-hub/lib/validate_status.rs:100-121`) is **fail-closed** on them, so the ~17-minute run is a **guaranteed
rejection even when fully green**. The producer travels with the branch, so this can never be fixed by
anything on main — the pre-anchor head must be **rebased**, not re-validated. `#1591` is anchored (rc=0);
`#1558` is pre-anchor and was caught in a drain claim by this check. Re-derive the live pre-anchor set with
the `is-ancestor` loop; do not trust a stale list. **HOLD mass-rebase** until the version-aware counts
consumer (`reject_missing_or_filtered`) lands — see task
`prs-predating-commit-anchoring-can-never-produce-a-qualifying-receipt`.

**Why systemd-run.** An agent sandbox CANNOT run `scripts/validate.rs` directly: BpfJailer denies a process
**creating its own** cgroup, so the wrapper exits 3 in ~9s having executed nothing. The tell is `CPU/wall
1.0x` on a many-core box — the run never boxed, never ran. Concluding "the PR is broken" from that exit is a
misdiagnosis (it cost hours). The working path launches validate as a transient user unit — the process ASKS
systemd for a scope instead of creating one itself, so it is **still boxed** (the boxing principle holds,
this is not a bypass), and it runs **detached with a durable log** that outlives agent recycling — the
difference between a green *claim* and green *evidence*. Both legs of GitHub-free landing require a validate
record, and until this path nothing could produce one. Let `apply-local-label` add the label FROM the ledger
record — never by hand. It publishes an immutable, remotely readable receipt containing the exact counted
ledger row and log digest before the label; a well-shaped comment that points at no such receipt is not
evidence. Derive the safe concurrency against total cores before fanning out records; do not guess (contended
runs are a recurring bug class). The Hermit Merge Gate must execute `ci-hub/validation/verify_receipt.sh`
from an immutable parent commit, never from the pull request under test — a PR-controlled verifier can
authorize itself even when the workflow YAML is separately pinned.

## Canonical Layout — full directory tree

Predicate in AGENTS.md: every normal worktree path is `worktrees/<slot>/{hermit,reverie,liteinst2}` where
`<slot>` is a named agent (`kvm`, `dbi`, `sabre`, `e9patch`, `liteinst`, `ci`, `coord`, `lander`, `opt` —
`hermit-` prefix stripped) or a generic `slotNN`. Nested layout v3: one slot per agent, exactly one mutating
owner. Old flat layout (`worktrees/slotNN` + sibling `worktrees_reverie/slotNN`) and primary-nested
`hermit/.worktrees/…` scratch trees are deprecated — never create either.

```text
~/work/dev-hermit/
|-- AGENTS.md ; CLAUDE.md -> AGENTS.md
|-- .orc/plugins/hermit-dev/       # coordinator policy plugin
|-- .gitmodules
|-- hermit/ reverie/ liteinst2/    # primaries; coordinator only
|-- agent-utils/                    # optional shared tooling; exact pin, on demand
|-- worktree-state.json             # machine-local slot->owner map (gitignored)
|-- worktrees/
|   |-- ACTIVE.md                   # human notes + script-managed slot table (gitignored)
|   |-- ARCHIVED.md                 # append-only completed-slot history (durable)
|   |-- kvm/{hermit,reverie,liteinst2}/  # named-agent slot; dbi/ slot01/ ... up to 12 active + 5 parked
|-- ai_docs/                        # durable research + handoffs
|-- experiments/                    # durable reproducible evidence
`-- scratch/                        # ignored transient material
```

Physical worktrees, build output, `worktree-state.json`, and `ACTIVE.md` are machine-local and gitignored;
`ARCHIVED.md` is durable. `ai_docs/transient/2026-07-27-worktree-management-map.md` is the authoritative index
of every place worktree information lives — read it before any worktree operation.

## Vocabulary — full glossary

- **Parent**: `~/work/dev-hermit/`, the harness repo holding gitlinks and workspace state.
- **Primary checkout**: `~/work/dev-hermit/{hermit,reverie,liteinst2}/`. Coordinator-owned; integration, pinning, inspection, cache donation.
- **Submodule**: a repo the parent records as an exact gitlink SHA — a commit, not a branch, not uncommitted contents.
- **Slot**: one opaque paired workspace under `worktrees/`. **Active worktree**: assigned to live work and in `ACTIVE.md`. **Parked slot**: clean, detached, retained for cache reuse, omitted from `ACTIVE.md`. **Legacy slot**: pre-policy non-canonical worktree in `ACTIVE.md`; may finish its task but must be removed, not reused.
- **Product worktree**: one nested submodule checkout inside a slot, e.g. `slot02/hermit`.
- **Feature branch**: task-specific branch in one product worktree; slot names are unrelated to branch names.
- **Hermit base**: current `rrnewton/hermit:main`, unless a task names another reviewed base. **Hermit upstream**: `facebookexperimental/hermit`, public reference — not the default landing target.
- **Shared slot**: active slot used by multiple research-only agents, or mutating agents with explicitly disjoint file ownership in `ACTIVE.md`. No two agents may edit the same file concurrently.
- **Handoff SHA**: the exact commit tested and offered for integration. A branch name alone is not evidence.
- **3pai agent sandbox**: agent execution environment identified at runtime by `META_3PAI_*` vars and the `3pai_sandbox.slice` cgroup; file/network policy enforced by BpfJailer.

## Product Vision — full statement

`goal-hermit-v2`: a robust deterministic execution engine whose `run`/`record` modes support arbitrary
real-world binaries, whose chaos mode exposes concurrency races, whose schedule search localizes races to
events and stack traces, whose production backend avoids ptrace overhead, and whose non-communicating processes
execute in parallel. `goal-qemu-linux-under-hermit`: run a complete Linux VM as a userspace QEMU process under
Hermit so deterministic execution, record/replay, chaos scheduling, and schedule search expose and localize
kernel races across the full stack. Prioritize correctness, faithful replay, race discovery/localization,
lower overhead, backend maturity, and QEMU/Linux viability. Do not close either long-range goal without its
required human verification.

## Process-Kill Safety — war story

Hard Invariant 15 exists because a broad `pkill` once killed other agents' hermit runs mid-task, and `pkill -f
"find / -path"` recurred later because a freshly recycled agent did not carry the verbal correction — the rule
does not survive recycling unless it lives in AGENTS.md. Up to eighteen agents share this box and its binary
paths (`hermit`, `cargo`, `python3`, …), so any name/pattern/`-f`-substring/user/`ps|grep|kill` match kills
siblings' live work.

## Task Lifecycle — phantom-closure rationale

**Scope since 2026-09-28: ORC only.** TaskGraph is ORC's short-term to-do list and dispatch memory, not
a project record, and other agents do not use it. Product defects go to GitHub issues, landing evidence
goes in the pull request, and pull-request ownership is the `Owner:` line in the pull-request
description. See `AGENTS.md`, "Task lists, pull-request ownership, and durable records". The rest of
this section explains the tags ORC's own tooling still reads.

Phantom closures (a task marked done while its work never landed) are a recurring failure mode. The 2026-08-08
policy stops fighting them with a gatekeeper — the coordinator-only closure step was unsatisfiable, because the
ORC coordinator has no shell and could never run the gateway, so in practice NOTHING closed and finished work
piled up as `implemented`-but-open. The defence is now the `implemented` tag plus the IMPLEMENTED evidence
note: `closed` deliberately does NOT mean landed, landing debt is enumerated from CLOSED+`implemented` records
by `drain-implemented-to-landed` and `health-tick`, and that debt is discharged only by the `CLOSURE-VERIFIED`
note `./ci-hub/bin/close-task` writes. A phantom closure is now visible as a CLOSED task that carries
`implemented` forever, rather than being prevented by a step nobody could execute. Ordinary agents have no
reliable fleet-wide peer-message channel: the
agent-side `SendMessage` registry is scoped to same-session subagents and cannot resolve fixed/numeric ORC
fleet names (`hermit-lander`, `hermit-247`), so never claim another fleet agent was notified merely because a
message was attempted. TaskGraph notes are pull-based — they preserve a handoff across recycling but do not
wake a recipient and are not delivery acknowledgement.

## Coordinator Checklist (outcomes to require, not commands to run)

Terse preflights recapping rules stated in full in AGENTS.md; each references a section there. The ORC
coordinator has no shell (AGENTS.md **Role Boundary**), so each bullet is an outcome it must have evidence
for — the agent holding the shell runs the commands and reports.

- **Before dispatch:** reconcile `ACTIVE.md` + both Git worktree lists + physical slot children; check parent/primaries/candidate slot for unexpected changes; confirm ≤ 12 active worktrees and ≤ 15 agents; record exclusive ownership or every sharing agent + disjoint path; confirm base SHA + publication target per repo, verifying a handed SHA against `ci-hub newest-green`; register the slot before work; apply *Establish what you have* when a premise came from a note/number.
- **Before Hermit publication or landing:** re-read concurrent local state,
  remote `main`, and the exact PR head; verify handoff SHA, diff, evidence, and
  cleanliness; classify validation under skill `hermit-validation-authority`; merge only
  when authorized; record the resulting `main` SHA and evidence.
- **Before parent pinning:** submodule commits clean, reviewed, tested, fetchable; inspect `git diff --submodule=log` before staging; stage only intended gitlinks + parent-owned files; validate a coordinated Hermit/Reverie pair when both move; commit parent changes to `main` only when the task authorizes it.
- **Before closeout:** each changed repo has a clean committed feature branch; record exact SHAs + validation in the pull request (or a tracked `ai_docs/` file) and `ARCHIVED.md`; detach slot children at their parent-pinned gitlinks; remove/update the slot row; keep ≤ 5 parked; leave unrelated concurrent work exactly as found.
- **Before closing a task (ORC TaskGraph only; the OWNING AGENT closes it — superseded 2026-08-08):** the coordinator-only closure gate is GONE. An agent closes its own task with plain `tg update <id> --status closed` once it has posted the PR/artifact URL, exact SHA, and validation evidence and tagged `implemented`. `./ci-hub/bin/close-task` is retained but optional; it is still the only writer of the `CLOSURE-VERIFIED` note that discharges landing debt, so use it once the change is on `main`. See AGENTS.md **Task Lifecycle And Closure**.

### Manual GH Actions dispatch under `merge-gate-v4` — the measurements

Evidence for the AGENTS.md predicate (Coordinator Judgement Rules). On a NO_RESULT the diagnostic gate may
re-dispatch missing legs and then deliberately cancel itself. Its historical log language about landing is
superseded by the owner's 2026-08-10 validate-first decision. Measured on hermit#1911 at `18758e9a`: a hand-rolled second
dispatch at the same SHA left 30+ jobs succeeding and exactly two — `test: sabre` and
`e2e: c-programs__verify__ptrace` — cancelled, which was enough to flip `Regular tests (GitHub-managed
portable)` to `failure`. Re-running `merge-gate-v4` instead produced `Every required leg has a PASSED result.`
on the first attempt. The one-clean-dispatch repair was verified moving `fda945d7` from `HOSTED RED 0/1` to
`HOSTED GREEN 1/1`.

## Action-time procedures (consulted when performing the action; invariants stay in AGENTS.md)

### Submodule pinning — A/B protocol, procedure, initialization

- **A/B protocol** (in `ci-hub/history/SUBMODULE-BUMPS.md`): clean green parent A → advance one gitlink to fetched `origin/main` → B changing only that gitlink → verify B → append to the ci-hub history store. Never bury a gitlink advance in an unrelated commit. Determinism changes need a powered repeated probe (one run is insufficient). Use `make single-submodule-bump ARGS='plan ...'` before execution.
- **Procedure:** confirm each submodule clean and on the intended SHA; inspect `git diff --submodule=log -- hermit reverie`; stage only pointers intentionally moved (`git add hermit reverie` records only checked-out commits, not uncommitted files); re-inspect `--cached`. If both move together, validate the exact pair and update both gitlinks in one commit with old/new SHAs + compatibility evidence. Confirm referenced commits are fetchable from authorized remotes before sharing the parent commit.
- **Initialization** reproduces recorded commits: `git submodule update --init --checkout -- hermit` and the Reverie form with `-c submodule.reverie.update=checkout` (override only when init is intended and `.gitmodules` marks it `update = none`). Do not recursively init optional/heavy nested submodules without a task that needs them.

### Existing Hermit PR checkout — never validate against the PR's historical Reverie pin

Checkout and preparation are one operation: `scripts/checkout-hermit-pr-latest-reverie.sh --repo
worktrees/<slot>/hermit [--push] <pr>`. It fetches current Hermit+Reverie main, checks out the PR without
rewriting history, merges current Hermit main, runs `check-reverie-pin.rs --update-to-latest` to update every
tracked pin site, commits the pin update, and runs full validation at that commit. `--push` publishes the
validated candidate with a non-force push. A stale pin is a hard validation failure; do not substitute raw `gh
pr checkout` + validation.

### Slot closing / parking / reclaiming

Close a slot only after intended work is committed and handed off. Record each child HEAD, feature branches,
exact SHAs, validation, and integration disposition in `ARCHIVED.md`; detach each child at its parent-pinned
gitlink so `git -C $slot status --short` is empty; remove the slot's row from `ACTIVE.md`. Keep feature
branches until reachable from a pushed branch or merged target. Reclaim a parked slot only through
`scripts/release-worktree.rs --slot <slot> --clean`; it binds the registry owner, process lease, recursive
cleanliness, and branch durability before ordinary non-force Git removal. After a newly allocated or replacement
owner is live, re-run the same allocator adoption so its exact tmux pane and unified cgroup lease are durable;
legacy/unbound ownership cannot authorize cleanup. Release excludes container creation, invokes the canonical
agent-podman owner/mount audit, then atomically moves the target off its published slot path and repeats the
process/container proof before removal. If an interrupted submodule cleanup or path fence left a release journal,
use the script's guarded `--recover-submodule-cleanup` path; never substitute raw `git worktree remove --force` or
`git worktree prune`. To reuse a parked slot, repeat the clean-start audit and branch from the current base — never
reset a parked child to make it current.

### Worktree Registry — conflicts to resolve before assigning a slot

Reconcile the registry with all Git worktree registries (`git worktree list --porcelain` for parent and each
product) and the filesystem; the parent list owns canonical nested slots, product lists should normally contain
only the primary checkout. Resolve before assigning: an unregistered physical checkout; a registered worktree
whose directory is missing; a live slot absent from `ACTIVE.md`; a row for a parked/missing slot; duplicate
rows; a branch checked out by more than one worktree; any non-canonical path. Never silently delete a stale
path — record what owns it and preserve uncommitted work first.

### Clean Start — submodule status flag legend

Submodule status flags (`git submodule status`): leading space = matches gitlink; `+` = HEAD differs (not
automatically an error — integration may be in flight; attribute before acting, do not erase with a submodule
update unless the reset is intended); `-` = not initialized; `U` = merge conflict.

## Communication Precision — full report-writing rules

Governs coordinator headlines, cross-task aggregation, and user-facing progress reports. Reports must be
specific enough that another engineer can act without re-deriving the scope.

- **Never headline a bare pass ratio.** `10/10 pass` is not a headline. Name the program category, the exact programs (or link a table), the Hermit mode and backend, and why that batch was selected.
- **Separate new results from baseline.** Label every rollup `New this run`, `Baseline reconfirmed`, `Regression`, or `Not rerun`, and state the commit/PR that changed between compared runs.
- **Classify programs before totaling** (system utilities, text-processing, interpreters/runtimes, compilers/build tools, databases, network programs, interactive applications, virtualization/emulators); mixed batches need subtotals.
- **Name execution context** (native baseline, ptrace, DBT, KVM; strict run / strict verify / record-replay / relaxed) and why it answers the batch question.
- **Name the tool** — never "the Tool"; say `StraceTool`, `Detcore`, `CounterTool`, etc.
- **Give the exact command line**, **say where** (`main`, `PR #N`, or exact branch/SHA), and **qualify the result** (determinism level `L0`/`L1`/`L2`, pass count `18/20`, exact programs/tests covered). "It works" is not a result. Bind evidence to commits, not branch names.
