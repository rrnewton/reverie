# Fixed backing for LiteInst runtime stacks

An active LiteInst runtime reserves the half-open address range
`[0x600000000000, 0x600100000000)` before installing its signal handlers.
The reservation consumes 4 GiB of virtual address space; it does not commit
4 GiB of physical memory. A collision or insufficient address-space limit
refuses initialization. The runtime never overwrites an existing mapping,
searches for a different address, or falls back to the guest allocator.

The first MiB contains allocation metadata behind an outer guard. The last
page is also a guard. Each stack has a writable, page-rounded interior and
one inaccessible page on each side. The actual SIGSYS alternate stack uses
at least 64 KiB. A Tool's existing callback continuation retains its 8 MiB
capacity. Registering a stack retains its lease for process lifetime, even
if a later initialization step fails. An unpublished lease can be reused
only after its entire extent becomes inaccessible and its interior contents
are discarded; failed cleanup quarantines the extent. Plain fork inherits
the metadata and live stack contents through private COW mappings.

The storage choice is explicit at the shared runtime installation seam.
Generic in-guest, e9patch and SaBRe callers retain their existing storage.
An inert LiteInst preload does not reserve the range. Standalone Strace has
an alternate signal stack but does not prepare or enter a Tool continuation.
Its tests report that distinction instead of counting an unused stack as
an executed callback. Disabling the alternate stack preserves that existing
option and earns no alternate-stack placement claim.

This change establishes fixed placement and boundary guards for these two
existing stack paths. Their interiors remain on protection key zero. It
does not prevent a guest from writing an interior or replacing a mapping,
and it does not isolate every runtime entry. Cold initialization and patched
entries still require stack isolation; allocator arenas, saved-state owners,
other globals, TLS and foreign-library allocation need separate treatment.
The syscall dispatcher, signal actions, completion protocol and determinism
comparators are unchanged by the storage selection.
