## Independent review — agent-utils `2912e109` (Python tick-hub engine)

Reviewed by **executing** at `2912e109` in a scratch clone. **Behaviour: approve.** One
disclosure gap worth recording before a pin decision.

### Mutation controls re-derived, not accepted

Applied each mutation myself and ran the suite:

| mutation | observed | claimed |
| --- | --- | --- |
| delete the exit-75 pre-check | 4 failed, 18 passed | 4 |
| reserve code 1 instead of 75 | 3 failed, 19 passed | 3 |
| consume cadence on NO_RESULT | 1 failed, 21 passed | 1 |
| drop the counted ACTION line | 1 failed, 21 passed | 1 |

4 of 4 killed. Baseline **1524 passed**, matching the stated figure.

### Exit-code widening: does not exist

The comparison is `result.returncode == NO_RESULT_EXIT` — exact equality, and the only such
comparison in `_eval_gate`. Swept the code space through the real classifier under **both**
`when` polarities:

```
   0 -> quiet / fire
   1,2,3,74,76,124,127,255 -> fire / quiet   (unchanged)
  75 -> no-result / no-result                 <- reserved
```

Neighbours 74 and 76 are untouched, and 75 resolves identically under both polarities — which
confirms the pre-`when` placement by observation rather than by reading the comment.

### The `render_emit` bypass is correct

`render_emit` merges captured over static fields, interpolates, **raises** on leftover
placeholders, and formats via the reminder's own title/skill. The NO_RESULT path skips
interpolation and the domain title — right, because claiming the domain verdict is exactly what
a gate that could not determine must not do — while `_no_signal_action` **preserves routing**
by reusing `reminder.emit.skill`. The counted ACTION is not skipped; only the parts that would
be false are. The path cannot raise: `detail` is a plain dict lookup, never interpolated.

### One candidate finding dropped on measurement

`format_no_result` neither normalises nor caps its detail while `_no_signal_action` does
(`" ".join(split())[:240]`), so I suspected a captured summary could inject a newline and split
one record in two. It cannot — `parse_kv_lines` iterates `splitlines()` and strips each line, so
a captured value can never contain a newline. Unreachable; not reported as a finding.

### The parity question: this port does add a divergence

The Rust NO_RESULT block pushes **only** the diagnostic line and continues. The Python block
emits **both** the diagnostic **and** a counted NO-SIGNAL ACTION, and increments `actions`.
`rs/tick-hub/src/engine.rs` has no `no_signal` helper at all, while it does maintain an actions
counter that NO_RESULT never touches. So the pair now differs in two observable ways on the same
input: whether an ACTION-only consumer sees anything, and the reported action count.

**The Python behaviour is the better one** and I am not asking for its removal — ACTION-only
consumers seeing nothing is precisely the failure being removed, and the Python engine is the
one that runs. The gap is disclosure: the message says "Ports 65e8a52d from the Rust engine to
the Python one", which reads as parity while it extends.

**Scope, stated precisely rather than alarmingly:** exit-75 was itself one of the three known
divergences and this port *closes* it. The count moves 3 → 3 — a substitution, not an
accumulation.

Read-only; I did not move the pin and make no recommendation on it.
