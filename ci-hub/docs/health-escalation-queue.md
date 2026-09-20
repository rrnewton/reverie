# Health escalation queue v1

New queue entries are immutable `event-*.json` envelopes. The producer records
the event ID, enqueue timestamp, origin host, host-scoped sink key, kind, exact
payload SHA-256, normalized SHA-256, and normalization policy before launching
the worker. Both sinks consume that identity; neither reconstructs the host or
key at drain time.

Payload-only `event-*.alarm` and `event-*.resolve` files are legacy evidence.
`run-health-escalation` refuses the entire queue while any such file exists. It
does not guess their origin host, convert them, move them, or publish newer
events past them. A present `.done` sidecar proves only that the named sink was
acknowledged; an absent sidecar is **uncertain**, not proof of non-delivery.

`recover-health-escalation-legacy` is the archive-only recovery path. It never
invokes a publisher, creates an acknowledgement, enqueues a replacement, or
changes a remote object. Its deterministic plan records every queue filename,
size, mode, source timestamps and SHA-256; alarm/resolve kind and ordinal;
line/finding count and the legacy normalized digest; each sink's exact sidecar
evidence; unknown origin-host attribution; anomalies; and the disposition of
every file. Hidden `.event-*` producer remnants are explicit unknown-attribution
anomalies. Malformed schema-v1 JSON is a refusal, not something this legacy tool
hides in an archive.

The legacy delivery log can be supplied as immutable-prefix evidence. Its full
reviewed prefix is size/SHA-bound, and matching attempt lines carry their byte
offset, length, SHA-256, return code, outcome, digest, full issue URL or
TaskGraph row when present. A later append is allowed; mutation or truncation of
the reviewed prefix is refused. Apply revalidates that prefix after creating the
sentinel and again immediately before releasing the queue. The exact reviewed
prefix is itself copied into the archive.

## Plan, review, then apply

The queue must already contain `.worker.lock`. `plan` takes that lock without
changing the queue and writes the manifest outside it. The output must not be a
direct, hard-link, or symlink alias of any source log:

```text
ci-hub/bin/recover-health-escalation-legacy plan \
  --queue ignored/ci-hub/health-escalation-pending \
  --source-log "$HOME/.local/state/hermit-health-escalation.log" \
  --output "$HOME/.local/state/health-escalation-legacy-manifest.json"
sha256sum "$HOME/.local/state/health-escalation-legacy-manifest.json"
```

Review the canonical manifest itself and independently compare the digest with
the command's `manifest_sha256`. In particular, review population counts,
sidecar states, all anomalies, source-log correlations, and that every origin
host remains `null`/`unknown` unless a separate evidence process has proved it.
This version records unknown attribution only for
`archive-preserve-no-replay`; it cannot use that uncertainty to publish.

Apply requires the reviewed digest explicitly and a same-filesystem archive
outside the queue:

```text
ci-hub/bin/recover-health-escalation-legacy apply \
  --queue ignored/ci-hub/health-escalation-pending \
  --manifest "$HOME/.local/state/health-escalation-legacy-manifest.json" \
  --expected-manifest-sha256 REVIEWED_64_HEX_DIGEST \
  --archive-root ignored/ci-hub/health-escalation-legacy-archive
```

Do not apply with an older installed producer. First require the installed
health tick and enqueue tool to match a landed revision that both stages outside
the queue and takes `.worker.lock`; otherwise an old producer can ignore the
recovery barrier. Planning remains safe before that cutover because it writes
nothing in the queue.

Under `.worker.lock`, apply reconstructs the complete canonical manifest and
requires byte identity. An added, removed, renamed, rewritten, remoded, or
retimestamped payload/sidecar is a refusal. The reviewed source-log prefix must
also still hash identically. Enqueue now takes the same population lock, and
the health tick stages pre-envelope input in
`ignored/ci-hub/health-escalation-staging`, outside the queue.

Before moving an original, apply writes and fsyncs an exact private snapshot.
It then atomically renames each reviewed legacy member into
`ARCHIVE_ROOT/MANIFEST_SHA/evidence/`, verifies both the moved original and the
snapshot, and publishes the manifest-addressed archive. A generated
`event-legacy-recovery-*.alarm` sentinel keeps both the worker and schema-v1
enqueue refusing the queue for the whole multi-file transaction. The sentinel
is moved into the completed archive only after every reviewed byte and the
archive-ready receipt have been read back. Thus schema-v1 events cannot
overtake or make an archive-published crash impossible to finalize.

The staging directory entry is fsynced before the sentinel becomes durable;
the final archive rename is fsynced before release; and the final `control/`
entry is fsynced before the sentinel is moved out of the queue. Idempotent
completion requires exact transaction, archive-ready, release, and preserved
sentinel records. Missing, corrupt, symlinked, or extra audit records are a
refusal rather than something retry silently reconstructs. Every
cross-directory rename fsyncs the destination before the source; rollback does
the same for mutant, restored, and control evidence before publishing its
`.failed` audit directory. A final archive coexisting with `.SHA.applying` is
also refused as an impossible/corrupt transaction state.

An in-process failure restores exact reviewed bytes, preserves any raced mutant
separately, and retains a `MANIFEST_SHA.failed.*` audit directory. A crash before
archive publication leaves the legacy sentinel and `.MANIFEST_SHA.applying`
transaction in place, so delivery remains blocked for evidence-led recovery. A
crash after archive publication is idempotently finalized by rerunning the same
`apply` with the same digest. Never remove a sentinel or transaction directory
merely to make the worker run.

The archive contains moved originals, exact snapshots, the reviewed manifest,
the exact reviewed prefixes of supplied source logs,
transaction/readiness/release receipts, and the moved sentinel. No payload or
acknowledgement is deleted. The snapshots make byte-exact rollback possible,
but restoring legacy blockers is a separate operator decision; it is not a
replay authorization.

## No automatic summary or backfill

Legacy filenames do not bind an origin host, and the historical fixed key
`operational-tick-findings` was shared across hosts. A legacy resolve could
therefore close another host's alarm. Recovery does not convert, chunk,
republish, summarize, resolve, or infer a host from payload prose, the current
hostname, or a historical log title. Even oversized payloads are moved
byte-for-byte rather than placed in argv, a TaskGraph note, or one GitHub body.

Whether to publish a historical summary remains an owner decision. It requires
separate proof of both the relevant host and current state. The safe new-stream
boundary is a fresh schema-v1 health tick, not a repackaged legacy payload:

```text
ci-hub/bin/enqueue-health-escalation --queue QUEUE --kind alarm \
  --payload-file CURRENT_FINDINGS --origin-host CURRENT_VERIFIED_HOST --findings COUNT
```

Use `--kind resolve --findings 0` only with a verified empty **current** payload.
The recovery tool never runs this command.

A publisher exit status of 78 creates an immutable per-event, per-sink circuit
record instead of a `.done` acknowledgement. Other sinks continue, but that
sink does not advance or reinvoke the same event with the same publisher. Use:

```text
ci-hub/bin/health-escalation-circuit show --event EVENT.json --sink SINK
ci-hub/bin/health-escalation-circuit clear --event EVENT.json --sink SINK \
  --expected-circuit-sha256 SHA_FROM_SHOW
```

`clear` takes the queue lock, compares the complete current circuit record and
event content, and removes only that circuit. It creates no acknowledgement and
does not invoke a publisher. A later ordinary worker run is the explicit retry.
Do not clear a circuit until its recorded permanent condition has been repaired.
