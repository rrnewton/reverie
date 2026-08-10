---
name: dbi-no-runtime-tool-selection
description: "Historical 2026-07-22 snapshot of the DBT (formerly DBI) backend's Guest-trait/dispatch behavior; verify every claim against current reverie-dbt source before relying on it."
---

# Historical DBT (formerly DBI) tool-loading snapshot — 2026-07-22

This records the reverie-dbt (DynamoRIO) backend's tool-loading and
Guest-trait-dispatch behavior as it stood on 2026-07-22, right after the
Guest-trait audit (tasks dbi-m1-guest-trait-audit, impl-dbi-simple-tools; full
audit `ai_docs/transient/20260722_dbi-guest-trait-audit.md`). The backend was
renamed **DBI -> DBT** on 2026-08-06/08 (hermit `e565b1ab`, reverie
`752f06cd`, both verified ancestors of current `origin/main` as of
2026-08-10), and the loading architecture has moved on since this snapshot —
do not treat the "one client.so with a statically compiled tool" picture below
as current without reconfirming it. In particular:

- **Verified NOW SPLIT into two libraries, not one.** Hermit's
  `detcore-dbt/src/lib.rs` resolves a separate **runtime** library
  (`libdetcore_dbt.so`) via `hermit_resources::resource()` or beside the
  running Hermit binary (`runtime_library_path()`), then builds the
  DynamoRIO **native client** (`libreverie_dbt_client.so`) against that
  runtime via `cmake -DREVERIE_DBT_RUNTIME=<runtime>` (`prepare_native_client()`),
  or uses a pre-packaged client from `hermit_resources::install_dir()`. This
  is a materially different lifecycle from "the client just has a tool
  statically linked in."
- **Verified `PROTOTYPE_TOOL` is a Cargo feature default, not an
  unconditional fact.** In `reverie-dbt/Cargo.toml`, `default =
  ["prototype-runtime"]` and `PROTOTYPE_TOOL`/`GLOBAL_STATE`/etc. in
  `reverie-dbt/src/lib.rs` are all gated `#[cfg(feature = "prototype-runtime")]`.
  It ships on by default today, but it is a feature flag an agent could
  disable or a future default could flip — cite the feature, not "the tool is
  compiled in" as a bare fact.
- **Not reconfirmed for this pass:** whether `has_cpuid_interception()` /
  `handle_cpuid_event` still route the way described below —
  `has_cpuid_interception` no longer appears in `reverie-dbt/src/lib.rs` at
  all (it is implemented for `reverie-liteinst` and `reverie-ptrace`; the DBT
  Guest may now inherit a `Guest` trait default). Re-derive this from current
  source rather than trusting the paragraph below.
- **Reconfirmed 2026-08-10 and still accurate:** `set_timer`/`set_timer_precise`
  still return `ENOSYS` with the same "no RCB-threshold trap in the native
  client yet, TODO-STUB(#31)" rationale; `read_clock` still returns the
  syscall-entry-sampled branch count; `Tool::handle_timer_event` is still not
  dispatched (same TODO-STUB(#31) comment site in current `reverie-dbt/src/lib.rs`).

## The 2026-07-22 snapshot (dated; re-verify anything not called out above)

The reverie-dbt (DynamoRIO) backend had **no runtime tool selection**. The
native client (`libreverie_dbt_client.so`) linked a `static PROTOTYPE_TOOL`
compiled in; `DbtRunner`/`hermit run --backend dbt` just shelled
`drrun -c client.so`. Running any other Reverie `Tool` meant editing the
client's Rust and rebuilding the `.so` (cross-repo: reverie-dbt is a pinned
git dep).

The driver dispatched **only** `Tool::handle_syscall_event` (via the native
`pre_syscall` event). NOT dispatched at that time: `subscriptions` (ignored;
all syscalls filtered), `handle_cpuid_event` (CPUID emulated in C, bypassing
Rust; `has_cpuid_interception()` returned false), `handle_rdtsc_event`,
`handle_signal_event`, `handle_timer_event` (still not dispatched as of
2026-08-10, see above), thread/exec/exit lifecycle.

The `DbtGuest<T>` handle itself was generic and real for syscall-driven
tools: `memory` (in-process LocalMemory), `tid`/`pid`, `regs`, `inject`,
`thread_state` worked. As of DBI-M2 (PR rrnewton/reverie#32, 2026-07-22)
`stack` and `tail_inject` were ALSO implemented: `stack` = in-process heap
arena `DbtStack` (shared address space, addrs valid immediately);
`tail_inject` = inject + stash-result-in-thread-local + suspend
(`future::pending().await`), driver installs the result on `Poll::Pending`
and drops the future. Still stubbed at that time (TODO-STUB(#31)):
`set_timer*` (ENOSYS), `ppid` (always None — correct for single-proc root),
`read_clock` (branch count sampled at syscall entry).

**Why:** established by the Guest-trait audit + building working
syscall-counter and strace tools on the backend (tasks
dbi-m1-guest-trait-audit, impl-dbi-simple-tools, 2026-07-22, back when it was
still named DBI). Simple syscall-driven tools worked by using `inject` (not
`tail_inject`) and process-global state (not GlobalState RPC, since its
global was `()`).

**How to apply:** for current DBT tool work, edit reverie-dbt and rebuild
the client — but first re-derive the loading path from `detcore-dbt/src/lib.rs`
(`runtime_library_path()`, `prepare_native_client()`) and
`reverie-dbt/Cargo.toml`'s feature set, since those are verified to have moved
past this snapshot. Don't expect reverie-examples tools to run on DBT as-is.
Full 2026-07-22 audit: `ai_docs/transient/20260722_dbi-guest-trait-audit.md`
(written before the rename, uses the old DBI spelling throughout — read it as
a historical source). Move maintained current-architecture guidance into
Reverie's own skill tree rather than expanding this file further.
