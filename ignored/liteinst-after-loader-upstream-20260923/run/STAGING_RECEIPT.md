# Current schema-3 after-loader staging receipt

No guest test had run when this receipt was finalized. The sole no-guest binder
preflight is recorded below.

## Frozen source and host

- Working directory: `/home/newton/work/dev-hermit/worktrees/slots/liteinst-reverie-replay-20260922`
- Preserved destination base: `5f5cc66039de38bc07301705753fc6e6de4d1928`
- Local pre-publication HEAD: `f554c81bdeca6f62d7298f87bd73563c2940a46a`
- Reviewed 48-path after-loader source manifest SHA-256: `5f881d9da1983b17329f24c5c347e8169806c20a87a0237e735f9004f18049e3`
- Complete 78-path product-content fingerprint after rustfmt: `ab2d1a90c0a1c6915b9ef3a769f8282073024694fb7681ab82223f4a176cfdeb`
- Pre-staging free-space receipt: `906G` available from `df -BG --output=avail .` (required floor: `400G`).
- Root `Cargo.lock`: SHA-256 `cb1bad488fb21983a95c72709a7b34d851bdceacbf18791b2382e8f9e3d4b39b`, size 67,085, mtime 1790088086. It predates staging, remained unchanged, and was Cargo's ignored build-resolution input; it is not staged or committed.
- Rust: `cargo 1.99.0-nightly (3efb1f477 2026-07-17)`; `rustc 1.99.0-nightly (26ae60a9e 2026-07-28)`, x86_64-unknown-linux-gnu, LLVM 22.1.8.
- C: `cc (GCC) 11.5.0 20240719 (Red Hat 11.5.0-15)`.

Key formatted-source Git blobs:

```text
32291612a4d3abdd72601e220f532707ad954bfb  reverie-liteinst/Cargo.toml
5a9acf1abcd817ae257de4e24cb112084d57888c  reverie-liteinst/build.rs
67a704519a2f54ef1395133c664fa30ce3d83432  reverie-liteinst/src/backend.rs
edd478e5951c70457b78437dd34013323eb325e8  reverie-liteinst/src/runtime.rs
d9641114e3887c833e55f8d1e158b6759b32ac53  reverie-liteinst/tests/after_loader.rs
eacbb043cbc3cfc38878a228072053c2211a282d  reverie-liteinst/tests/fixtures/after_loader.c
cce72c11033db74426afdc2d1bac1dc2e24564d8  reverie-ptrace/src/after_loader.rs
15de6e87d18146efd4a6c1403a2e1e767df96322  reverie-ptrace/src/after_loader/manifest.rs
ddfea89431ec1dcf8ee53270a016ed5707b8fdb9  reverie-ptrace/src/after_loader/tests.rs
14398360862b405e4bb6332a87286341552c69a9  reverie-ptrace/src/task.rs
60c613fae5757c0e08e658d4c015c59f46d256ce  reverie-ptrace/src/task/after_loader_task.rs
80f101b31eef800301d00147708484368e8d620f  reverie-ptrace/src/tracer.rs
69e03e985150fd7b803f161f5eb2d9d1ccffeec1  safeptrace/src/lib.rs
759d837e7b729e30224461a0beee4ce0a1cea573  safeptrace/src/notifier.rs
4eaa7ff109a7de2afb4bd9b9a9a69a0e45fe3b56  safeptrace/src/regs.rs
```

## Exact commands

Every command ran from the working directory above. Exit receipts in this
directory are `0`.

```sh
cargo build --release -p reverie-liteinst --lib \
  --no-default-features --features liteinst-after-loader-experiment \
  > ignored/liteinst-after-loader-upstream-20260923/run/runtime-build.log 2>&1

install -m 0755 target/release/libreverie_liteinst.so \
  ignored/liteinst-after-loader-upstream-20260923/stage/libreverie_liteinst.so

cc -O2 -fno-pie -no-pie \
  -Wl,--dynamic-linker=/usr/lib64/ld-linux-x86-64.so.2 \
  -o ignored/liteinst-after-loader-upstream-20260923/stage/after-loader-fixture \
  reverie-liteinst/tests/fixtures/after_loader.c \
  > ignored/liteinst-after-loader-upstream-20260923/run/fixture-build.log 2>&1

CARGO_TARGET_DIR=ignored/liteinst-after-loader-upstream-20260923/generator-target \
  cargo run --quiet \
  --manifest-path ignored/liteinst-after-loader-upstream-20260923/generator/Cargo.toml -- \
  ignored/liteinst-after-loader-upstream-20260923/stage \
  ignored/liteinst-after-loader-upstream-20260923/stage/after-loader-fixture \
  ignored/liteinst-after-loader-upstream-20260923/stage/libreverie_liteinst.so \
  > ignored/liteinst-after-loader-upstream-20260923/run/generator.log 2>&1
```

The generator did not use `--offline` or `--locked`. It is an isolated ignored
crate with its own lock; its only non-path direct dependency is `sha2 = 0.10.9`,
already present in the product graph. It invokes
`LiteinstCallerImage::runtime_stage_marker` for the marker and writes three
canonical schema-3 manifests. Its isolated `Cargo.lock` is 44,473 bytes with
SHA-256
`589132943a3afc3e8b2843836df39451331b21f972dedc172021d81485a85c37`.
No product lock was staged or copied.

## Cargo feature fingerprint

The staged runtime is byte-identical to
`target/release/libreverie_liteinst.so`. Its library fingerprint is
`target/release/.fingerprint/reverie-liteinst-786bc187666f3003/lib-reverie_liteinst.json`
(SHA-256 `82f8a9a51839140f0e893ddca2f62942c850189e8716f0f0e73af87c1af7578a`),
which records exactly `features=["liteinst-after-loader-experiment"]` and no
default or preload-constructor feature. The runtime build log SHA-256 is
`53a1447d711282df221e7b3db4ab0678410a80d7f77db26cd4622387188c06e2`.

## Frozen artifacts

```text
mode size    SHA-256                                                          path
0755 19224   e71a87451a988c09cda15d17e75f01292119426d65c2a507d64bd8deb3dd82d7  stage/after-loader-fixture
0755 1881736 462609264749a591d93efdff6f596d6337d6930b960c948545250ed16016205f  stage/libreverie_liteinst.so
0644 196     40e4f4b87a0135a1b123ee1f624067494150a2f8a9f12cccf94c6d27e3fc3cf3  stage/runtime.marker
0644 1997    3d2c470197194236a9da9274f98a4333691f095602d72dd8f1b4a29d1c83160c  stage/four-canonical.manifest
0644 1973    7c886ac99e5a54ea4852b055422654b058a998f735c2b35b9f46e2cc7909484d  stage/one-call.manifest
0644 2049    c7fa200d3b3705f3eb36e7271a7b8b7387e9c715a51f0a468811a25c12c4ce35  stage/unpatchable.manifest
```

The fixture is x86-64 `ET_EXEC` with exact
`PT_INTERP=/usr/lib64/ld-linux-x86-64.so.2` and only `libc.so.6` in `DT_NEEDED`.
The runtime is x86-64 `ET_DYN` with `libgcc_s.so.1`, `libc.so.6`, and
`ld-linux-x86-64.so.2` in `DT_NEEDED`. Complete `readelf` receipts are
`fixture-readelf.txt` (SHA-256
`9845a8b8c5e1ad56745b803b9cbd230c1cdadfc2994bc9225f27515c7dc3dfce`)
and `runtime-readelf.txt` (SHA-256
`fbf3c5031393fa9978b6bcc0b37dfe3e9cf2ddec7a98eb24ebafe3a461f10dee`).

Reviewed host inputs bound by every manifest:

```text
31e1679f18ef95be0860ea88b3083434aa187ef3b6d804aa46efe8dea31aa71d  /etc/ld.so.cache
3a47ec2c0e2b0f0948ea0caefb77d298679df3b8474631c7be7989f11b762c5a  /usr/lib64/ld-linux-x86-64.so.2
1bb475607dfdcec1cf8b1a885e2c71a36797e786e41248deb50ab54397d56aac  /usr/lib64/libc.so.6
3d144b557008c93e4e67567d5dc98cb81cf55187a698d1b4872ce84ba8a26bed  /usr/lib64/libgcc_s-11-20240719.so.1
```

`/etc/ld.so.preload` was absent. The generator source SHA-256 is
`e846d7f53e7e8e2d61c65fb2382a6752fc96d23389b336de4c3df9059a4711eb`;
its output log SHA-256 is
`7d891dbb0c1ab5b463d216d97331eb0e7b93d50576021fb9c31423096bcd2f26`.

## No-guest binder preflight

After independent artifact-byte review, the exact-filter
`staged_reviewed_profiles_bind_exact_union_graph` test bound all three reviewed
manifests in Cargo's release profile with default features disabled and only
`liteinst-after-loader-experiment` enabled. It does not start a tracee. The test
passed 1/1; `binder-preflight.log` has SHA-256
`01d8dc2d1933ecfb7d37e8e324b414b6b33b8e8d276aea8c2362e55372eb546b`.
All six staged artifact hashes remained byte-identical after binding.

The exact command (with `stage` equal to the canonical absolute stage directory
recorded above) was:

```sh
REVERIE_LITEINST_AFTER_LOADER_FIXTURE="$stage/after-loader-fixture" \
REVERIE_LITEINST_AFTER_LOADER_RUNTIME="$stage/libreverie_liteinst.so" \
REVERIE_LITEINST_AFTER_LOADER_MARKER="$stage/runtime.marker" \
REVERIE_LITEINST_AFTER_LOADER_MANIFEST_FOUR_CANONICAL="$stage/four-canonical.manifest" \
REVERIE_LITEINST_AFTER_LOADER_MANIFEST_FOUR_CANONICAL_SHA256=3d2c470197194236a9da9274f98a4333691f095602d72dd8f1b4a29d1c83160c \
REVERIE_LITEINST_AFTER_LOADER_MANIFEST_ONE_CALL="$stage/one-call.manifest" \
REVERIE_LITEINST_AFTER_LOADER_MANIFEST_ONE_CALL_SHA256=7c886ac99e5a54ea4852b055422654b058a998f735c2b35b9f46e2cc7909484d \
REVERIE_LITEINST_AFTER_LOADER_MANIFEST_UNPATCHABLE="$stage/unpatchable.manifest" \
REVERIE_LITEINST_AFTER_LOADER_MANIFEST_UNPATCHABLE_SHA256=c7fa200d3b3705f3eb36e7271a7b8b7387e9c715a51f0a468811a25c12c4ce35 \
cargo test -p reverie-liteinst --release --no-default-features \
  --features liteinst-after-loader-experiment --test after_loader \
  staged_reviewed_profiles_bind_exact_union_graph -- \
  --exact --nocapture --test-threads=1
```
