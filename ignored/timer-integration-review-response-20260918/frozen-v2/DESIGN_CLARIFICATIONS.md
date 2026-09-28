# Successor interface and ownership clarification

This supplements frozen-v1's clarification and approved design v2 without replacing either document. It is an author clarification, not independent approval.

`PreparedSignalToken { site: CallbackSignalSite, selection_nonce }` and `ParkedSignalFailureContext { site: CallbackSignalSite, ledger_nonce }` carry the original complete lifetime/boundary. Callers retain and compare the full site. Nonces are local, not globally unique by themselves. In the backend diagnostic `Error::SignalEffects`, the context is now boxed at ledger creation and moved on failure; production Hermit neither constructs nor dereferences that backend diagnostic context.

An observation lease has one admitted use in an original callback. A new periodic observation needs a fresh lease; the old completed identity cannot consume another signal. Uniqueness does not require numerically increasing nonces. The original callback remains available to genuine structured-hook RPCs; observing is not itself a reason for the site getter to return None.

The process dequeue journal has one global sequence and full private owner stamps. Only the removing Guest can announce an entry. A later owner suspends without holding a backend lock until the prior owner acknowledges. Wrong-owner acknowledgments and stale earlier records fail. The last exact owner/record acknowledgment is idempotent. This preserves Hermit's existing contiguous process sequence protocol, including its sender-TID/MmId/startup-identity validation.

Failure transfers only caller-owned committed effects. It marks the notification stream terminal, wakes waiting owners into terminal cleanup, and never sends a later ordinary notification across a failed predecessor. This is not rollback and does not invent a guest syscall outcome. Already committed owner stamps remain valid for consuming cleanup after task retirement; they do not authorize new removal from a reused TID.

Bookkeeping reservation for an ordinary injection happens at the fallible KvmGuest boundary; failure enters the private terminal driver path with prior effects. The unsubscribed production executor path performs the same fallible preflight. It cannot turn getpid into a made-up EOVERFLOW/ENOMEM result.

The protected caught-selection action cannot be changed by posthook rt_sigaction writes. Queries, writes for unrelated signals, preselection action changes, and unrelated returning injections remain available. No scheduler ordering, timer phase, virtual-time, or replay representation is changed by these component corrections.
