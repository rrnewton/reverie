# riptrace

`riptrace` is a runnable demonstration of the experimental `reverie-sabre`
backend. The host executable starts an RPC service and launches the guest
through SaBRe. `riptrace_plugin` is injected into the guest as a shared object
and handles syscalls synchronously.

## Build

Build the host executable and plugin from the Reverie workspace root:

```sh
cargo build -p riptrace -p riptrace-tool
```

This produces:

- `target/debug/riptrace`
- `target/debug/libriptrace_plugin.so`

Build the SaBRe revision pinned in
`../reverie-sabre/SABRE_UPSTREAM.toml` separately with CMake, Make, and GCC.

## Run

Pass the loader and plugin explicitly:

```sh
target/debug/riptrace \
  --sabre /path/to/sabre/build/sabre \
  --plugin target/debug/libriptrace_plugin.so \
  -- /bin/echo hello
```

The same paths can be supplied through `SABRE_BINARY` and `SABRE_PLUGIN`.
Use `--output FILE` to write the syscall trace to a file, `--only-failures` to
filter successful calls, or `--quiet --summary` to report only the count.

The demo currently supports dynamically linked Linux x86-64 guests. SaBRe
does not support static executables.
