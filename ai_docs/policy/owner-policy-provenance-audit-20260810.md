# Owner-policy provenance audit — 2026-08-10

**Task:** `audit-invented-policy-attributed-to-the-owner`

**Snapshot:** `rrnewton/dev-hermit` main `2fbf74848f4ec68bba8dae3683e6fd25e57c43c8`

**Scope:** parent `AGENTS.md`, 40 versioned coordinator skills, 24 ci-hub text
documents plus their named executable consumers, 21 directive-ledger records,
497 TaskGraph tasks, and observed coordinator practice.

## Decision summary

The systemic defect is real: a valid engineering concern can acquire an
agent-designed ceremony, then be reported back to the owner as though the owner
specified that ceremony. The failure is false attribution, not necessarily a
bad safety concern.

The audit's two motivating cases have different classifications:

1. **Mandatory structured gate-rotation record: INVENTED ceremony over a
   DERIVED concern.** The owner did not recognize the phrase
   “repository-owner-authored rotation record” (live session turn 1890). The
   underlying rule — branch-controlled gate code cannot authorize its own gate
   replacement — remains a valid derived fail-closed control. A plain, clearly
   owner-authored approval is sufficient owner intent; the exact JSON form may
   remain an optional machine-friendly form, not the only accepted speech act.
2. **Critical double review: OWNER-ORIGINATED, but previously over-scoped.** The
   owner explicitly required double review for Hermit syscall handling, DetCore
   scheduling, and Reverie API changes (turn 1897), then refined the rule to two
   different reviewer agents, with cross-agent/model diversity preferred rather
   than universally mandatory (turns 1899 and 1902). Calling the whole rule
   invented would have deleted a real safety requirement.

The default when provenance is missing is **UNKNOWN**, not OWNER-ORIGINATED and
not INVENTED.

## Classification contract

| Class | Required evidence | What the class permits |
|---|---|---|
| **OWNER-ORIGINATED** | Recoverable verbatim owner message, with session/turn or durable message id. | Quote and enforce the owner's semantic rule within its stated scope. |
| **DERIVED** | Named owner decision plus a traceable explanation of how the mechanism implements it. | Keep the control on engineering merits; never quote the mechanism as the owner's words. |
| **INVENTED / STALE** | No owner basis after evidence search, or a later owner message explicitly rejects/supersedes the rule. | Remove mandatory ceremony or stale scope while preserving any separately justified concern. |
| **UNKNOWN** | Provenance cannot currently be established. | Keep fail-closed where safety requires, but label it fleet-derived/unknown and ask rather than impersonating owner authority. |

Absence in one search surface is not proof of invention. The dual-review
correction demonstrated why: the first session-local search found no owner hit,
while the append-only predecessor/live record did.

## Evidence hierarchy and limitations

1. The primary evidence used here is the append-only `content_blocks` table in
   the ORC session databases. Owner citations below identify session, turn, UTC
   timestamp, and GChat message id when present. `user_source.GChat.is_owner=true`
   distinguishes owner GChat from submitted coordinator relays.
2. Versioned policy, skills, task descriptions, labels, coordinator prose, and
   the directive ledger are **claims about provenance**, not provenance.
3. All **497/497 current TaskGraph task creator fields are NULL**. A task titled
   `OWNER DIRECTIVE` therefore proves no owner source by itself.
4. The directive ledger has 21 records. Its `source_row` values do not identify
   a source session/database/message and its records contain no verbatim owner
   quote. It can prove implementation/ancestry state, not speaker identity.
5. The shared `rrnewton` GitHub identity historically represented both the
   human and agents. Unprefixed history before the fleet-comment cutover cannot
   establish speaker identity from login alone.
6. Optional local memory mirrors are advisory. Repository `AGENTS.md` and
   versioned skills are authoritative; memory drift cannot overwrite them.

## Verified owner-originated semantic roots

Mirrors inherit the classification of the semantic root. Exact schemas,
filenames, labels, flags, and parsers remain DERIVED unless the source message
specified them.

| Semantic root | Owner source | Scope retained |
|---|---|---|
| Autonomous, forward-driving, self-healing team | predecessor `4fb50e87…`, turn 7642, 2026-07-30 08:54:21 UTC, GChat `9VhLpT22Izk.0JMON9nd_tk` | Replace verified-dead agents without routine permission. “Verify before replace” is derived safety. |
| Structured hourly status with workstream→worker map | predecessor turn 7658, GChat `N8LZ3zIzOCU.zEbjI7nZUc0` | Structured status record. Exact JSON schema/script is derived. |
| Role/model prefixes on PR descriptions/comments; distinguish human | predecessor turn 2890, 2026-07-24 14:39:49 UTC | Speaker provenance. Exact trailer grammar and wrappers are derived. |
| Parent/nested-worktree operating model | predecessor turn 95; live turn 959, 2026-08-09 15:29:30 UTC, GChat `6MMzf8JTCi0.6MMzf8JTCi0` | Coordinator owns parent; agents edit allocated slots, with named parent-output exceptions. Exact registry schema is derived. |
| Capacity: ≤12 workspaces, ≤5 unused/parked slots, ≤15 agents under the stated sharing assumptions | predecessor turn 164, 2026-07-21 21:54:32 UTC | These numbers are owner-originated. This corrects the preliminary UNKNOWN classification. |
| Never use `git clean` | predecessor turns 2851, 5709, and 7645 | Destructive cleanup prohibition. Specific recovery tools are derived. |
| Bot-created issues stay on `rrnewton` forks | predecessor turn 168, 2026-07-21 21:58:37 UTC | Fork-only bot issues. The issue wrapper is derived enforcement. |
| Avoid big/binary files in the parent | predecessor turn 95 | Artifact placement rule. Exact 2 MiB threshold is not established by that message. |
| Post-facto human review mode; one `post-facto-human-review` label; owner-only `human-approved` | predecessor turn 7242, GChat `eRQ20DieSaA.eRQ20DieSaA` | Human review is post-land/fix-forward; obsolete labels stay retired. |
| PR sections: Summary, Determinism, Validation; KVM Relationship to gVisor; Human Review Required for labeled PRs | predecessor turn 7280, GChat `Vqm6h8v8oLw.Vqm6h8v8oLw` | These named sections and scopes are owner-originated. |
| Core DetCore scheduling is a post-facto-human-review trigger | predecessor turn 7281, GChat `Vqm6h8v8oLw.YJHl_7uwHQg` | Core scheduling/race-search changes. |
| At least one adversarial review for every PR | predecessor turn 10947, 2026-08-03 15:25:38 UTC, GChat `65BtSt8lE6g.2ew-JhbF9xY` | Ordinary single-review path. |
| Critical double review | predecessor turn 7967; latest controlling refinement live turns 1897/1899/1902 | Exactly Hermit syscall handling, DetCore scheduling, and Reverie API; two different reviewer agents. Cross-agent/model diversity is preferred, not a universal hard family gate. |
| Local VALIDATE recorded in the LEDGER is normal Hermit landing authority; GH Actions is secondary/P2 | live turns 1348 (`pP_MNiC4OQ8.0bIAnHnSo8w`), 1355 (`q2aDzSVjnZs.k9R7L2mSzSI`), 1856 (`ALk0fgvl8YE.SKVpaUMWghU`), and 1878 (`8yurH_jb_oE.H5SM8IsyotI`) | Work → review → VALIDATE → LEDGER-derived label → author lands → repeat. |
| Red-tip fix is P0 and lands directly | predecessor turn 21024, GChat `LLLI-aRgrtw.7IwqKBRrbGI` | The fix for proven red main, not unrelated work. Exact lander mechanics are derived. |
| Do not use Meta development boxes as external GH Actions runners | predecessor turn 14750, GChat `dmyUYensZ_c.7AZjxyzib9Y` | External workflow code cannot execute on those high-power developer boxes. The exact never-root/minimum-privilege topology is derived security design. |
| Neutral future agent names | live turn 1931, 2026-08-10 15:07:33 UTC, GChat `Mqa7DSb0MB0.vASNNagMqAk` | Do not leak stale task purpose in future agent ids. `Task:` trailers are derived provenance support. |
| Compatibility green requires two full deterministic/parity runs with identical output | live turn 1911, GChat `TxwIyxf22V8.6gzBMa4B4AQ` | Compatibility-scorecard cells only; it does not replace VALIDATE's full-profile/count/coverage predicate. |

## Derived controls that should remain, without fabricated owner attribution

| Control family | Owner decision implemented | Derived mechanism |
|---|---|---|
| Exact evidence binding | “Dig deeper/check assumptions”, adversarial review, and local-ledger authority | Exact-head `APPROVED-AT`, stale-review invalidation, nonzero execution, coverage counts, one verifier per authority, positive/negative mutation brackets, proxy-binding registry. |
| Local landing authority | VALIDATE/LEDGER is the normal authority | `validate-run` admission, durable receipt schema, `validate-status`, `locally-validated` as a cache, land-lock, ancestry proof, and `NO_RESULT` fail-closed semantics. |
| Producer self-vouch refusal | Trust the recorded run and do not certify untested behavior | A PR modifying registered producer `scripts/validate.rs` cannot use its own candidate-produced receipt; registered exact-head external evaluation is the narrow exception. This is engineering policy, not an owner quote. |
| Gate self-authorization refusal | Review and evidence must remain independent | Candidate gate code cannot authorize its own replacement; trusted-current-main code verifies the transition and registers only after landed ancestry. Keep `SELF_AUTHORIZATION_REFUSED`. |
| Review independence | One review ordinary; two agents for named critical classes | Author-disjoint reviewer preference, exact-head review receipts, and rejection invalidation. Do not turn model family into a hard universal proxy. |
| Worktree safety | Coordinator-owned parent and allocated nested worktrees | `ACTIVE.md`, allocator/release single writers, explicit path ownership, clean handoff, recovery refs, no stash, explicit-path commits. Exact path spelling and lease schema are derived. |
| Process safety | Autonomous self-healing plus “measure twice, cut once” | PID + start_ticks + boot_id identity, narrow PGID/child cgroup, no broad pattern kills, verify-before-replace. |
| Task closure integrity | Autonomous completion and evidence-based reporting | `implemented` tag, PR/artifact/SHA evidence, anti-phantom review, ancestry-backed `CLOSURE-VERIFIED`. |
| Git/submodule integrity | Parent reproducibility and no destructive history | Pin ancestry checks, fresh fetches, main-only products, no force-push, latest-Reverie checkout helper. |
| Comment provenance | Human/agent speaker distinction | Role/team/machine trailers, fleet-comment prefixes, exact task trailers, and comment wrappers. |
| Bot issue destination | Fork-only bot issues | Registered issue-creation wrapper that rejects/rewrites upstream destinations. |

These controls may be mandatory because the repository adopted them, but their
documentation must say “derived control implementing X,” not “the owner required
this exact schema.”

## Invented, stale, or scope-inflated enforcement

| Current/observed rule | Classification | Required disposition |
|---|---|---|
| A structured `GATE-DEFINITION-ROTATION-AUTHORIZED` JSON object is the sole valid owner approval | **INVENTED mandatory ceremony.** Turn 1890 shows the owner did not recognize it; turn 1894 agrees only to a plain PR note. | Accept a clearly owner-authored plain approval; keep structured form optional. Preserve every independent gate/head/blob/path/review/receipt check and self-authorization refusal. |
| Universal Claude+Codex lanes, universal double review, or same-family reviewer refusal | **STALE / OVER-SCOPED.** Turns 1897, 1899, 1902 define the narrower current rule. | One review ordinary; two different reviewer agents only for the three critical classes. |
| Codex-only fleet restriction | **EXPIRED.** Turn 1900 says its reason was rate limits and intended lifetime was 12 hours. | Historical evidence only; never standing dispatch policy. |
| `Linux Semantics` mandatory on every ordinary PR | **OVER-SCOPED.** The source at predecessor turn 7967 concerned core determinism/virtual-time/post-facto work. | Keep it for the critical/post-facto lane; do not impose it universally without a new owner decision. Current `.claude/skills/post-facto-review/SKILL.md` still contains the universal wording and conflicts with `AGENTS.md`. |
| Task title/tag/priority/source_row proves owner authorization | **INVENTED proxy.** 497/497 task creators are NULL; directive rows do not dereference a message. | Require a source pointer/quote or classify UNKNOWN. |
| Shared `rrnewton` login proves owner speech | **INVENTED proxy.** Agents historically wrote through the same identity. | Use post-cutover fleet prefixes and an independently identifiable owner message; pre-cutover unprefixed history remains ambiguous. |
| Normal Hermit landing waits for hosted green or a merge-gate | **STALE.** Superseded by live turns 1348, 1355, 1856, 1878. | GH Actions remains P2 except the narrow registered-producer self-vouch exception. |

### Live enforcement contradiction at this snapshot

`ci-hub/landing/README.md` still states that the newest owner comment **must**
contain the structured marker and exact JSON object. The executable parser and
its tests also have uncommitted, separately owned work attempting to relax that
ceremony. This audit does not absorb or publish those files. The final change
must preserve self-authorization refusal and reject fleet-authored comments
masquerading as owner speech.

## UNKNOWN roots — do not remove, but stop attributing them to the owner

No recoverable owner source was established in this pass for these exact
mechanisms/scopes:

- mandatory `mechanism:<slug>` tags and their exact overlap behavior;
- stable lowercase outcome-slug syntax for every semantic unit;
- exact `worktrees/<slot>/{hermit,reverie,liteinst2}` spelling, the ban on
  branch-named directories, and some lease/adoption details (the nested-slot and
  coordinator-ownership roots are owner-originated; these details are derived
  or unknown);
- “at most one agent-utils PR in flight” and the exact direct-main publishing
  protocol;
- one-logical-commit style and several commit-message formatting details beyond
  speaker attribution;
- the exact 2 MiB text threshold;
- individual role charters and task-specific gates not linked to an owner
  message.

UNKNOWN does not mean unsafe or removable. Each item needs either a source
pointer, a clear derived-from statement, or an owner decision before anyone
calls it owner policy.

## Task descriptions and coordinator practice

TaskGraph is a work ledger, not a speaker-authentication system. At this
snapshot:

- tasks: **497**;
- tasks with NULL creator: **497**;
- directive records: **21**, with non-dereferenceable `source_row` fields and
  no stored source quote;
- versioned skills: **40**;
- active owner-attribution phrase hits outside transient status logs: the live
  set includes correct citations, derived controls described too strongly, and
  the rotation-record contradiction above.

Therefore every task-specific “OWNER DIRECTIVE” obligation is UNKNOWN unless
its description/note carries a dereferenceable owner message. Coordinator
repetition cannot upgrade it.

## Guardrail design

The durable fix is provenance metadata at the policy boundary, not another
ceremony reported as owner speech.

1. Every new enforced clause records:
   - `provenance_class`: owner-originated / derived / invented-stale / unknown;
   - `source_session`, `source_turn`, and durable message id/quote for owner
     origin, or `derived_from` naming an already cited semantic decision;
   - reason;
   - expiry or explicit review trigger when temporary.
2. A linter rejects the phrases “owner decision,” “owner required,” and
   “owner-authored” without a source pointer. It does **not** reject an UNKNOWN
   or DERIVED safety control.
3. Task/directive schemas gain creator/speaker provenance and a dereferenceable
   source-message pointer. Titles, tags, and `source_row` remain caches.
4. Policy mirrors point to one versioned root. Optional memory can be compared
   read-only but never promoted over repository policy.
5. Review checks separate **concern** from **mechanism**. Example: gate
   self-authorization is the concern; one exact JSON speech format is a
   mechanism. Rejecting the mechanism must not erase the concern.
6. Every provenance audit prints raw matched owner lines before counting them;
   a zero-hit search of one session is insufficient to classify INVENTED.

## Immediate handoffs

- `global-consistency-pass-memories-skills-agents-md`: reconcile the live
  universal-Linux-Semantics wording and keep expired Codex-only/universal-family
  rules out of every mirror.
- gate-rotation implementation owner: accept plain post-cutover owner approval
  without weakening `SELF_AUTHORIZATION_REFUSED`, exact transition checks, or
  fleet-comment impersonation refusal.
- TaskGraph/directive maintainers: add source-message provenance; until then,
  report owner origin as UNKNOWN unless independently cited.

No enforcement code or existing policy file was changed by this audit. The
artifact classifies and routes; each implementation remains separately owned
and reviewed.
