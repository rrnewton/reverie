# Reverie DBT FINAL-frame diagnosis

Date: 2026-08-17

Review targets:

- Reverie main: `af82f1b92a2762840dc5894984be5e1f5bfc5cb9`
- Existing FINAL-frame commit: `9aed97dc00e5f813c33a300abd39264c7dbfc12e`
- Hermit failing composition: `fb87e3013bab805e80c665913af62462b1f2eb1e`
- Existing Hermit rehearsal pinned to `9aed97dc`: `743368a5b406822b7e59b368ed2acd0e2f5cdbdd`

No Reverie or Hermit source was edited for this diagnosis.

## Conclusion

The `/bin/true` failure at Hermit `fb87e301` is caused by one missing sender
call, not by the reader discarding a FINAL frame and not by a missing evidence
protocol.

`9aed97dc` is a real, implemented, tested commit. It adds the missing
`require_evidence_flush(EVIDENCE_FRAME_FINAL)` call and strengthens an existing
source-order test. The commit is not on Reverie main, but it is preserved on
remote branches. The dedicated branch currently points at descendant
`c8d7d986b6dc0d19ff4f3f33ed51fcbe7fd0f3a6`.

The patch from `9aed97dc` applies cleanly to current Reverie main `af82f1b9`:

```text
git diff-tree --no-commit-id -p 9aed97dc | git apply --check -
exit 0
```

For the simple process lifecycle exercised by `/bin/true`, this is a targeted
fix in existing infrastructure: seven production lines in
`reverie-dbt/native/client.c` plus a test update. It does not add a frame type,
change framing, or relax the collector.

The later work on the same branch is broader. In particular, `c8d7d986` adds
`EVIDENCE_FRAME_CHILD_FINAL` for a parent to confirm a reaped SIGKILL child.
That is a protocol addition and is separate from the one-call defect proven
here. The existing sequence is:

1. `9aed97dc`: finalize the distinct background scheduler process.
2. `9f5e0d9e`: stop and finalize scheduler processes across exec, then restart
   after failed exec.
3. `476f4277`: virtualize process-group identity exposed once exec evidence
   became comparable; this is not a FINAL-frame transport change.
4. `c8d7d986`: add authenticated completion for reaped children, including the
   new `EVIDENCE_FRAME_CHILD_FINAL` frame.

## Sender path: the frame is not emitted on `af82f1b9`

The protocol and transport already exist on `af82f1b9`:

- `reverie-dbt/native/client.c:356-362` defines `EVIDENCE_FRAME_FINAL` as frame
  kind 5.
- `reverie-dbt/native/client.c:512-574` sends a frame and waits for a one-byte
  collector acknowledgement, retrying an identical frame if the ACK is lost.
- `reverie-dbt/native/client.c:669-703` flushes buffered DATA, sends the
  requested terminal frame, and marks the sender finalized only after the send
  succeeds.
- `reverie-dbt/native/client.c:737-742` makes a failed flush fatal with
  `protected evidence transport failed`.

The missing path is `reverie-dbt/native/client.c:3856-3862` at `af82f1b9`:

```c
static void runtime_background_init(void *argument) {
  (void)argument;
  atomic_store_explicit(&runtime_background_state, 2, memory_order_release);
  evidence_callback_enter();
  reverie_dbt_runtime_background_init_v2(&runtime_callbacks_page.value);
  evidence_callback_leave();
  atomic_store_explicit(&runtime_background_state, 3, memory_order_release);
}
```

The external scheduler client thread is implemented as a distinct process. Its
records are admitted under that process's `SO_PEERCRED` identity, but returning
from this function runs neither application thread teardown nor `event_exit`.
The process therefore emits START and DATA frames and exits without invoking
the existing FINAL sender.

`9aed97dc` changes only that missing lifecycle edge at
`reverie-dbt/native/client.c:3862-3869`:

```c
  evidence_callback_leave();
  // ... no application thread-exit or process-exit callback runs ...
  require_evidence_flush(EVIDENCE_FRAME_FINAL);
  atomic_store_explicit(&runtime_background_state, 3, memory_order_release);
```

The ordering matters: scheduler emission finishes, FINAL is acknowledged, and
only then is the background process published as quiescent.

## Reader path: it retains received frames and correctly refuses incompleteness

The reader is not dropping a received FINAL:

- `reverie-dbt/src/evidence.rs:787-797` verifies the frame, applies it to the
  collector, and only then sends the success ACK. The source comment states
  that the frame is durable in collector state before the ACK.
- `reverie-dbt/src/evidence.rs:507-517` handles FINAL by validating its sequence
  and removing that exact process image from `images`.
- `reverie-dbt/src/evidence.rs:653-664` refuses publication if any process image
  remains without FINAL.

If the background process had called the sender and the transport had failed,
the native client would have exited through `protected evidence transport
failed`. If the collector had received and acknowledged FINAL, the image would
have been removed. The observed status-0 guest followed by a retained image is
the outcome of the absent call site.

The values `next_sequence: 9` and `next_sequence: 1492` in the two failed runs
are evidence-frame sequence positions, not INFO-message counts.

## Live before/after bracket

### Before: current composition pinned to `af82f1b9`

Runnable command:

```bash
env -u HERMIT_INSTALL_DIR \
  timeout --signal=TERM --kill-after=10s 120s \
  /home/newton/work/dev-hermit/ignored/dbt-verdict-compose.JrTHWz/target/release/hermit \
  --log info run --backend dbt --strict --verify --verify-strict \
  --verify-json /tmp/af82-true.json -- /bin/true
```

Observed output on a clean retry:

```text
:: DBT Run1...
Error: ... DBT guest exited with status Some(0) while protected evidence failed:
DBT evidence is missing FINAL frames for process images
{ProcessKey { ... }: ImageState { epoch: 0, next_sequence: 9, pending_exec: false }}
```

Exit status was 1. The JSON remained `verdict=no_result`, `verified=false`, and
`comparison=null`. Run2 was not launched.

### After: existing rehearsal pinned exactly to `9aed97dc`

The command below used the clean disposable Hermit checkout at existing commit
`743368a5`; its `Cargo.lock` pins Reverie exactly to `9aed97dc`.

Build commands:

```bash
env -u HERMIT_INSTALL_DIR \
  CARGO_TARGET_DIR=/tmp/reverie-final-frame-9aed-target \
  CARGO_NET_OFFLINE=true CARGO_INCREMENTAL=0 \
  cargo build --locked -j 1 -p detcore-dbt

env -u HERMIT_INSTALL_DIR \
  CARGO_TARGET_DIR=/tmp/reverie-final-frame-9aed-target \
  CARGO_NET_OFFLINE=true CARGO_INCREMENTAL=0 \
  cargo build --locked -j 1 -p hermit --features dbt --bin hermit
```

Run command:

```bash
env -u HERMIT_INSTALL_DIR \
  timeout --signal=TERM --kill-after=10s 120s \
  /tmp/reverie-final-frame-9aed-target/debug/hermit \
  --log info run --backend dbt --strict --verify --verify-strict \
  --verify-json /tmp/reverie-final-frame-9aed-true.json -- /bin/true
```

Observed output:

```text
hermit 0.2.0 (2026-08-18, g743368a5b406)
:: DBT Run1...
:: DBT Run2...
:: Comparing captured verification logs...
Logs contain 81 | 81 INFO messages
Done processing logs, no substantive differences found (81 | 81 INFO messages compared).
```

Exit status was 0. The JSON reported `verdict=matched`, `verified=true`,
canonical strictness, `compare_logs=true`, full trace, exact remainder, and
guest exit code 0.

This live bracket changes the pinned Reverie lifecycle implementation while
retaining the fail-closed collector. It demonstrates that the missing call is
sufficient for the simple background-process lifecycle.

## Reachability and recorded validation

`9aed97dc` is reachable from these remote branches:

```text
origin/sanity-driver/finalize-dbt-background-evidence
origin/sanity-driver/dbt-combined-measurement
origin/sanity-driver/qemu-tcg-preemption-safe
```

The commit records these validations:

```text
cargo fmt --all -- --check
cargo check -p reverie-dbt --all-targets
cargo clippy -p reverie-dbt --all-targets -- -D warnings
cargo test -p reverie-dbt --lib: 75 passed, 6 ignored
live protected-evidence guards: 6/6 passed
evidence_fork_live: 3/3 passed
```

Those are commit-recorded results, not rerun in this diagnosis. The live
`/bin/true` control above was rerun here. The exact runtime-library build took
3m49s; the matching CLI retry completed in 27.55s after one unrelated host BPF
`FILE_OPEN` denial.

## Limits and landing state

- This diagnosis proves `9aed97dc` for the simple background scheduler process.
  It does not substitute for the later exec and reaped-child regressions.
- Reverie main `af82f1b9` does not contain `9aed97dc`; their common base is the
  protected-evidence commit `268a25b6`.
- The production patch applies cleanly to `af82f1b9`, but no replay or source
  edit was made here.
- Reverie landing remains blocked by the root skill reset described by the
  owner. Nothing was prepared or landed.
- Root `AGENTS.md` references
  `.skills/supplemental_docs/coordinator-codex.md`, which is absent in the
  current parent checkout; the available copy is under the in-progress skill
  reset tree. This did not affect the source diagnosis.
