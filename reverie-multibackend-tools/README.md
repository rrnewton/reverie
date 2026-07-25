# reverie-multibackend-tools

Same Reverie tool code, one binary per **(tool × backend)**.

This crate is the build infrastructure that proves a single, backend-agnostic
Reverie [`Tool`] implementation can be linked against each backend. The tools
live in their own dependency-light library crates and are written **once**:

| tool crate               | what it does                                    |
| ------------------------ | ----------------------------------------------- |
| `reverie-tool-sysctr`    | counts syscalls, aggregated across the tree     |
| `reverie-tool-riptrace`  | strace-like per-syscall tracer                  |

This crate adds the thin per-backend adapters (`src/lib.rs`) and the binary
entry points (`src/bin/`).

## Binaries

| binary                     | tool     | backend | feature   |
| -------------------------- | -------- | ------- | --------- |
| `reverie-sysctr-ptrace`    | sysctr   | ptrace  | `ptrace`  |
| `reverie-sysctr-kvm`       | sysctr   | kvm     | `kvm`     |
| `reverie-sysctr-dbi`       | sysctr   | dbi     | `dbi`     |
| `reverie-riptrace-ptrace`  | riptrace | ptrace  | `ptrace`  |
| `reverie-riptrace-kvm`     | riptrace | kvm     | `kvm`     |
| `reverie-riptrace-dbi`     | riptrace | dbi     | `dbi`     |

## Building

The backends have very different build requirements, so each is a feature and
each binary declares `required-features`.

```bash
# ptrace + kvm binaries (default features):
cargo build -p reverie-multibackend-tools

# a single binary:
cargo build -p reverie-multibackend-tools --bin reverie-sysctr-ptrace

# the DBI binaries additionally need the DynamoRIO submodule built from source:
scripts/backend-submodule.sh activate dynamorio
cargo build -p reverie-multibackend-tools --features dbi
```

`dbi` is intentionally **not** a default feature: `reverie-dbi`'s build script
requires the `third-party/dynamorio` submodule and performs a from-source CMake
build, so a plain `cargo build` would fail on a checkout that has not activated
it.

## Running

### ptrace (mature; any guest command)

```bash
reverie-sysctr-ptrace   -- /bin/sh -c '/bin/true; /bin/echo hi'
reverie-riptrace-ptrace -- /bin/true
```

### kvm (bounded prototype; a single static ELF)

`argv[0]` must be a loadable ELF; further arguments are passed to the guest.

```bash
reverie-sysctr-kvm   /path/to/program [args...]
reverie-riptrace-kvm /path/to/program [args...]
```

### dbi (DynamoRIO)

The DBI backend has no runtime tool selection: the tool is compiled into a
separately-built DynamoRIO native client (`REVERIE_DBI_CLIENT`, built by
`reverie-dbi/scripts/build-client.sh`), which the launcher loads into the guest.
These binaries link the shared tool against the DBI backend and drive the real
`DbiRunner`; embedding a *specific* tool into a per-tool native client (so the
launcher's tool and the client's baked-in tool are guaranteed to match) is owned
by the DBI native-client work.

```bash
export DYNAMORIO_HOME=/path/to/dynamorio
export REVERIE_DBI_CLIENT=/path/to/client
reverie-sysctr-dbi   /bin/true
reverie-riptrace-dbi /bin/true
```

## Design notes

* The tool crates depend only on `reverie` (the trait crate) — never on a
  specific backend — so they are trivially reusable.
* `sysctr` aggregates **live** (one RPC per syscall) rather than contributing
  totals at process exit, because not every backend drives the exit hooks (the
  KVM static-ELF runner does not). Live counting relies only on
  `handle_syscall_event`, which every backend drives.
* Cross-backend agreement is observable: `reverie-sysctr-{ptrace,kvm}` both
  report the same syscall count for the same static program.

[`Tool`]: https://docs.rs/reverie/latest/reverie/trait.Tool.html
