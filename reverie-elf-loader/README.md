# reverie-elf-loader

A standalone building block for starting an ELF image inside its new process.
It is **unwired**: no Reverie backend or Hermit runtime calls it. It implements
the LA row of the launcher design approved in coordinator message 1817, not
the authorization, syscall mediation, or runtime handover stages.

## Build

`build.rs` invokes the system C compiler to build `loader/loader.c` and
`loader/entry.S` with `loader/loader.ld`. The result is a static, freestanding
ET_EXEC at `0x100000`: no libc, dynamic loader, allocator, or Rust runtime.
The Rust host library embeds that template and constructs each sparse start
image. C and assembly retain the proven syscall/mapping implementation from
the supplied `lp.c`; assembly gives explicit control over the first and last
instructions. No extra Cargo build dependency or network fetch is needed.

The build also generates PIE and non-PIE observation fixtures, an interpreter
entry observer, and a separate loader with test mutations enabled. The
production template rejects nonzero mutation flags. Every generation command,
source hash, binary SHA-256 and compiler version is recorded in
`target/debug/build/reverie-elf-loader-*/out/PROVENANCE.txt`. Compiler logs,
generated images, and test evidence stay under `target/`.

This workspace centralizes Buck rules in the root `BUCK`, so the new crate's
`rust_library` and companion freestanding executable `genrule` are there too.
The Buck build embeds the genrule output; Cargo uses `build.rs`. The ordinary
Cargo parity suite is independent of Meta's Buck test infrastructure.

## Start protocol

1. Pin a readable regular ELF file. Construct `Invocation::execve` or
   `Invocation::execveat` with the literal filename and child descriptor number.
   Preparation uses the kernel's execfn rules: a relative dirfd call synthesizes
   `/dev/fd/N/path`; an empty-path call synthesizes `/dev/fd/N`.
2. Call `prepare_start`, naming a short symlink that the caller will create.
   Redundant separators pad the symlink filename to the native execfn length.
   Execute that padded name with exactly the target's argv and envp.
3. `PreparedStart::write_image` writes the sparse per-start ELF. Its replaceable
   PT_NOTE becomes an R-only PT_LOAD with the target's native `start_data`,
   `end_data`, and `start_brk`. The kernel itself installs those mm fields.
4. In the child, disable ASLR with `personality(ADDR_NO_RANDOMIZE)`. Pass the
   target on fd 100 and `PreparedStart::metadata()` on fd 102, both without
   CLOEXEC. Metadata is `execfn\0comm\0flags`; the production flags byte is zero.
   Keep other caller descriptor state identical to the native control.
5. The loader uses its own shared scratch mapping, leaving the original stack
   untouched while it reads the target/interpreter. It removes the placeholder,
   maps both ELF images, and relocates vvar/vvar_vclock/vdso to their native
   positions. It patches scalar auxv types 3, 5, 7, 9, and 33, plus the bytes
   addressed by type 31 (`AT_EXECFN`). The host plan lists these patches.
6. The loader sets native comm, closes its target/metadata/interpreter FDs,
   removes transient scratch, makes the final record read-only, resets extended
   processor state, and jumps to the interpreter with zero GPRs except RSP.

The caller owns argv/envp preparation, descriptor reservation, immutable file
identity and exec authorization. Preparing a start does not execute anything.
The loader diagnoses its own post-exec failures on stderr and exits 127; it
does not implement the later backend's failure or rollback protocol.

## Admitted scope and named refusals

The implementation supports little-endian ELF64 x86-64 ET_EXEC and ET_DYN
programs with PT_INTERP and exactly one non-executable PT_GNU_STACK. Headers
must have ordinary page-congruent load geometry, ascending load addresses,
and at most 128 program headers. ELF32, static targets, executable/missing or
repeated stack headers, malformed/unsupported ELF geometry, launcher paths
longer than the native filename, and unsupported execveat flags have explicit
errors. Raw and biased load ranges must be below `0x800000000000`, the default
x86-64 user address limit.

`Error::ZeroInterpreterLoadSpan` refuses an interpreter whose maximum
`PT_LOAD` memory end minus its minimum page-aligned load address is zero,
matching Linux 6.17. The loader repeats this check defensively. Tests require
native SIGSEGV and the two named refusals for zero spans, and native/loader
parity for a one-byte positive span, with both ET_EXEC and ET_DYN interpreters.

An ET_DYN interpreter's first load address remains an mmap hint when the main
image has zero load bias. A biased main discards that hint, as Linux does.
Controls use a free `0x800000` interpreter address with ET_EXEC and PIE mains.
Preparation and loading both use the last PT_LOAD containing the program
header table for AT_PHDR; controls map the header page twice.

`Error::PaddedPathTooLong` includes the terminating NUL in its reported size.
4095 filename bytes qualify; 4096 do not. A 4095-byte relative path can succeed
in native execveat while its `/dev/fd/N/` prefix requires a named loader refusal.
The suite tests the real native call at this boundary.

`Error::LowLoadSegment` refuses resolved program segments below `0x400000`
(including fixed-address interpreters). The qualifying ET_EXEC fixture starts
at exactly `0x400000`; a fixture at `0x3ff000` is refused. PIE addresses are
checked after applying their native bias.

Finite `RLIMIT_AS` has a named refusal: the extra mappings consume address
space. Finite `RLIMIT_DATA` is supported at or above the target/interpreter ELF
startup footprint. `Error::FiniteDataLimitBelowStartupFootprint` refuses smaller
limits, reporting both the limit and its conservative page accounting boundary.
Both no-libc entry fixtures succeed at exactly **282624 bytes (69 pages)**;
native execution fails with SIGSEGV one byte below, and preparation gives the
named refusal there. `/bin/true` has a 24576-byte ELF startup boundary.

Shared scratch is excluded from Linux's private writable data accounting.
Although shadow file pages are read-only, its anonymous BSS is writable and
counts; the shadow and real data mappings replace each other. Tests sample
VmData at every traced syscall and require loader peak accounting to be no
larger than the native control, alongside an identical first failed brk request
at a finite 2 MiB limit. Overlapping loads can make the admission budget
conservative; those refused starts are not reported as native execution errors.

For AT_EMPTY_PATH, comm comes from the executable dentry. A procfd link ending
in ` (deleted)` with nonzero link count is ambiguous: it might be a live name
with that literal suffix or an unlinked dentry with surviving hardlinks.
`Error::AmbiguousDescriptorComm` refuses this class. Fully deleted files and
memfds have zero link count and are covered by the parity tests. Preparation
classifies the interpreter through O_PATH before reopening the pinned regular
file, so a malformed PT_INTERP FIFO cannot block an ordinary read-open.

## Evidence and exact differences

The ordinary Linux integration tests run kernel and loader starts with ASLR
disabled. At the exec stop, ptrace supplies identical 16-byte `AT_RANDOM`
witnesses to both runs before any instruction. This is the only replacement
of inherently random data; comparisons keep stack addresses and all other
stack bytes exact. A restored instruction breakpoint captures both starts at
the interpreter's actual first instruction, avoiding syscall-stop register
artifacts. Procfs loader values are bound to their original kernel exec event.
The `observed_initial_register_and_segment_state` test checks GPRs/RIP,
RFLAGS, MXCSR, x87 FCW/FSW and abridged FTW, all six segment selectors, FS/GS
bases, XMM, and supported YMM/ZMM/opmask/PKRU fields exactly. The independent
assembly observer stores GPRs before changing registers, switches to its own
stack before PUSHFQ and FXSAVE, and reads segment selectors directly and bases
with ARCH_GET_FS/GS. It samples the original stack through the top page.
Each native and loader observation must agree with its own ptrace capture;
the two records must also agree byte for byte. CPUID and XCR0 gate
YMM, ZMM/opmask and PKRU observations. The test does not assert x87 data
registers, instruction/data pointers or every possible XSAVE component.

The allowed procfs differences are an **exact set**, not a filter for arbitrary
new differences:

| Surface | Expected difference |
| --- | --- |
| `/proc/self/exe` | Prepared loader image rather than target file |
| `/proc/self/auxv` | Exactly value types **3, 5, 7, 9, 33** retain the kernel's loader values; separate low-interpreter-hint controls require both `AT_BASE` values to be zero, native vDSO to equal loader kernel-origin vDSO, and exactly **3, 5, 9** to differ |
| `/proc/self/stat` | Code fields **26 and 27** retain loader code bounds |
| `/proc/self/maps` | Loader RX file mapping at `0x100000`, and exactly one `0x200000–0x201000` R-only shared record mapping |

All stack auxv entries (including AT_NULL), argv, envp, execfn, initial stack
pointer/content, native program/interpreter/libc bases, special mappings,
heap/data fields, comm and cmdline must agree. Map comparisons retain ordinary
rows exactly and validate each extra loader row's extent and permissions.

Mutation controls execute seven faulty starts and require the ordinary
comparator to fail by name: omitted placeholder → `heap_metadata`; omitted
vDSO relocation → `mapping_parity`; dirty R12, x87 status, x87 tags, DS/ES
selectors or FS/GS bases → `entry_registers`. The x87 controls change status
and tags separately. Every register mutation must also fail the independent
assembly-record parity comparison after agreeing with each ptrace capture.
They do not turn an observed mismatch into a successful parity result.
Each mutation first requires a healthy start from the same test template.
Per-start image hashes and preparation controls are recorded under
`target/elf-loader-tests/` and the Cargo build output directory respectively.

```sh
cargo --offline fmt --all -- --check
cargo --offline clippy -p reverie-elf-loader --all-targets -- -D warnings
cargo --offline test -p reverie-elf-loader
```

Tests fail, rather than silently skip, when required personality/ptrace/procfs
facilities are unavailable. ASLR-on execution, other architectures, arbitrary
kernel/version portability and complete exec semantics are not claimed.
