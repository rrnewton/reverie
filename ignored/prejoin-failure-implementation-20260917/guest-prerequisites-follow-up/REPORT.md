All required pre-existing guest and compiler inputs for the unchanged 24 KVM CLI methods and pthread fixture are available. The complete file list is `inputs-complete.json`, SHA256 `0caa0dd4ca1d6a20b69ddc332a1c15e3a1f4e0dd3ec31731b9a7078023e5a421`. It contains 262 path spellings with resolved paths, full symlink chains, device/inode, size, mode and SHA256. No product source, index or ref changed; this was read-only preparation, not compilation or execution of any guest.

The source is the Hermit worker's `ignored/prejoin-main-guest-plan-v1/guest-prerequisites.json`, original 24-name selection and frozen `cli.rs`/`kvm_harder.rs` copies. `READBACK.json` binds those copies to the live original test files, checks all five exact fixture byte sequences, and retains their original assertions and compiler flags. All 2,188 readback checks passed. The owned Reverie source remained `12d4ce8c0bc426f1ae41416f5b4a699e2c300879`, tree `ba584b989e7dc89fce4ac7eabaa05166ba035069`, with no tracked modifications.

The fixed environment was copied exactly from `/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917/ignored/prejoin-main-callers-v8/compile-input-context.json`. Its PATH is `/home/newton/.cargo/bin:/usr/local/bin:/usr/bin:/bin`, with LC_ALL=C, LANG=C and TZ=UTC. `lookup-inputs.json` records the first PATH selection and every preceding candidate's absence for cc, as, ld, timeout, rust-script and cargo-nextest. It also records absent compiler/linker, Perl and shell-startup overrides. This establishes the discovery configuration; the actual service must recheck the files, symlinks, absences and its declared environment before execution and after completion.

The original empty-input compiler-selection probe succeeded with `/usr/bin/cc -x c -fsyntax-only -`, stdin `/dev/null`. `/usr/bin/cc` resolves to `/usr/bin/gcc`, GCC 11.5.0 for x86_64-redhat-linux. The gcc/clang fallbacks were not used or executed. The direct cc requirements in `cli.rs:611` and `kvm_harder.rs:18` therefore remain satisfiable.

| Input | Exact resolved selection |
| --- | --- |
| C driver | `/usr/bin/gcc` |
| cc1 | `/usr/libexec/gcc/x86_64-redhat-linux/11/cc1` |
| assembler | `/usr/bin/as` |
| collect2 | `/usr/libexec/gcc/x86_64-redhat-linux/11/collect2` |
| linker | `/usr/bin/ld.bfd`, through `/usr/bin/ld` and `/etc/alternatives/ld` |
| LTO wrapper | `/usr/libexec/gcc/x86_64-redhat-linux/11/lto-wrapper` |
| collect2 plugin | `/usr/libexec/gcc/x86_64-redhat-linux/11/liblto_plugin.so` |
| default guest interpreter | `/usr/lib64/ld-linux-x86-64.so.2`, requested as `/lib64/ld-linux-x86-64.so.2` |

The driver SHA256 is `546023eae5ff58287b1d987e059d38e2733dfe49eda827282d6942707c4c25d0`. All five `-###` outputs retain the original fixture flags and exact subprocess/link commands without executing them. They select non-PIE `crt1.o`, `crti.o`, GCC `crtbegin.o`/`crtend.o` and `crtn.o`; the queried alternative `Scrt1.o`/S variants are also bound but are not the dry-run selection. The sysroot query is empty, meaning this host root rather than a separate sysroot. The input list binds the actual header dependencies, startup objects, libc/libgcc linker scripts and their archives/shared objects. `/usr/lib64/libc.so` names `/lib64/libc.so.6`, `/usr/lib64/libc_nonshared.a` and the loader. The pthread link uses `-lpthread`; its existing `/usr/lib64/libpthread.a` is an 8-byte archive, with the implementation integrated into the bound libc.

| Original fixture bytes | Original compiler flags | Dependency-only header count |
| --- | --- | ---: |
| `tests/c/kvm_exact_child_waits.c` | `-O0 -g -Wall -Wextra -Werror` | 92 |
| `tests/backend-parity/fixtures/cpuid_probe.c` | `-O2 -g -std=c11 -Wall -Wextra -Werror` | 33 |
| `tests/backend-parity/fixtures/pthread_lifecycle.c` | `-std=c11 -O2 -g -Wall -Wextra -Werror -pthread` | 51 |
| `run_kvm_pipe_pipe2_and_getgroups_round_trip` inline C | `-O2 -Wall -Wextra -Werror` | 51 |
| `run_kvm_random_device_lseek_matches_linux` inline C | `-O2 -Wall -Wextra -Werror` | 61 |

`fixtures.json` gives exact source/copy paths and all five hashes. `queries/*-header-dependencies/stdout` is GCC's dependency-only `-M` output. Discovery substitutes only copied source and deliberately nonexistent output paths in its dry-run argv; it does not replace the original test commands. The two original inline fixture commands retain `flags -o GENERATED_BINARY GENERATED_SOURCE`. No `NOT-BUILT-*` output exists. The normal tests must still compile their original fixture bytes and bind the actual produced ELF; these queries cannot substitute for that evidence. The existing TempDir deletion behavior of the two inline tests also remains unchanged.

All 16 required guest path spellings and `/etc/hostname` are bound. ELF inspection uses `readelf -W -l -d` and a retained `ldconfig -p` cache listing, never `ldd` or runtime loader execution. `elf-dependencies-complete.json` records 86 unique ELF files, including executables, shared objects and startup objects, with each PT_INTERP and DT_NEEDED dependency. None has RPATH/RUNPATH; each discovered DT_NEEDED has one available cache candidate. This is static dependency discovery, not observation of runtime mappings. The loader cache/configuration, included configuration files, selected host fixture and conditional NSS/timezone files are separately bound.

The Perl tests explicitly use Fcntl and POSIX (`-MFcntl=F_GETFL`, `use Fcntl`, and `-MPOSIX`). `perl-inputs.json` binds 21 direct or source-visible conditional module files, the Fcntl/POSIX/List::Util/mro XS objects and their ELF dependencies. It records all six configured module-directory candidates, `.pmc` precedence candidates, optional bootstrap files and sitecustomize paths. Every listed module has exactly one existing candidate; the sitecustomize and compiled-module overrides are absent. Scalar::Util uses List::Util's XS rather than a missing independent Scalar/Util.so. The broader loader/error modules are conditional inputs, not a claim that each module is loaded by every test. No Perl interpreter was invoked for this discovery.

The production helpers are also bound with their loaders/dependencies: `/home/newton/.cargo/bin/rust-script`, SHA256 `90a6cc5197f2d35f9d9a7c3b30a1a8ca7dcfcfa645024fa4b7a1d565461d7a00`; and `/home/newton/.cargo/bin/cargo-nextest`, SHA256 `ee1af2e9ce0f5eeeee6832035c0d8001a2fdb557332a0383434dfe6b38f0fe83`. Their future generated/cache executables and the Hermit/test executables remain the worker's separate execution-artifact bindings.

There are no unavailable required files in this discovered set. `unavailable-complete.json` preserves four original observations and their dispositions: the bare `liblto_plugin.so` query was unresolved although all dry runs identify the bound absolute plugin; the optional `libpthread.so` name is unresolved while the selected archive is available; external `specs` is absent because GCC uses the retained built-in specs; `/etc/ld.so.preload` is absent and must stay absent for this binding. This is not a claim to have copied or hashed every possible host-root file.

There were 126 successful bounded query children: 40 original compiler/cache/dependency queries and 86 ELF metadata queries. Their recorded sums are 0.428112 CPU seconds and 0.660471 wall seconds. Each query had 5 CPU seconds per process, 15 wall seconds, 1 GiB address space and 1 MiB per output stream, with core files disabled. Complete stdout/stderr and exit records are retained, within bounds and untruncated. The original empty-input probe and `-M` queries execute compiler frontends but do not create objects or guest binaries. The static parser's first supplemental attempt mistakenly matched `--enable-plugin` in GCC's configuration prose; its assertion stopped before new metadata queries, and `complete-inputs-v1-failure.json` plus the original script retain that preparation failure. The corrected parser requires the exact standalone `-plugin` option.

The discovery environment retains the worker compile-context TMPDIR `/tmp/hermit-prejoin-integration-20260917-v7`. GCC's dry-run text therefore contains hypothetical temporary names there; they are not created files or guest execution records. The actual guest services will instead use their declared fresh observer-output TMPDIR/XDG_STATE_HOME, as planned by the worker. That deliberate environment difference must remain explicit rather than relabelling discovery as execution.

The remaining execution obligations are unchanged: real KVM API 12/device admission, original 24-method selection and pthread assertions, actual generated guest/test/Hermit executable binding, declared service environment and supervision, and actual CPU/terminal accounting. No hardware admission, VM, Hermit, test or guest execution was performed by this task.

Primary records in this directory:

- `inputs-complete.json`: `0caa0dd4ca1d6a20b69ddc332a1c15e3a1f4e0dd3ec31731b9a7078023e5a421`
- `elf-dependencies-complete.json`: `36408b98626bc20f7e558614961c608086225491cd3e3f295aa6a956db872a59`
- `perl-inputs.json`: `3503f23490300fec38b19eeeee42cbf7bedbf8cdc43f35398c597f47fd86802e`
- `lookup-inputs.json`: `321f70a1829b0d24f8950b46ab416ae2c39c17d8374ff7a110e44d8a44053621`
- `READBACK.json`: `35c15da27c7f06648983193954f04aaab3f27f425065db146915daa2b0a46612`
