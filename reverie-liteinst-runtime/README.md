# Reverie LiteInst runtime package

This Linux x86-64 leaf owns the in-process implementation used by the
`reverie-liteinst` coordinator facade. It has no facade, tracer, safeptrace or
libunwind package dependency. It explicitly enables `reverie-core/pmu` and uses
core's shared PMU, vDSO and instrumentation statistics definitions directly.
No PMU, vDSO, syscall, clock, permission or failure policy is reimplemented here.

The facade still owns `LiteinstBackend`, its inherent launch methods and the
`Backend` implementation, command configuration, preload-library discovery,
`PreloadTool`, logging adapters and run observers. Host tracer launch methods
and their backtrace support are unchanged. Existing runtime-facing facade paths
reexport this crate's exact types and functions; there is no extension trait.
The shared bootstrap schema lives here once. Hidden public bootstrap, logging,
runtime marker and statistics bridges support the existing coordinator without
duplicating their representation or implementation.

## Initialization and linkage

There is one patch allocator, one set of assembly entries and one built-in
constructor definition, all owned here. The facade forwards its historical
features. The default `preload-constructor` feature retains the original built-in
initialization contract. A custom tool runtime using `clocked_initializer!`
must depend on this package with `default-features = false`, as it previously
did for the facade. Do not combine a custom constructor with the built-in one.
The macro resolves its implementation through `$crate`; facade reexports retain
both Rust API reachability and the legacy DSO C exports. Never validate exports
by executing an unreviewed runtime.

Production log targets retain their legacy `reverie_liteinst` spelling where
observable. Coordinator logging remains in the facade; moving the owning crate
does not select a new logging filter or change the transport protocol.

## Cargo export synchronization

This exported tree contains no Buck source target for the LiteInst package.
The existing LiteInst manifest and root workspace are the source of this Cargo
split; no fictitious Buck rule is supplied. Upstream/internal autocargo owners
must add the real runtime target, transfer the moved sources/assembly and
feature/dependency edges, and make the facade depend on that target before
regenerating the export. Preserve the explicit core PMU edge, constructor
feature forwarding, legacy facade cdylib target and host tracer/backtrace edges.
Do not regenerate over this split without that source synchronization. Existing
`@fb-only`/`@oss-only` markers and other generated manifests remain untouched.

This package boundary alone does not establish native startup, complete guest
admission, determinism or a published Hermit dependency pin. Consumers must use
a revision that actually contains the package; a missing package at an older
pin is not repaired by merely spelling its new name in a product manifest.
