# Health escalation queue v1

New queue entries are immutable `event-*.json` envelopes. The producer records
the event ID, enqueue timestamp, origin host, host-scoped sink key, kind, exact
payload SHA-256, normalized SHA-256, and normalization policy before launching
the worker. Both sinks consume that identity; neither reconstructs the host or
key at drain time.

Payload-only `event-*.alarm` and `event-*.resolve` files are legacy evidence.
`run-health-escalation` refuses the entire queue while any such file exists. It
does not guess their origin host, convert them, move them, or publish newer
events past them. Recovery therefore starts with a separately reviewed manifest
that binds every legacy filename and file SHA-256 to its known origin host and
audits each existing `.taskgraph.done` and `.github.done` sidecar. There is no
automatic legacy conversion command: an unacknowledged legacy delivery is
uncertain and must not be silently resent. Once that audit decides which events
may be republished, the future-format creation boundary is:

```text
ci-hub/bin/enqueue-health-escalation --queue QUEUE --kind alarm \
  --payload-file AUDITED_COPY --origin-host VERIFIED_HOST --findings COUNT
```

Use `--kind resolve --findings 0` only with a verified empty payload. Archiving
the corresponding legacy evidence is a distinct operator action.

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
