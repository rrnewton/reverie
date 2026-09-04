# How to rerun

Requirements: a readable-writable `/dev/kvm`, a host with CPUID faulting
(`arch_prctl(ARCH_SET_CPUID, 0)` returns 0), `cc`, and a checkout of
`rrnewton/hermit`. Roughly two minutes of build per leg on a large box.

## Two traps that will silently give you the wrong answer

**1. Do not measure in the shared parent checkout at
`/home/newton/work/dev-hermit/hermit`.** Its local `main` was frozen at
`78aabb5b6d26` on 2026-09-04, which is the pre-fix base. A measurement taken
there while believing it is main reproduces the old behaviour and reads as the
fix not working. Use a worktree at an explicit SHA.

**2. Do not put the probe binary under `/tmp`.** Hermit virtualizes the guest's
`/tmp`, so a binary at an absolute `/tmp` path is not necessarily the file the
guest opens. Build it inside the checkout.

Also: run hermit through the parent repository's `bin/safehermit`, never a bare
`target/debug/hermit`. Naming an absolute path does not consult `PATH`, so a
shim called `hermit` does not cover it.

## Build the probe

```bash
cd <hermit checkout>
mkdir -p ignored/review-probe
cp <this directory>/subleaf_probe.c ignored/review-probe/
cc -O2 -std=c11 -Wall -Wextra -Werror \
   ignored/review-probe/subleaf_probe.c -o ignored/review-probe/subleaf_probe
```

## The three legs

`$R` is the `dev-hermit` parent, `$S` the hermit checkout, `$H` its
`target/debug/hermit`, `$P` the probe just built.

```bash
cd "$S" && git checkout c3a85da26110fd8dfc31433c8d4109726127703b
cargo build --bin hermit

# head, ptrace
"$R"/bin/safehermit "$H" run --base-env=minimal --backend ptrace -- "$P"
# head, KVM
"$R"/bin/safehermit "$H" run --base-env=minimal --backend kvm    -- "$P"
```

The base leg is **"revert the two changed source files to the base SHA"**, not
"check out the base SHA". Checking out `4d8f8661` works too here, but reverting
just the two files keeps the rest of the tree identical, so anything that differs
is unambiguously the lookup change:

```bash
git checkout 4d8f866102882b6eeabdf99cc7e81433cc3c95c5 -- \
    detcore/src/cpuid.rs detcore/src/lib.rs
cargo build --bin hermit
"$R"/bin/safehermit "$H" run --base-env=minimal --backend ptrace -- "$P"

git checkout HEAD -- detcore/src/cpuid.rs detcore/src/lib.rs   # restore
cargo build --bin hermit
```

⚠️ `git checkout <sha> -- <paths>` **stages** those paths. `git checkout -- <paths>`
afterwards restores from the index and therefore restores the wrong content.
`git checkout HEAD -- <paths>` is the one that undoes it. Confirm with
`git status --short` before you trust the rebuilt binary.

⚠️ Rebuild between legs. A reverted source file with a stale binary measures the
binary you did not change.

## The manifest cell

```bash
cargo build --bin hermit --bin test-harness -p hermit-manifest-plan
target/debug/test-harness run --test backend-parity-c/cpuid-probe \
    --mode verify --backend ptrace
target/debug/test-harness run --test backend-parity-c/cpuid-probe \
    --mode verify --backend kvm
```

`test-harness` shells out to `target/debug/hermit-manifest-plan`, which
`cargo build --bin test-harness` alone does not produce; build the package.

To show the cell is not vacuous, apply the same two-file revert, rebuild, and
rerun the ptrace leg. It fails on both attempts with
`verify exited with status 125 before producing a terminal comparison`, which is
the guest's new `return 3` path.

## The unit-test coverage measurement

In `detcore/src/cpuid.rs`, replace the body of `InterceptedCpuid::cpuid`'s
subleaf branch with `let _ = subleaf; Some(result)` and run
`cargo test -p hermit-detcore --lib cpuid`. To show which assertions actually
discriminate, additionally remove the two leaf-`0x07` assertions from
`indexed_leaves_do_not_alias_subleaf_zero` and narrow its loop to
`[0x0d, 0x0f, 0x10, 0x12, 0x14]`. With the lookup still reverted, it passes.

## Reading the output

Only the `sub=1` lines carry the finding. Compare the base ptrace leg against the
head ptrace leg, then the head ptrace leg against the head KVM leg. See
`RESULTS.md` for the expected values and what they mean.
