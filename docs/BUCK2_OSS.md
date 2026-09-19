# Reproducing the OSS Buck2 build

The Buck2 graph is generated from Reverie's authoritative root `Cargo.toml`
and tracked `Cargo.lock`. Generated dependency rules and vendored sources are
ignored because they can be recreated byte-for-byte.

## Build from a fresh checkout

Install Git, rustup, and the open-source
[DotSlash](https://dotslash-cli.com) launcher, then run:

```sh
./bootstrap/regenerate-rust-deps
./bootstrap/buck2 build \
  //:reverie-ptrace \
  //:reverie-kvm \
  //:reverie-liteinst \
  //:reverie-rpc-transport
```

On a Meta development host, `/usr/bin/dotslash` is the internal DotSlash2
launcher and cannot read this public descriptor (it reports a missing `scheme`
field). Invoke the open-source launcher explicitly there; do not add an
internal `scheme` field to the public descriptor.

These are library targets for the ptrace backend, KVM backend, in-process
LiteInst backend, and the RPC transport shared by non-ptrace backends. They do
not replace the Cargo test suite; run the repository's normal Cargo validation
as well when changing runtime behavior.

## Immutable inputs

- Buck2 release `2026-08-01`, selected by a DotSlash descriptor containing the
  expected size and BLAKE3 digest for each supported platform.
- Reindeer commit `e3d72748131d3a70378055f091e0647c1edad85e`.
- Reindeer's bootstrap compiler `nightly-2026-05-22`.
- Reverie's build compiler `nightly-2026-07-29`, selected by the root
  `rust-toolchain.toml` without changing the toolchain used by other projects.

The first Reindeer invocation fetches the exact source commit, refuses a dirty
cached source tree, installs its pinned compiler if needed, and builds into a
cache keyed by both source and compiler revisions. Set
`REVERIE_BUCK2_TOOL_CACHE` to relocate that cache. DotSlash downloads and
verifies the selected Buck2 release binary.

`regenerate-rust-deps` removes only Git-ignored generated paths, vendors the
versions fixed by `Cargo.lock`, generates `shim/third-party/rust/BUCK` twice,
and rejects unequal output or a lockfile change. The generated-path patterns
must remain in the repository-root `.gitignore`: placing them in
`shim/.gitignore` causes Reindeer to omit the vendored source files.

The checked-in root `BUCK` file and shim remain usable when Reverie is consumed
as Hermit's `reverie` cell. Hermit is responsible for pinning that cell to the
same immutable Reverie revision used by its Cargo manifests and lockfiles.
