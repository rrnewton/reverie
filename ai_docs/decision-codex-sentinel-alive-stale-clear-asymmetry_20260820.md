# Decision needed: the one branch where an `alive` owner does not refuse release

**Raised:** 2026-08-20 by hermit-902, from
`ai_docs/slot-owner-authority-cannot-report-alive_20260820.md` point 4.
**Status: open. This is the owner's decision, not an agent's.** Nothing here is
implemented, and nothing should be until it is decided — it is the only place in
the slot lifecycle where a live-process verdict can be overridden, and changing
it in either direction changes what the release path is allowed to do.

## The branch

`scripts/release-worktree.rs::prepare_stale_clear`, lines 5353–5364:

```rust
let authority_state = authority["state"].as_str().unwrap_or("indeterminate");
let codex_sentinel = lease.source == "codex-systemd-sentinel-v1";
if authority_state == "indeterminate" {
    return Err(format!(
        "slot {slot} owner authority is INDETERMINATE; stale-clear is fail-closed"
    ));
}
bind_owner_authority_to_locked_state(state, slot, authority)?;
if authority_state != "dead" && !codex_sentinel {
    return Err(format!(
        "slot {slot} owner authority is {authority_state}; live ORC owner refuses stale-clear"
    ));
}
```

For an ORC-supervised owner, any verdict other than `dead` refuses. For a
`codex-systemd-sentinel-v1` owner, `&& !codex_sentinel` switches the check off,
so a slot whose guarded authority says **`alive`** can be stale-cleared. The
`indeterminate` refusal above still applies to both, and the lease must already
be expired, not quarantined, and generation-matched to reach this point.

## Why it is probably deliberate

A Codex sentinel is supervised by systemd, not by a tmux pane. Its lease TTL is
maintained by the sentinel unit itself, so an **expired** lease is a statement by
that supervisor that it has stopped renewing — arguably a stronger claim about
the *ownership* than the liveness probe's claim about a *process*. A systemd unit
whose main PID is still up but whose lease has lapsed is a plausible state, and
refusing forever on `alive` would strand those slots with no recovery path other
than the codex revocation journal.

The surrounding machinery supports that reading:
`verify_release_authority_absent` has a whole sentinel branch requiring
`sentinel.revoked` plus a durable revocation proof whose four identity fields
must still match, and `automatic_handoff` still has to come back complete or the
lease is quarantined instead. So the sentinel path is not unguarded — it is
guarded by a *different* authority.

## Why it should still be decided rather than left as a discovery

- It is the **only** place in the tree where "not dead" does not refuse. Every
  other gate in this mechanism is fail-closed, and the codebase says so
  repeatedly in its own comments. A single exception that is only visible by
  reading the `&&` in one condition is the kind of thing a future reader
  reasonably assumes is a bug and "fixes" — in whichever direction they guess.
- The refusal message for the ORC case names the reason (`live ORC owner refuses
  stale-clear`). There is no corresponding message on the sentinel path saying
  why a live owner was permitted, so the log of a sentinel stale-clear does not
  record that this trade-off was taken.
- Nothing currently exercises it. Every sidecar on devbig030 is
  `orc-tmux-pane-supervisor-v1` or dead-by-boot-id, and the authority has not
  emitted `alive` for anything on this host, so the branch is untested in
  practice and its behaviour is inferred from the source rather than observed.

## The options

**A. Leave the behaviour, document and log it.** Add a comment stating that an
expired systemd-maintained lease outranks a live-process verdict for a sentinel
owner and why, and emit an explicit line when the branch is taken so a stale
sentinel clear is attributable afterwards. *Cost:* none to behaviour. *Risk:* the
asymmetry remains real; a live sentinel process can still lose its slot.

**B. Make it refuse like the others** — drop `&& !codex_sentinel`. *Cost:* a
sentinel whose unit is alive but whose lease lapsed can no longer be stale-
cleared, and must go through the revocation-journal path instead. *Risk:* slots
strand if that path is broken or unavailable; this is the direction that can
*remove* a recovery route, so it needs someone to confirm the revocation path
actually works before it is taken.

**C. Narrow it** — permit the sentinel override only when the recorded sentinel
is *also* revoked with a valid durable proof, i.e. require the two authorities to
agree rather than letting the lease clock override the process verdict. *Cost:*
more work than A. *Risk:* lowest of the three, but it is a behaviour change and
should not be made on an untested branch without someone who knows the sentinel
lifecycle.

## Recommendation

**A now, C when someone owns the sentinel lifecycle.** A costs nothing, removes
the discovery hazard, and makes the branch attributable in a log; C is the right
end state but should not be written by someone inferring the sentinel contract
from the source, which is all I did here.

**Explicitly not recommended:** widening this to other owner sources. Whatever is
decided, `indeterminate` must keep refusing on both paths.
