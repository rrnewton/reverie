# What does `test.liteinst_strict` do under hermit#2384?

**Date:** 2026-08-24 · **Agent:** hermit-005 · **PR:** https://github.com/rrnewton/hermit/pull/2384

## Why this exists

`test.liteinst_strict` was the last validate node in hermit#2384's tally with no
number against it. Two earlier attempts to reach it were cut short — the first by
the DAG's eager-exit after an unrelated 29-second failure, the second by the
agent-utils `dagrun` rename breaking `scripts/validate.rs` before any node ran.
A pull request whose description claimed **three** red cells was heading for a
landing window with this node unmeasured.

**It needs no box lock.** It is a `cargo build` plus a `nextest` run, not a
validate, so it does not compete for the box-exclusive admission lock. That is
why it could be measured while the queue was busy.

## Method

The node's exact command from `ci/dag/portable.json`, run on both sides on the
same host, with the two prerequisites the node depends on built first:

```
cargo build --release --locked -p hermit --features third-party-backends
./scripts/stage-liteinst-runtime.sh release \
    "$PWD/target/release/libreverie_liteinst.so" "$PWD/target/liteinst-runtime-build"
HERMIT_LITEINST_TEST_BINARY=$PWD/target/release/hermit \
    ./ci/run-nextest-counted.sh -p hermit --features third-party-backends \
    --test liteinst_advanced -j 1
```

- base = `main` at the time, `8adea793e241`
- head = hermit#2384, `8279353335bb` (patch byte-identical at every rebase since)

## Result

| | tests run | passed | failed | exit |
|---|---|---|---|---|
| base | 23 | **23** | 0 | **0** |
| head | 23 | 4 | **19** | **100** |

Both prerequisites succeeded on both sides — `BUILD_EXIT=0`, `STAGE_EXIT=0` — so
this is a real measurement of the node, not a build or staging failure wearing
a test result. See `exits.txt`.

**RELEASE, not debug.** The 19 names are identical to those measured earlier in
the debug whole-package population, so these reds are a property of the change
and not of the build profile. That was recorded as a prediction *before* this
run and is confirmed by it.

## The 19 cells

`liteinst_detcore_strict_verify_micro_suite`,
`liteinst_strict_verify_encoding_and_digest_utilities`,
`liteinst_strict_verify_file_and_text_utilities`,
`liteinst_strict_verify_formatting_and_sequence_utilities`,
`liteinst_strict_verify_heap_growth_avoids_trampoline_mappings`,
`liteinst_strict_verify_identity_utilities`,
`liteinst_strict_verify_path_and_language_utilities`,
`liteinst_strict_verify_python_entropy`,
`liteinst_strict_verify_python_random_example`,
`liteinst_strict_verify_round2_arithmetic_and_predicate_utilities`,
`liteinst_strict_verify_round2_encoding_and_comparison_utilities`,
`liteinst_strict_verify_round2_representation_and_path_utilities`,
`liteinst_strict_verify_round3_encoding_and_compression_utilities`,
`liteinst_strict_verify_round3_portable_system_utilities`,
`liteinst_strict_verify_round3_stdin_filter_utilities`,
`liteinst_strict_verify_semantic_file_and_sqlite_utilities`,
`liteinst_strict_verify_semantic_text_utilities`,
`liteinst_strict_verify_shell_and_entropy_consumer`,
`liteinst_strict_verify_virtual_identity_and_time`.

These are **not 19 defects**. They are one defect — the memfd-trampoline-inode
divergence documented on the PR, where a host-global inode counter leaks into
guest-visible `/proc/self/maps` — now visible across the whole LiteInst
strict-verify suite because the change makes buffer content comparable.

## The completed tally

With this node measured, hermit#2384's blast radius across the three affected
validate nodes is:

| DAG node | base | head | newly red |
|---|---|---|---|
| `test.cli` | 43 / 0 | 41 / 2 | 2 |
| `test.app_strict_verify` | 8 / 0 | 7 / 1 | 1 |
| `test.liteinst_strict` | 23 / 0 | 4 / 19 | **19** |
| total | 74 / 0 | 52 / 22 | **22** |

The PR description said three. It now says 22 independent cells, 23 named, each
spelled out in full.

## Files

`exits.txt` per-phase exit codes · `run.sh` the driver · `base-test.txt` full base run
(named .txt because the repo ignores `*.log`) · `head-failures.txt` the failure lines extracted from the 1.9 MB head
log, which is not committed per repo-cleanliness.
