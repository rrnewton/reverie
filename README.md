# Hermit Development Workspace

**Hermit runs an ordinary Linux program deterministically: given the same
inputs, the program — even a multithreaded one — behaves identically every time,
down to thread scheduling, timers, and the results of system calls.** It does
this by intercepting the points where a process can observe or influence
nondeterminism — system calls, signals, thread scheduling, and instructions such
as `rdtsc`, `rdrand`, and `cpuid` — and making each of them reproducible.

That determinism is useful for:

- **Reproducible builds** — run a build under Hermit and get byte-identical
  output on every machine, which unblocks content-addressed build caches and
  distribution "reproducible builds" efforts.
- **Debugging concurrency bugs** — a race that reproduces once under Hermit
  reproduces every time; Hermit's *chaos mode* goes further and deliberately
  perturbs the schedule to surface races that rarely appear on their own.
- **Record and replay** — capture a run once and replay it exactly afterward, in
  the spirit of [Mozilla rr](https://rr-project.org/).

Hermit is an open-source project [originally from Meta][upstream-hermit], built
on [Reverie][upstream-reverie], a framework for intercepting a guest process's
system calls. **This repository — `dev-hermit` — is the workspace for developing
Hermit and Reverie**, not a separate product: it pins the exact versions of the
two that build and test together, carries the build/test/setup tooling, and
provides isolated worktrees so several changes can be worked on at once. If you
only want to build and run the `hermit` binary, skip to
[Build Hermit directly](#build-hermit-directly).

Active development happens on the maintained forks — `rrnewton/hermit` and
`rrnewton/reverie` — which is where day-to-day changes land; the historical Meta
upstreams remain useful references.

- <https://github.com/rrnewton/hermit>
- <https://github.com/rrnewton/reverie>

[upstream-hermit]: https://github.com/facebookexperimental/hermit
[upstream-reverie]: https://github.com/facebookexperimental/reverie

## Clone the workspace

```bash
git clone --recurse-submodules https://github.com/rrnewton/dev-hermit.git
cd dev-hermit
git submodule update --init --recursive
```

The product and backend source submodules use public HTTPS URLs, so this path
does not require a GitHub SSH key. Existing clones can adopt the recorded URLs
and initialize the complete pinned tree with:

```bash
git submodule sync --recursive
git submodule update --init --recursive
```

### Submodule checkout policy

All product and backend source submodules are checked out by default. Recursive
initialization gets Hermit, Reverie, LiteInst2, and Reverie's pinned DynamoRIO,
e9patch, and SaBRe sources; those nested backend entries all use
`update = checkout`. No per-backend update override or `--force` is needed.

The normal `make`, `make build`, and `make build-full` entry points also run the
submodule checkout chain before building, so they initialize any missing
required checkout at its recorded revision. Run `make checkout-all` to perform
that initialization explicitly. These Make targets use `with-proxy`
automatically when the wrapper is installed and fall back to plain `git`
elsewhere.

The parent `agent-utils` submodule is separate on-demand tooling, not a product
or backend source dependency. It stays out of ordinary recursive initialization
and is materialized by the scripts that require it.

`WORKTREES.md` explains the optional paired-worktree layout for working on
several changes concurrently. Whichever layout you use, do not develop directly
in the top-level `hermit/` or `reverie/` checkout — those are kept pinned so the
workspace always describes a known-good combination.

## Workspace dependency profiles

Hermit intercepts programs through interchangeable *backends*. The default
backend uses the Linux `ptrace` API and needs only a compiler and Rust; optional
backends — dynamic binary instrumentation (DBI) via DynamoRIO, and the QEMU
demos — need extra tooling. The parent Makefile therefore separates the
lightweight `ptrace` build from those optional dependencies. Each installer may
invoke `sudo` and is followed by the corresponding non-mutating doctor:

```bash
make install-deps-core   # compiler, native libraries, Rust, Python/GDB
make install-deps-full   # core + CMake/Perl/Ninja for the default DBI feature
make install-deps-qemu   # core + QEMU, qemu-img, and static BusyBox
```

On CentOS/RHEL, enable EPEL for the static `busybox` package. The distro QEMU
binary may be `/usr/libexec/qemu-kvm` and BusyBox may be
`/usr/sbin/busybox`; in that case export those paths as `QEMU_BIN` and
`BUSYBOX` for `doctor-qemu` and the QEMU demos.

`make install-deps` remains an alias for `install-deps-core`. Rust is managed
by rustup rather than distro packages. If rustup is absent, install it from
<https://rustup.rs>, reload the shell, and rerun the installer; it installs the
channel selected by `hermit/rust-toolchain.toml` with rustfmt and Clippy.

Run `make doctor` for one aggregated, zero-build report covering all profiles,
or select `doctor-core`, `doctor-full`, or `doctor-qemu`. The doctor verifies
the selected Rust nightly and components, compilers/native libraries, CMake and
Perl, Python/GDB, QEMU/static BusyBox, user/PID namespaces, parent-child ptrace,
seccomp, and PMU availability. PMU and Ninja are warnings because compilation
does not require them; missing required tools or runtime capabilities fail the
selected profile.

Build the lightweight `ptrace` binary or the full default feature set with:

```bash
make                 # release Hermit without optional DBI dependencies
make build-full      # release Hermit with default features, including DBI
```

## Build Hermit directly

You do not need this workspace to build Hermit. For a standalone product
checkout:

```bash
git clone https://github.com/rrnewton/hermit.git
cd hermit
```

Hermit requires x86-64 Linux and uses the nightly toolchain selected by
`rust-toolchain.toml`.

Debian/Ubuntu dependencies:

```bash
sudo apt-get update
sudo apt-get install -y build-essential git libunwind-dev liblzma-dev
```

Fedora/CentOS dependencies:

```bash
sudo dnf install -y gcc gcc-c++ git libunwind-devel xz-devel
```

Build and run a first command under Hermit:

```bash
cargo build --workspace
./target/debug/hermit run -- /bin/echo hello
```

Some integration fixtures require Go. Precise scheduling, CPUID tests, and
namespace-backed integration tests also depend on host hardware and runtime
policy; see [the container deployment guide](ai_docs/container-deployment.md).

## Validate changes

Start with the narrowest relevant test, then use the repository gates that the
host can actually support:

```bash
cargo test -p hermit-detcore
cargo test -p hermit
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

`cargo test --workspace` does not cover the complete historical Buck test
matrix. Report PMU, CPUID, namespace, ignored, quarantined, and unlanded cases
separately. `scripts/validate.rs` is the broader repository gate, but its
hardware-specific steps require a suitably provisioned host.

## Contribution flow

Hermit product changes use:

```text
feature branch -> pull request -> rrnewton/hermit:main
```

Use the `origin` remote for the maintained fork and `upstream` for
`facebookexperimental/hermit`. Do not push feature work directly to `main`.
Keep commits scoped, include exact validation evidence (the commands you ran and
their results), and leave any human-review hold in place even when CI is green.

Networked `git` and `gh` in a Meta environment must go through the proxy
wrapper:

```bash
with-proxy gh pr list -R rrnewton/hermit
```

## Documentation map

Design and reference notes live in [`ai_docs/`](ai_docs/README.md); dated,
situational investigation records are kept separately under
[`ai_docs/transient/`](ai_docs/transient/) and are not maintained. Start here:

- [Architecture overview](ai_docs/architecture-overview.md)
- [Container deployment](ai_docs/container-deployment.md)
- [QEMU integration status](ai_docs/qemu-integration-status.md)
- [Schedule search guide](ai_docs/schedule-search-guide.md)
- [PR status snapshot](ai_docs/pr-status.md)
- [Known limitations and future work](ai_docs/known-limitations.md)
- [Hermit v2 roadmap](ai_docs/hermit-v2-roadmap.md)
- [QEMU and virtme-ng host setup](ai_docs/qemu_vng_setup.md)
- [SaBRe assessment](ai_docs/sabre_backend_assessment.md)
- [KVM backend design](ai_docs/kvm_backend_design.md)
