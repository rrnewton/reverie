# Running-kernel local debug/source availability

## Usable matching artifact found

This host has **more than headers/BTF**: an unstripped x86-64 ELF kernel with
embedded DWARF, symbol table, line tables, location/range data and BTF.

Canonical file:
`/usr/lib/modules/7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf/vmlinux-7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`

Convenient aliases:
`/boot/vmlinux-7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf` and
`/usr/src/kernels/7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf/vmlinux`.
Both resolve through `/lib/modules` to that same file. Size: 524253040 bytes.

- GNU build ID: **fa15f90afce5930f146e2138d2ba10eb95e3e5ad**.
- SHA256: **e4ca09342f074c21080a6d31d17fb7ad6f5fbe2c7b84e14759acfd97e16e0db2**.
- Its entire 84-byte `.notes` section equals the running kernel's
  `/sys/kernel/notes` byte-for-byte, including the GNU build ID. Notes SHA256:
  `4bab71b60fbb4fdb2a33125327bafac94f3f982378d5931f004dfd410eeab55a`.
- The image SHA256 also equals the installed RPM file digest (algorithm8).
  This is a running-build/package-image match, **not a uname-prefix inference**.

Package owner: `kernel-7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf.x86_64`.
Build date: 2026-08-24 01:49:47 local time; installed 2026-08-26 13:47:54.
RPM records source package
`kernel-7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf.src.rpm`; that name is provenance
metadata, not proof the source RPM is present or proof of a full vendor Git SHA.

## Directly useful signal/xstate symbols

`nm` and offline `addr2line` resolve the requested implementation functions:

| Function | Link-time address | Embedded source location |
|---|---|---|
| `get_sigframe` | `ffffffff821d46c0` | `/rw/compile/arch/x86/kernel/signal.c:96` |
| `get_sigframe_size` | `ffffffff8215ae30` | `/rw/compile/arch/x86/kernel/signal.c:231` |
| `x64_setup_rt_frame` | `ffffffff821d4370` | `/rw/compile/arch/x86/kernel/signal_64.c:165` |
| `fpu__alloc_mathframe` | `ffffffff821d4950` | `/rw/compile/arch/x86/kernel/fpu/signal.c:497` |
| `fpu__get_fpstate_size` | `ffffffff8475dec0` | `/rw/compile/arch/x86/kernel/fpu/signal.c:512` |
| `fpu__init_system_xstate` | `ffffffff8475dee0` | `/rw/compile/arch/x86/kernel/fpu/xstate.c:807` |

These are static ELF addresses, not sampled runtime addresses. This artifact
permits exact-build offline disassembly, DWARF layout and line-mapping inspection.
No allocation maximum, stack safety property or xstate permission semantics was
derived in this availability task. DWARF line paths do not contain the C bodies.

## Bounded source/debug-package negatives

- `/usr/src/debug` and `/usr/local/src` are empty. `/rw/compile` and the exact
  DWARF-referenced C paths do not exist.
- The matching kernel/kernel-devel package manifests contain the vmlinux and
  development tree, but no requested signal/fpu C bodies or packaged source
  archive/source RPM. Kernel-devel's vmlinux is another alias, not a source tree.
- No installed packages match the queried kernel debug/source package patterns.
  Separate debug-module/build-ID paths for this exact image do not exist; a
  separate debuginfo package is unnecessary because DWARF is embedded here.
- `/usr/src/SRPMS`, `/usr/src/redhat/SRPMS`, and `/var/cache/yum` are absent.
  A depth-three, filename-constrained lookup in `/var/cache/dnf` found no
  matching kernel7.1.3 RPM or Linux7.1.3 source archive. This is not a whole-disk
  absence claim or a claim about unexamined caches.
- Full vendor commit/local-patch provenance and human-readable implementation
  sources remain unavailable in the checked locations. The source request in
  `scratch/liteinst-timer-delivery/SIGNAL-STACK-PROOF-INPUTS-20260904.md` is only
  partially unblocked: exact-build binary/DWARF inspection can proceed locally.

## Evidence and method

Raw metadata/manifests and checks:
`/home/newton/work/dev-hermit/ignored/running-kernel-source-availability-20260904/`.
Key files: `vmlinux-sections.txt`, `vmlinux-notes.txt`, `kernel-identities.sha256`,
`build-identity.txt`, `rpm-digest-match.txt`, `package-info.txt`, `package-files.txt`,
`target-symbols.txt`, `target-source-lines.txt`, `conventional-paths.txt`,
`dwarf-source-path-check.txt`, and `matching-cache-artifacts.txt`.

Commands used: `rpm -qf/-qi/-ql/--dump`, scoped installed-package queries,
`file`, `readelf -n/-S`, `nm --defined-only`, and
`DEBUGINFOD_URLS= addr2line -a -f -e <image>` for six selected addresses.
`readelf` identifies `.notes` at file offset0x1f5b788, size0x54; copying those
84 file bytes and comparing with `/sys/kernel/notes` establishes the note match.
No debug/source download facility was used.

No network, package installation, privilege changes, `/proc/kcore`, device
memory, runtime instrumentation, build, guest execution, product edits or
recursive agents. The 524MB image stays in its installed location; only metadata
and 84-byte notes were retained. Product remains clean at
`8e8d36adf2ea220c35a5d2bc0c6a41aa90f92675`. Stop at availability evidence.
