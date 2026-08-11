# Slot-owner sidecar and legacy recovery contract (2026-08-10)

## Decision

Every new or re-adopted slot owner must carry a versioned `owner_sidecar` in
its `worktree-state.json` slot row. The allocator writes it under the same
exclusive registry lock as the ownership update. Keeping it in the guarded row
is intentional: a second file would create a crash window in which the registry
and its alleged owner generation disagree.

Schema 1 binds all of these values:

- slot, mutating agent, task, source, and recording timestamp;
- supervisor PID, host boot ID, and `/proc/<pid>/stat` start ticks;
- an exact generation derived from that three-part process identity;
- the owning cgroup and, for ORC owners, the tmux pane.

`scripts/slot-owner-authority.py --slot <slot> --json` is the only verifier for
this sidecar. It holds the registry lock while reading and returns:

- `dead`, rc 0, only for a boot-ID mismatch, absent PID, PID reuse, or an exact
  recorded process in a terminal kernel state **and** an empty/absent recorded
  owner cgroup;
- `alive`, rc 1, when PID, boot ID, and start ticks all match a running process;
- `indeterminate`, rc 2, for missing, malformed, unsupported, unreadable, or
  binding-mismatched evidence.

Only `dead` can participate in a recovery decision. `alive` and
`indeterminate` both refuse. Validate-lock remains a separate box-compute
authority and is never consulted for slot ownership.

The cgroup condition closes a process-namespace blind spot: a sandbox may not
resolve a host PID through its `/proc` even while the owner remains in the
recorded cgroup. Any cgroup member, including an unmapped member rendered as
PID `0`, makes the result `indeterminate`; unreadable cgroup state does too.

## Allocation and adoption

For an ORC owner, `allocate-worktree.rs` resolves the requested agent to one
live tmux pane, samples the pane supervisor start ticks before and after reading
its cgroup, reads the current boot ID, and publishes schema 1 atomically with
the pane/cgroup lease. A failed adoption removes any prior sidecar so an old
incarnation cannot survive as apparent authority.

For a Codex coordinator slot, the already-versioned systemd sentinel remains
the slot-level supervisor. The allocator first re-verifies its recorded PID and
start ticks, then adds the current boot ID and publishes the same schema-1 slot
sidecar. This sidecar proves the sentinel incarnation, not that an individual
logical Codex subagent is running.

An allocation performed before its future owner exists remains deliberately
unbound and therefore `indeterminate`. The living owner must re-run the exact
allocation command (adoption) before the row becomes provable. Inventing a
coordinator or allocator PID as a substitute would bind the wrong subject.

## Legacy cohort: 34 recoverable, 34 permanently unprovable from recorded evidence

The task-start measurement was 68 of 73 active rows without a complete
mutating-owner pane/cgroup lease. One concurrent allocation later changed the
live totals to 69 of 74; the analysis below preserves the original 68-row
denominator by excluding that later `codex-validatelock` row.

| Legacy class | Rows | Recoverable now | Contract |
|---|---:|---:|---|
| Exact-live coordinator sentinel (`main_pid@main_pid_starttime` matches `/proc`) | 33 | 33 | Re-run the exact sentinel allocation/adoption; verify first, then record boot ID and schema 1. |
| Exact live ORC pane but no recorded cgroup (`codex-timeouts`) | 1 | 1 | Re-run allocation from the live pane; capture the pane supervisor identity and cgroup. |
| No exact-live sentinel or pane identity | 34 | 0 | Do not synthesize a sidecar. These are permanently unprovable from the historical slot record and remain `indeterminate`; they must age out through the separately reviewed recovery mechanism below. |
| **Total** | **68** | **34** | **34/68 recoverable; 34/68 permanently unprovable from recorded evidence.** |

All 33 sentinel identities were re-read from `/proc` and matched their recorded
start ticks. The one ORC migration candidate was present in the fresh agent
snapshot with a live pane. These positive facts permit a present-tense rebind;
absence from either source was not interpreted as death.

## Recovery for the 34 unprovable rows

These rows cannot be migrated to schema 1 retroactively. Their missing process
identity is historical evidence that was never recorded, and a later PID,
agent-name census, idle clock, clean checkout, or empty process scan cannot
reconstruct it.

The only current non-destructive recovery is the coordinator protocol adopted
for legacy rows:

1. `agent_liveness_probe.py <recorded-owner> --json` must return rc 0
   `verified-dead`; rc 1 `alive` or rc 2 `unverifiable` refuses.
2. Independently prove all child repositories clean and detached as recorded,
   archive exact recovery SHAs/branches, and prove zero users through
   cwd/exe/root/fd/map-files/maps and cgroup inspection.
3. Confirm the row has no live `coordinator_lease` (non-clean release is
   refused for those rows).
4. A coordinator may run `release-worktree.rs --slot <exact-slot>` without
   `--clean`. This retains every file and marks the row released.

The non-clean release command merely warns about dirt and does not itself
enforce the zero-process census. Therefore the external proofs above are
mandatory. A row whose owner cannot be independently proven dead remains
active indefinitely unless a future owner returns and performs a live
adoption, or the human owner approves a new, separately reviewed mechanism.
Age alone is not such a mechanism.

## Required brackets

The verifier tests pin both sides and the failure boundary:

- exact live identity: `alive`, never `dead`;
- missing PID, reboot, or PID reuse: exact old incarnation `dead`;
- missing legacy sidecar, unsupported schema, binding mismatch, malformed
  process record, unreadable cgroup, or unreadable registry: `indeterminate`,
  never `dead`.

---

## Unified lease extension and live census (2026-08-11)

Status: durable design and registry census only. No lease, slot, pane, lock,
process, or registry state was changed while producing this extension.

Task: `one-lease-model-ttl-and-standard-stale-clear-for-every-holder`

Artifact attribution:
`[hermit2, codex-drain1, gpt-5.6-sol, devbig014, role=debugger]`

### Decision

Use one versioned lease protocol for validate-lock, slot ownership, Herdr panes,
and future holdable resources. Preserve the operational shape of the Herdr pane
fix at parent commit `67623cc5`: cap the resource, reap at the next create, and
use a TTL so a dead producer does not require a human to notice it. Do not copy
its recordless-old classification literally: expiration makes a resource
eligible for a stale-clear attempt; it is not evidence that the holder is dead.

The common safety rule is:

> Clear only after the exact holder generation is proven dead, the cleanup
> domain is proven empty and complete, the recovery handoff is durable, and the
> resource generation is unchanged. LIVE and INDETERMINATE both refuse.

This preserves both required brackets: a dead holder's expired lease clears
without a human, and a live holder's lease is never cleared out from under it.

### Exact five-row census

Measurement time: `2026-08-11T10:52:03-07:00`.

Parent HEAD at the investigation boundary:
`76385fd756bde58bbc1020120593227e470e25e1`.

Source: canonical local `worktree-state.json`, queried by status and then by the
exact `owner-lease-revoked` rows. This is a registry census, not an estimate
from process names, age, directory count, or the residue gate.

| Registry state | Rows | Denominator |
|---|---:|---:|
| `active` | 77 | 77/96 total |
| `owner-lease-revoked` | 5 | 5/96 total |
| `released` | 14 | 14/96 total |
| **Total** | **96** | **96/96** |

The cap-counting/non-released denominator is 82 (`77 active + 5
owner-lease-revoked`). The half-released population is therefore **5/82 =
6.1%** of non-released rows, and **5/96 = 5.2%** of every registered row. Both
denominators are reported so released residue cannot change the apparent rate
silently.

| Slot | Recorded owner | Recorded task | Revocation phase |
|---|---|---|---|
| `codex-rescue` | `codex-rescue` | `encode-per-node-write-domains-and-fail-closed` | `revoked` |
| `codex-validatelock` | `codex-validatelock` | `p0-validate-lock-quarantine-refused-entire-fleet` | `revoked` |
| `slot04` | `codex-reviewC` | `p0-cmake-cache-key-omits-source-path-green-run-poisons-next` | `revoked` |
| `slot74` | `sol-1843` | `1843-no-verify-pin-lint-bypass-carries-an-untracked-rebase-obligation` | `revoked` |
| `w7` | `codex-reviewB` | `confirm-main-validated-at-new-head-a51da7ad` | `revoked` |

All five rows carry `coordinator_lease_revocation.phase=revoked`. Zero of 96
rows carry `release_journal`; zero of 96 carry `orphan_residue_journal`. The
unfinished state is the retained coordinator lease-revocation transaction, not
a path-removal release journal. This terminology matters: repairing the wrong
journal would not make any of the five claimable.

This count is narrower than the residue gate's 15-16 candidate reports. It
counts the exact registry state under investigation and does not reclassify the
other candidates as half-released.

### Root cause in the slot release path

The release guard currently asks three different questions as though they were
one:

1. **Ownership:** may this caller renew or request release?
2. **Liveness:** is the exact recorded holder generation alive?
3. **Active use:** does any process or container still reference the resource?

`release-worktree.rs` rejects when a live process's `DG_AGENT_NAME` matches the
recorded owner before its later cwd/exe/root/fd/maps, cgroup, container, and
target-path census. The name match proves identity and liveness, not active use.
A correctly finishing owner is still alive while invoking release, so the guard
revokes the coordinator sentinel, records the intermediate transaction, and
then refuses the same owner. Recovery and re-adoption also refuse that retained
transaction. `w7` and `slot04` are two instances of this transition.

The active-use checks already exist separately. They remain load-bearing; only
the owner-name-as-use veto moves out of that predicate.

### Canonical record and verb

Each resource keeps its lease in its existing canonical authority. Do not add a
second box-global cache that can drift. A shared verifier reads the records
through resource adapters.

Every record carries:

- schema version, resource kind/id, and random generation/CAS nonce;
- holder principal plus PID, boot ID, start ticks, and supervised domain;
- issue, renewal, TTL, and expiry times;
- state and cleanup-domain descriptor;
- recovery-handoff manifest and digest;
- last transition actor, action, time, old/new record digests, and evidence.

TTL values may differ by resource. Every lease has an explicit expiry and a
normal heartbeat renews no later than one-third of the TTL. Expiry is a
scheduling and authority boundary, never death proof. A worktree is an
ordinary filesystem tree, so metadata cannot revoke a live process's ability
to write it; the lease governs allocator/releaser authority and reuse.

The public verb family is:

```text
ci-hub lease status          --resource <kind>/<id>
ci-hub lease renew           --resource <kind>/<id> --expected-generation <nonce>
ci-hub lease release-request --resource <kind>/<id> --expected-generation <nonce>
ci-hub lease stale-clear     --resource <kind>/<id> --expected-generation <nonce>
ci-hub lease stale-clear --all
```

Existing `validate-lock reclaim-dead`, release-worktree recovery, and pane reap
become compatibility front doors into the same verifier. They do not retain
independent decision logic.

`stale-clear` returns three distinct outcomes:

- `CLEARED`: exact holder dead, domain empty and complete, handoff durable, and
  generation unchanged;
- `REFUSED`: holder live, lease neither expired nor release-requested, domain
  populated, or caller unauthorized;
- `INDETERMINATE`: identity, record, schema, host, domain, or evidence cannot be
  verified.

Neither refusal outcome is a pass or permission to retry destructively.

### State machine and self-release

```text
ACTIVE -- exact holder renew ------------------------------> ACTIVE
ACTIVE -- exact holder release-request --------------------> DRAINING
ACTIVE -- TTL ---------------------------------------------> EXPIRED

DRAINING/EXPIRED -- LIVE ----------------------------------> REFUSED
DRAINING/EXPIRED -- INDETERMINATE -------------------------> QUARANTINED
DRAINING/EXPIRED -- DEAD + POPULATED/UNKNOWN domain -------> QUARANTINED
DRAINING/EXPIRED -- DEAD + empty complete domain
                  + durable handoff + unchanged generation -> CLEARED
```

Only an `UNPROVEN` domain may use an explicit `domain_complete` attestation.
Record its attester and evidence. Attestation never overrides LIVE,
INDETERMINATE liveness, a POPULATED domain, generation drift, or a missing
handoff.

The exact live owner may request release. That records DRAINING and stops
renewal. A separate finalizer waits for the exact owner generation to exit and
then repeats every active-use and generation check at the final removal
boundary. This lets a finishing agent start release without pretending it is
already dead, while preserving live work.

### Attribution and automatic handoff

Every successful clear records the acting UID, PID, boot ID, start ticks,
machine, command/action, time, resource generation, old/new record digests, and
mechanical evidence. An operator-supplied `domain_complete` claim also records
the attester and exact evidence. The fleet comment/commit prefix is disclosure,
not authentication, and cannot be the protocol principal.

Before destructive finalization, the mechanism generates a manifest with:

- resource and holder generation, owner, and task;
- every repository branch, HEAD, cleanliness result, and origin reachability;
- validation target, log, receipt, and service handles where applicable;
- durable evidence paths, exact continuation argv, and manifest digest.

For a clean slot, HEAD is the automatic recovery SHA. Normal owner-initiated
release refuses before DRAINING unless intended work is clean and that SHA is
origin-reachable.

After sudden death with dirty work, no honest recovery SHA exists. Generate the
dirty-state manifest and continuation command, quarantine the slot, and remove
nothing. Do not manufacture an automatic commit: the tool cannot distinguish
intended untracked work from secrets, build products, or unrelated files.

### Capacity and the Herdr precedent

Run `stale-clear --all` before every acquire/create and optionally from a timer.
Clear only mechanically eligible leases, then apply the cap. Quarantined
resources remain non-reusable and non-cleanable. Report active, expired,
quarantine, released residue, and physical totals side by side so quarantine
cannot make occupancy disappear.

Commit `67623cc5` added the Herdr cap of 20, a 30-minute completed-pane linger,
a six-hour orphan TTL, and create-time reaping. The observed pane population
fell from 46 to 16. Keep those operational properties.

The current pane classifier skips an unreadable run record and can then reap an
old pane by label age. Its test deliberately makes a recordless pane past TTL
reapable. That lets age substitute for liveness. New panes must carry exact
identity and expiry; dead exact holders clear, live holders never clear, and
unreadable or legacy recordless holders remain INDETERMINATE. Capacity refuses
rather than evicting a possible live holder.

### Required brackets

1. Expired + exact holder DEAD + proven-empty domain + handoff clears.
2. Expired + holder LIVE refuses; deleting this guard fails this exact test.
3. Unreadable identity/schema, boot/start ambiguity, or generation drift
   refuses.
4. Dead + populated/unknown domain refuses.
5. Anonymous attestation refuses; named evidence discharges only UNPROVEN.
6. Exact live owner can request release; foreign/restarted owner cannot.
7. Finalizer waits for exit and repeats the full active-use census.
8. Use appearing between census and removal is caught by final CAS/fence.
9. Missing recovery SHA/continuation, dirty state, or unpublished work never
   removes files.
10. Old recordless pane past TTL remains indeterminate; new-schema dead pane
    clears.
11. Repeated stale-clear is idempotent and every success names its actor.
12. Removing attribution fails the attested-discharge test specifically.

### Migration and sequencing

Validate-lock and land-lock already carry expiry and heartbeat fields; adapt
them first. New slot allocations and panes write the common schema at creation.
Legacy records without exact identity or expiry remain INDETERMINATE. Adding a
timestamp cannot create missing historical evidence. Update the AGENTS.md
immortal-lease sentence only after every consumer calls the common verifier.

Implementation is held until these current owners publish or explicitly hand
off their work and the paths are clean:

| Order | Current owner/task | Contended surface | Required predecessor outcome |
|---:|---|---|---|
| 1 | `codex-2` / `p0-new-prefix-requirement-broke-the-lander-receipt-incompatibility` (active) | lander receipt-prefix consumer and current policy/docs surfaces | Land or hand off the exact repair and confirm receipt binding stays fail-closed. |
| 2 | `codex-validatelock` / `validate-lock-needs-a-sanctioned-discharge-verb-for-wedged-unknown` | `ci-hub/lib/validate_lock.rs` and shared attestation changes in `ci-hub/lib/landing_lock.rs` | Publish the exact attester/evidence schema and tests, or explicitly hand off those blobs. |
| 3 | `codex-validatelock` / `p0-slot-cap-breach-92-active-vs-cap-12` | `scripts/release-worktree.rs` and `scripts/release-worktree-target-scope-test.sh` | Publish or hand off the exact-slot cleanup boundary with its 5-positive/10-negative and 59-fixture evidence. |
| 4 | `codex-drain1` / unified lease task | common lease engine, adapters, policy, and focused tests | Re-read latest main after orders 1-3, confirm paths clean, then implement one engine and stale-clear verb. |

The loose gate-definition rotation pair is separately attributed to the closed
`codex-slots` task `audit-invented-policy-attributed-to-the-owner`; it is not
lease-model work and remains untouched. Unrelated dirty parent paths are never
staging authority.

After the path handoff, implement in one semantic sequence: common verifier and
read-only status; validate/land adapters; slot renewal plus two-phase release;
pane adapter with recordless-old refusal; compatibility front doors; then
policy updates and cross-consumer mutation tests.

#### Publication-time reconciliation

While this artifact waited for the serialized parent writer, order 2 landed as
parent commit `b4708652746df4950bddadd4dce756ad3b805c8b` immediately before this
document. It changes only `ci-hub/lib/landing_lock.rs` and
`ci-hub/lib/validate_lock.rs`, is on current `origin/main`, and records the
attester/evidence contract. Those two paths are now clean, so order 2 is
**satisfied**, not an outstanding wait.

The remaining implementation blockers at publication are order 1, the active
`codex-2` receipt-prefix repair, and order 3, the loose
`scripts/release-worktree.rs` plus target-scope test owned by
`codex-validatelock`. The unified lease implementation starts only after both
are published or explicitly handed off and their overlapping paths are clean.
