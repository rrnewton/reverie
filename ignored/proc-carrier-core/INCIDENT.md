# Formatting boundary incident

At approximately 2026-09-21 03:23:57 -0700 I ran:

```text
rustfmt --edition 2024 reverie-kvm/src/proc_carrier.rs reverie-kvm/src/lib.rs reverie-kvm/src/error.rs
```

This was not a `--check` invocation and did not set `skip_children=true`.
Because `lib.rs` is the crate root, rustfmt followed its module declarations and
reflowed concurrent integration work in `reverie-kvm/src/elf.rs` and
`reverie-kvm/src/executor.rs`, outside my assigned write boundary. I did not
intend or authorize those edits, and did not revert them. The integration owner
confirmed the reflow and retained the current form.

Observed mtimes during the audit at 2026-09-21 03:28:04 -0700:

```text
2026-09-21 03:19:54.045486547 -0700 reverie-kvm/src/lib.rs
2026-09-21 03:19:54.046486545 -0700 reverie-kvm/src/error.rs
2026-09-21 03:23:57.711125985 -0700 reverie-kvm/src/elf.rs
2026-09-21 03:26:59.097891662 -0700 reverie-kvm/src/proc_carrier.rs
2026-09-21 03:27:15.672868390 -0700 reverie-kvm/src/executor.rs
2026-09-21 03:16:32.665825078 -0700 reverie-kvm/src/vm.rs
2026-09-21 03:22:33.702243441 -0700 reverie-kvm/tests/static_elf.rs
```

The later executor mtime reflects continued work by its owner and obscures the
mtime of the earlier rustfmt traversal. `vm.rs` predates my first owned edit;
`tests/static_elf.rs` is not a child module of `lib.rs`. No other file beneath
`reverie-kvm/src` had an mtime after 03:19:40.

Subsequent formatting command:

```text
rustfmt --edition 2024 reverie-kvm/src/proc_carrier.rs
rustfmt --edition 2024 reverie-kvm/src/proc_carrier.rs reverie-kvm/src/error.rs
```

These named only owned non-root modules; the second command left `error.rs`
byte-identical (mtime remained 03:19:54). No `cargo fmt` command was run. Future
formatting is restricted to `proc_carrier.rs` and `error.rs`, never `lib.rs`.

Cargo commands run before the pause:

```text
script -q -e -c 'CARGO_INCREMENTAL=0 timeout 180s cargo check -p reverie-kvm' ignored/proc-carrier-core/first-cargo-check.typescript
rustfmt --edition 2024 reverie-kvm/src/proc_carrier.rs && df -B1 --output=avail . | tail -1 && CARGO_INCREMENTAL=0 timeout 180s cargo check -p reverie-kvm --locked
df -B1 --output=avail . | tail -1; script -q -e -c 'CARGO_INCREMENTAL=0 timeout 180s cargo test -p reverie-kvm proc_carrier --lib --locked -- --nocapture' ignored/proc-carrier-core/first-core-tests.typescript
```

The first check was the one authorized unlocked command. It exited 101 solely
because integration used three free core functions as associated methods; thin
associated wrappers resolved that mismatch. The locked check then passed. The
focused test run compiled and ran 14 tests: 13 passed and the independently-to-
be-verified transcript golden vector failed. Its exact output is retained.

Cargo.lock hashes:

```text
before 3e6e50a6cec5239e5cc10e5fb291de2ec42f09015046b963697c10459ef2ed7a
after  25b12ddd48c1e2ebe75ca5a09a339cc3de5d3b37b71ea1d14a16a13b9875dcc3
```

First check transcript SHA-256:

```text
a648f0aa37d2f7f9a88461242cbcb8d16d04e4356ff4d9082fb28e196141a620
```

No commit, push, GitHub operation, TaskGraph write, or cleanup/revert occurred.
