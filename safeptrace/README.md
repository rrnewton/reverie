# safeptrace

Safe Rust wrappers around the Linux `ptrace` API, used by
[Reverie](https://docs.rs/reverie-core), the instrumentation framework beneath
[Hermit](https://hermetic-infra.org).

The state types distinguish a running tracee from a stopped tracee, helping
callers use ptrace operations only when they are valid. This is a low-level
interface; callers still own process lifetime and tracing policy.

```toml
[dependencies]
safeptrace = "0.4"
```

## Optional features

- `memory`: guest-memory access through `reverie-memory`. Memory access requires
  a stopped tracee.
- `notifier`: asynchronous ptrace event notification for runtimes such as Tokio.

Both features are off by default. For asynchronous tracing with memory access:

```toml
[dependencies]
safeptrace = { version = "0.4", features = ["memory", "notifier"] }
```

See the [API documentation](https://docs.rs/safeptrace) for the state machine
and wait contracts. For a complete execution engine and its platform setup,
use [`hermit-run`](https://crates.io/crates/hermit-run).
