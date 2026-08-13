# devbig030 is refusing validate admission — and it is not the CPUID gap

2026-08-12, ~20:15 PDT. Incidental finding while auditing approval lines; recorded separately
because a reboot decision is pending on a premise this measurement changes. Read-only: I did
**not** run the recovery command, clear the quarantine, or touch any lock state.

## The claim being acted on, and what is actually true

The claim relayed to the fleet was that hermit-908 tried to validate **#2213** and **no
validate run was admitted at all**, offered as evidence that the CPUID-faulting gap has
escalated from "runs fail" to "runs cannot start".

Measured:

- **Runs are being admitted on devbig030.** The ledger carries 12 `run.result` events for this
  box since 2026-08-13T01:00Z, the most recent at 02:53:18Z. They fail, but they run.
- **hermit-908's run was admitted and completed** — for **#2221**, not #2213:
  `state: completed`, target `e00caa4903e4…`, started 02:39:59Z, finished 02:40:22Z,
  `exit_code: 3`.
- **Exit 3 was an admission refusal, not a test failure.** From its log:

  ```
  validate-lock: box-global anchor held at /run/user/212630/ci-hub/validate-box.lock
  validate-lock: cleanup quarantine: ACTIVE: payload identities remain active:
                 2487923@15655764, 2553747@15658810
  ```

So the blocker is a **validate-lock cleanup quarantine**, not the CPUID capability. The CPUID
gap makes a run *fail a cell*; this makes a run *never start*. Two different mechanisms with
two different remedies, and merging them is the same error the fleet has now made twice today.

## The quarantine is holding on dead identities

Both identities the quarantine names as "remain active" are dead:

```
PID 2487923 DEAD (no /proc entry)
PID 2553747 DEAD (no /proc entry)
```

`ci-hub validate-lock status` agrees, and is explicit about why it still refuses:

```
residual=2553747:15658810
verification=all recorded residual identities and the captured process group are absent,
             but residual process-domain capture was incomplete; escaped descendants
             remain unproven
owner_pid=425967
validation_supervisor_process=exited:pid 425967 is absent
recovery=ci-hub validate-lock census-orphaned-domain --agent hermit-902 \
         --operation validate:c2fbd504ade4798536bedc37c36d75b6043455bb --pgid 1157509 \
         --attest-domain-empty --attester '<agent-name>' --evidence '<observations>'
recovery_precondition=confirm by hand that the payload's unit cgroup is absent/empty AND
             every cgroup it could migrate into is empty. reclaim-dead alone CANNOT clear
             this state
```

**This is fail-closed by design, not a defect.** The supervisor died before it could census its
payload domain, so the lock cannot prove no escaped descendant is still running, and it refuses
rather than guessing. That is the correct behaviour. It also names its own recovery path, which
requires a human-or-agent attestation that the cgroups are empty.

I did not run it. The precondition is a by-hand proof about cgroups on a shared box carrying
other agents' work, and an unproven attestation is exactly the kind of manufactured evidence
this fleet has spent the day removing. Recording it for the owner or coord to action.

## Consequences for the pending reboot decision

Three independent constraints on this box, and the reboot answers only one of them:

| constraint | what it blocks | does a reboot fix it? |
| --- | --- | --- |
| CPUID faulting absent (kernel) | the `privileged-cpuid.faulting` cell, hence full/privileged receipts | **unknown** — 6.19.2 carries `MSR_K7_HWCR_CPUID_USER_DIS` which 6.13.2 lacks, but that is a header define, not proof the `arch_prctl` path sets the feature for AMD ([capability note](host-capability-cpuid-faulting_20260812.md)) |
| validate-lock cleanup quarantine (this note) | **all** validate admission on the box | **yes, incidentally** — the state lives in `/run/user/<uid>` and does not survive a boot |
| review authority | landing, everywhere | no |

So a reboot would clear the admission quarantine as a side effect while leaving the question it
is actually being considered for unanswered. Conversely, clearing the quarantine through the
documented census path restores admission **without** answering the CPUID question either. They
should be decided separately, and the quarantine has a documented remedy that does not require
a reboot at all.

Worth stating plainly: with the quarantine active, *no* PR can be validated here regardless of
CPUID, so the "route everything to devbig014" conclusion holds today — but for a reason that is
cheaper to fix than a reboot.

## Also: a third stale record, left as found

The box anchor still names hermit-908's completed run:

```
pid=3790780 host=devbig030 agent=hermit-908 kind=validate target=e00caa4903e4…
```

`pid 3790780` is gone. This is the same shape as the stale `landing-box.lock` record naming a
dead PID recorded in [the lock premise audit](hermit-main-lock-premise-audit_20260812.md).
Preserved exactly as found — clearing these is what destroys the diagnosis.

## Commands

```sh
cat /run/user/$(id -u)/ci-hub/validate-box.lock
python3 -c "import json;print(json.load(open('<record path from the anchor>')))"
tail -12 ignored/validate/validate-hermit-908-e00caa4903e4-1786588799.log
for p in 2487923 2553747; do [ -d /proc/$p ] && echo "$p ALIVE" || echo "$p DEAD"; done
./ci-hub/ci-hub validate-lock status
```
