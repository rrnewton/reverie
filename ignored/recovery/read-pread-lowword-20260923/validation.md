# Read and pread64 fd low-word validation

- Recorded: 2026-09-23T00:25:15-07:00
- Repository: rrnewton/reverie
- Base: dc7dac97995fee2393d2bcef147116134314781a
- Exact head: 03ce6d472df1537b68fdc6ead5e2a89d5294a390
- Frozen artifact: read-pread-lowword-frozen-v1.diff
- Artifact SHA-256: a16c40b0b4974135f6fa05399a75392e9ab1ec03360611d71110d8f8d4480ee6
- Artifact identity: byte-identical to `git diff --binary origin/main..HEAD`

## Green gates

- `cargo fmt --all -- --check`: pass
- `git diff --check`: pass
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`: pass
- Focused unit: 1 passed, 0 failed
- Required real-KVM integration with `REVERIE_REQUIRE_KVM=1`: 1 passed, 0 failed
  - native once
  - direct KVM twice
  - Tool KVM twice
  - exact stdout, stderr, and exit status comparison only; no L2/log/replay-parity claim
- Full library default: 816 passed, 0 failed
- Full library serial: 816 passed, 0 failed

## Causal mutation evidence

Each production decoder was individually restored to its old `i32::try_from` behavior while the tests remained unchanged:

- `read`: focused unit observed `EBADF` instead of `EFAULT`; required KVM guest exited 3.
- `pread64`: focused unit observed `EBADF` instead of a two-byte read; required KVM guest exited 4.

Both source files were restored to their pre-mutation SHA-256 values before final green runs.

## Oracle correction and retained limitations

Preliminary review caught an invalid initial oracle: Linux validates a negative `pread64` offset before fd lookup, so a combined bit-31 fd, bad pointer, and offset `-1` returns `EINVAL`, not `EBADF`. The frozen tests use offset `0` for the bad-fd/bad-pointer discriminator and separately require `EINVAL` for a valid high-word fd plus offset `-1`.

Two pre-existing ordinary-file `pread64` ordering gaps remain out of scope: zero-length handling versus a noncanonical pointer, and negative-offset validation versus a bad buffer. This slice changes only fd decoding and does not claim to repair those behaviors.
