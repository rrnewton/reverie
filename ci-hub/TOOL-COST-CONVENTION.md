# Tool cost awareness

Every substantive user-facing operation owned by dev-hermit must make its time
cost visible. The caller should never discover after launch that a command
needs hours.

## Scope

Cost reporting applies only when an operation performs meaningful work: a
network query, repository scan, build/test, queue wait, durable background
launch, or another action whose wall/CPU cost can affect the caller's plan.
Trivial control paths MUST print no cost lines. This includes `--help`,
`--version`, usage/argument errors rejected before work starts, static text,
and instant status reads of an already-local small state file. Parse and
validate the command first; arm cost reporting only after selecting a
substantive operation. Do not wrap a multi-command front door indiscriminately.

## Required output

1. **Estimate before work.** Print expected wall and CPU time before the first
   expensive or mutating step. Derive it from known parameters and measured
   history: input count, repetitions, probe count, warm/cold cache state, and
   parallelism. Print that basis. If the required measurements do not exist,
   print `unknown` and state what data is missing. A static broad guess is
   prohibited, including as a bootstrap.
2. **Actual on every completion path of substantive work.** Print elapsed wall and total CPU
   (user + system) with the exit status on success, failure, signal, and bounded
   early exit. Preserve the command's exit status.
3. **Every displayed number is accountable.** A number shown to a human must be
   derived from real data with its basis stated (source, window, and sample size
   where applicable), or explicitly labeled `unknown` / `not measured`. Never
   print an invented constant that merely looks like a measurement.

Canonical lines are stable and machine-readable enough for future ingestion:

```text
# <name> tool COST ESTIMATE wall=<seconds>s cpu=<seconds>s basis='<parameters/history>'
# <name> tool COST ESTIMATE wall=unknown cpu=unknown basis='not measured: <missing data>'
# <name> tool COST ACTUAL wall=<seconds>s cpu=<seconds>s cpu_user=<seconds>s cpu_system=<seconds>s exit=<code|signal:N>
```

Every cost line MUST begin with `# ` and MUST name the reporting tool as its
first field (`# <name> tool COST ...`). The two requirements are load-bearing,
not cosmetic:

- The `# ` comment-like prefix makes the line unmistakably **meta-output** — the
  price of running the *reporting* tool — rather than a value belonging to the
  thing being reported on. A bare `COST: 3.2s` is a number with an unstated
  subject, the same class of error as reading a process count as load. The
  prefix also makes cost lines trivially greppable and filterable.
- Naming the tool keeps the subject unambiguous even when several tools'
  output is interleaved in one CI log — the normal case, not the edge case.

Write estimates and actuals to stderr so structured stdout remains usable. A
tool may print richer detail, but it must retain these fields and meanings.

## Shared helper

Wrap commands with `ci-hub/bin/tool-cost`; it prints the estimate before launch,
uses `wait4` to measure the child process tree, forwards common termination
signals, reports actual wall/CPU, and returns the child's exit code.

```bash
ci-hub/bin/tool-cost \
  --tool multisect/search \
  --estimate-wall-seconds 7200 \
  --estimate-cpu-seconds 21600 \
  --basis 'derived from 12 probes x 3 reps x measured p50 600s/probe (last 8 warm probes) / parallelism 3' \
  --actual-json ignored/ci-hub/multisect-cost.json \
  -- ./multisect ...
```

When no defensible estimate exists, use the explicit unknown path; do not put a
timeout, upper bound, or plausible constant into an estimate field:

```bash
ci-hub/bin/tool-cost \
  --tool ci-hub/example \
  --estimate-unknown \
  --basis 'not measured: no retained runs for this operation' \
  -- ./example ...
```

`--actual-json` is optional. When supplied, the helper atomically writes schema
v1 containing the same estimate plus actual wall, total/user/system CPU, and
exit fields. Long-lived stores should ingest or reference that JSON instead of
parsing human-facing log lines. `estimate.kind` is `derived` or `unknown`;
unknown estimates store JSON `null` for wall and CPU seconds.

For a self-contained product tool that cannot depend on the parent checkout,
keep its calculation local but use the same output contract. Parent launchers
should still use the shared wrapper rather than adding another timer.

## Estimation rules

- **Parallel work:** wall estimate is critical-path work divided by effective
  parallelism plus setup/queue overhead; CPU estimate is summed work and is not
  divided by parallelism.
- **History-backed work:** state the sample/window and warm-vs-cold class. Fall
  back to `unknown`, not a conservative-looking number, when history is absent.
- **Network work:** derive queue/API retry allowance from measured history when
  available. A configured timeout or retry cap is a bound, not an estimate; label
  it as a bound and keep the estimate unknown when no runtime data exists.
- **Early rejection after work begins:** estimate first when parameters are
  valid enough to size substantive work; otherwise report an unknown estimate
  and the measured actual. Argument parsing, help, version, and usage errors
  happen before cost reporting and print no cost lines.
- **Nested tools:** report the outer operation once. Set
  `CI_HUB_TOOL_COST_ACTIVE=1` when an already-timed parent invokes another
  wrapped ci-hub entrypoint.

## The `ci-hub` front door's own cost, so it is not rediscovered again

⚠️ **THIS COST HAS BEEN INDEPENDENTLY REDISCOVERED SIX TIMES AND WORKED AROUND
LOCALLY EACH TIME** — `health/pr_status.py`, `health/tick-hub.yaml`,
`landing/main_soft_green.py`, `tests/test_operational_bounds.py`, `health/README.md`,
and the `validate-run` branch of the `ci-hub` shim itself. Every workaround was
correct where it sat and left the next reader to rediscover the cause from
scratch. The sixth finder had no way to know they were the sixth. The reason is
written at the shebang in `ci-hub.rs`, which is not where anyone hits the cost;
it is written here because callers hit it from the calling side.

**`--force` is load-bearing and must not be removed.** Measured 2026-08-27 with a
two-file reproduction: a `#[path]` module edited between runs is **ignored**
without `--force` — the stale cached binary runs and prints the old value — and
picked up with it. rust-script keys its cache on the top-level file only. Dropping
the flag reintroduces a stale binary reporting success, which this project has
already been bitten by.

**The cost is bimodal, and the warm case is not what the workarounds assume.**
Measured 2026-08-27 on devbig014, rust-script 0.36.0:

```text
  warm dispatch (--help, cache present)     0.05-0.06s wall   0.02-0.03s user
  cold cache (fresh XDG_CACHE_HOME)        16.36s     wall  69.13s     user
```

⚠️ **WARM DISPATCH IS ~0.03s OF CPU AND IS NOT WORTH BYPASSING.** An earlier
measurement recorded "29 cached runs at 1.07–2.76s wall, 0.75–0.82s user" and
attributed that to `--force`. Re-measured against the same command, the split is:

```text
  ci-hub validate-status --sha SHA --repo rrnewton/hermit --json   1.12-1.20s user
  ci-hub --help  (dispatch alone, same warm cache)                 0.03s      user
```

So roughly **2.6% of that figure is the front door and ~97% is the command's own
work** — reading and parsing the ledger. A bypass built to avoid the front door
saves the 2.6%; if the caller is slow, the ledger read is where to look.

**What actually costs is a COLD cache, at 69s of CPU.** Reach for a bypass only
when a caller genuinely faces one. Two known ways to be cold:

- `ci-hub validate-run` creates a fresh `mktemp -d` cache on **every** invocation
  (see the `ci-hub` shim), deliberately, so a validate launch cannot reach the
  user's shared rust-script cache before the run-owned one is installed. It
  therefore pays the cold cost every time, by construction.
- Any caller with a different or evicted `XDG_CACHE_HOME`, and the machine-wide
  Cargo build lock that a concurrent rebuild holds.

**Before adding a seventh bypass, measure which case you are in.** `ci-hub --help`
against your own environment separates them in one command: ~0.05s means your
cache is warm and the front door is not your cost; tens of seconds means it is
cold and the bypass may be justified — say which in the comment, with the number.

## Numeric claims and ratchets

- Counts, percentages, and progress indicators must be computed from the data
  actually inspected. State the denominator or sample size and the data window;
  progress must reflect completed work, not a theatrical counter.
- Configuration values, timeouts, resource limits, and decision thresholds are
  legitimate constants only when labeled as policy, limits, bounds, or
  thresholds. Do not describe them as observed values or estimates.
- Asserted ratchets are legitimate when their provenance is recorded beside the
  declaration and the code verifies the count. For example,
  `RR_COMPAT_EXPECTED=139` records the establishing PRs, measured exclusions,
  and the exact passing-label set in `hermit/scripts/validate.rs`. That is an intentional
  compatibility floor, not a fabricated measurement. A ratchet without recorded
  provenance is an audit failure even when its current value happens to be right.
- A summary must not imply that work was measured if a probe did not run. Emit
  `unknown`, `not measured`, or an explicit unavailable result instead.

The current compliance inventory and follow-ups are in
[`TOOL-COST-AUDIT.md`](TOOL-COST-AUDIT.md).
