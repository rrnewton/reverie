# reverie-ptrace

The reference Linux `ptrace` execution backend for
[Reverie](https://docs.rs/reverie-core), the instrumentation framework beneath
[Hermit](https://hermetic-infra.org). It launches and supervises guest process
trees, intercepts selected syscalls and signals, and runs a Reverie `Tool`.
Hermit's command-line interface is packaged as
[`hermit-run`](https://crates.io/crates/hermit-run).

## Build requirements

The default features build on stable Rust. On Ubuntu 24.04, install the native
compiler and libunwind development dependencies first:

```sh
sudo apt-get install -y build-essential pkg-config libunwind-dev liblzma-dev
```

```toml
[dependencies]
reverie-ptrace = "0.4"
```

The build uses the system `libunwind-ptrace` and discovers its private link
dependencies through pkg-config. No extra LZMA linker flag is needed.
Guest launch also requires permission to use ptrace and any namespaces
requested by the command. See the
[`hermit-run` installation guide](https://crates.io/crates/hermit-run) for its
kernel and Ubuntu AppArmor requirements.

## Kernel compatibility and fatal cleanup

The ordinary ptrace engine explicitly selects safeptrace's
`*_on_ptracer_thread` SDK mode. This permits retained-descriptor tracing and
process/thread lifecycle notification on Linux 6.8. Native thread pidfds are
used from Linux 6.9. Safeptrace's generic sibling-pollable SDK interfaces retain
their native `PIDFD_THREAD` requirement; they do not implicitly select the
owner-thread fallback. Optional LiteInst cleanup also retains its native
thread-pidfd requirement.

On the legacy ordinary path, Reverie's fatal-tree cleanup requests SIGSTOP
through already captured regular group pidfds, relays actual signal-delivery
stops, and holds every owned task at a genuine stop or observes its actual
terminal state before killing the tree. Coalesced signals and successful
requests are never treated as stop acknowledgments. Existing exit hooks and
terminal waits still finish through their original owners.

Legacy nonleader observations wake the existing ptracer task, which consumes
reports on the actual ptrace-owning OS thread. Retained target and host-owner
directories authenticate that wait authority independently of signal
permissions. Background hints never substitute for consumed stop or terminal
reports, including when a host ptracer thread exits or reattaches.
The local SDK wrappers are `!Send` and `!Sync`; the backend retains their
original authority in Send, non-Future drivers and explicitly polls them on
the owning ptracer thread. Guest trait and callback futures keep their existing
Send contracts. A refused or cancelled adapter retains its driver in the
enclosing task's cleanup owner.

This cleanup barrier proves physical stops of the owned tasks; it does not
claim job-control completion for unowned members. Standalone safeptrace
`TerminalCleanup::request_sigstop` still requires a native thread pidfd for
exact thread delivery: live legacy handles return `EOPNOTSUPP`, and dead
retained identities return `ESRCH`. Native API errors and signal metadata
remain unchanged.

## Function guests in tests

`TracerBuilder` launches ordinary subprocesses and works on stable Rust.
`spawn_fn` and the `testing::test_fn` helpers also work on stable Rust, but
libtest normally captures printing macros before they reach stdout/stderr.
To collect `println!` and `eprintln!` from a forked function guest, run stable
tests with `--nocapture`, or use nightly Rust and explicitly enable the
off-by-default `nightly` feature:

```sh
cargo +stable test -- --nocapture
cargo +nightly test --features reverie-ptrace/nightly
```

Direct `std::io::Write` calls to `std::io::stdout()` and `std::io::stderr()`
bypass libtest's printing-macro capture and are captured by the backend on
stable Rust. This limitation concerns function guests inside libtest; it does
not affect subprocess output capture.

See the [API documentation](https://docs.rs/reverie-ptrace) for launch, event,
and cleanup contracts. For background, read
[Hermit: Deterministic Linux for Controlled Testing and Software Bug-finding](https://developers.facebook.com/blog/post/2022/11/22/hermit-deterministic-linux-testing/).
