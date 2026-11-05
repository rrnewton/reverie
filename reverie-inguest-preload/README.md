# reverie-inguest-preload

Standalone `LD_PRELOAD` leaf for the shared Reverie in-guest runtime.

Build it explicitly:

```sh
cargo build -p reverie-inguest-preload
```

Cargo produces `libreverie_inguest.so`. Set `REVERIE_INGUEST_TOOL` to
`passthrough` or `spoof-getpid` and preload that library to install the
corresponding built-in tool. Embedding backends should depend on the
`reverie-inguest` rlib instead and install the runtime themselves.
