# Reverie LiteInst runtime package

This Linux x86-64 leaf owns the in-process implementation used by the
`reverie-liteinst` launcher crate. It has no launcher, tracer, safeptrace or
libunwind package dependency. It explicitly enables `reverie-core/pmu` and uses
core's shared PMU, vDSO and instrumentation statistics definitions directly.
No PMU, vDSO, syscall, clock, permission or failure policy is reimplemented here.

The launcher crate owns `LiteinstBackend`, its caller-owned preparation methods,
logging adapters and run observers. It does not expose ambient preload-library
discovery or a pathname-based launcher. The generic `Backend` methods return `Unsupported`;
the runnable API accepts a caller-owned `PreparedCommand` and retains its owner
through pidfd-based child termination and reap. Existing runtime-facing paths
reexport this crate's exact types and functions; there is no extension trait.
The shared bootstrap schema lives here once. Public bootstrap, logging, runtime
marker and statistics bridges support the launcher-side RPC server without
duplicating their representation or implementation.

## Initialization and linkage

There is one patch allocator, one set of assembly entries and one built-in
constructor definition, all owned here. The launcher crate forwards its
features. The default `preload-constructor` feature links the constructor but
does not select or install a tool from ambient state. The removed
`REVERIE_LITEINST_TOOL` selector is refused. A custom tool runtime using `clocked_initializer!`
must depend on this package with `default-features = false`, as it previously
did for the launcher crate. Do not combine a custom constructor with the built-in
one. The macro resolves its implementation through `$crate`; launcher reexports
retain both Rust API reachability and the existing DSO C exports. Never validate exports
by executing an unreviewed runtime.

Production log targets retain their `reverie_liteinst` spelling where
observable. Launcher logging remains in `reverie-liteinst`; moving the owning crate
does not select a new logging filter or change the transport protocol.

The `private-crt` feature permits ordinary binaries to link without a private
CRT provider. Its ambient TLS and initial-capture probes use nullable link-time
references to `pl_tls`, `pl_guest_arch_prctl`, and `pl_take_initial`. Private
startup refuses a missing or partial provider before changing preparation state
or consuming a capture. Symbol presence does not establish ownership or readiness;
the existing native control, TLS transaction, and initial-frame checks still apply.
An active control remains installed even if a required function is missing.

Explicit GNU startup consumers still require the six strong `pl_gnu_*` imports.
The private build must include the actual native provider objects explicitly;
weak references alone do not extract definitions from a static archive. Neither
ordinary linking nor the host-test provider fixtures authorize private execution.

The RNG descriptor tests require the existing 8192-byte image with SHA-256
`a8ccf4c1a87dd38740ac49cbd355d9e462b43699befaab26281dfe0b71065567`.
An explicit `LITEINST_RNG_DESCRIPTOR_ELF` fixture remains authoritative. Without
that selector, the tests read this process's vDSO through the existing mapping
and ELF checks and require the same digest. A different host image is an error,
not a skipped test or a new supported profile. This test-only fallback does not
change production acceptance or require embedding kernel image bytes in source.

## Cargo export synchronization

This exported tree contains no Buck source target for the LiteInst package.
The existing LiteInst manifest and root workspace are the source of this Cargo
split; no fictitious Buck rule is supplied. Upstream/internal autocargo owners
must add the real runtime target, transfer the moved sources/assembly and
feature/dependency edges, and make the launcher crate depend on that target before
regenerating the export. Preserve the explicit core PMU edge, constructor
feature forwarding and existing launcher cdylib target.
Do not regenerate over this split without that source synchronization. Existing
`@fb-only`/`@oss-only` markers and other generated manifests remain untouched.

This package boundary alone does not establish native startup, complete guest
admission, determinism or a published Hermit dependency pin. The manifests pin
public, fetchable `liteinst2` revision
`95ee5e6917fa33191eb41c3f1606ea8b03c1b78c`; crates.io also publishes
`liteinst2` 0.1.0. Consumers must use a Reverie revision that actually contains
this package; a missing package at an older pin is not repaired by merely
spelling its new name in a product manifest.
