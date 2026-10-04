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
