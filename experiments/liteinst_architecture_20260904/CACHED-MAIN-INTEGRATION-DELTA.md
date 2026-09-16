# Cached main versus the preserved LiteInst bases

Read-only local-object inspection, 2026-09-04 PDT. No fetch, ref movement, merge/rebase, build, execution, product edit, or remote landing claim.

## Decision

**No newly available shared-preload/LiteInst/Tool mechanism appears between Reverie 320412c and cached main 37e7b727.** Those source trees are byte-identical. The named captured-output and refused-exec/vfork fixes, plus short socket paths, already exist in the old base. Do not replay them into the component branch as missing work.

Cached Reverie main changes only four DBT/KVM CPUID implementation/test files. Their path set has **zero intersection** with the preserved component changes from 320412c to 3e3e. That establishes no same-file conflict in these deltas, not a tested merge or semantic approval. The actual future integration issue is a unified Reverie revision and a typed Detcore artifact/CLI route; updating to cached main alone supplies neither SUD-only activation nor the unresolved signal/clock ownership contract.

## Exact anchors

| Repository | Preserved base/component | Cached origin/main inspected | Root checkout HEAD observed |
| --- | --- | --- | --- |
| Hermit | `4d8f866102882b6eeabdf99cc7e81433cc3c95c5` | `500dded1b2c614694ba97bdafe2d196c8ed63eb4` | `89e684c8ce72e8cf0ded0cd30506f90b9570d9fd` |
| Reverie | base `320412c5967790939ebe405c73e394ffd9c41459`; component `3e3e3e3a4b9db7a20ea8f7382441c6fe4b5a9af9` | `37e7b727a7f921d9c49bad4db6969297fc20f78a` | `320412c5967790939ebe405c73e394ffd9c41459` |

All source references below are `path:line` **at the indicated pinned commit**, not claims about mutable checkout line numbers. Raw evidence: `/home/newton/work/dev-hermit/ignored/cached-liteinst-integration-20260904/` (`anchors.txt`, `content-identities.tsv`, `equality-checks.txt`, small patches and numbered excerpts). Tree/blob identity and actual diffs establish presence; commit ancestry alone was not used to declare a squash-landed patch missing.

## Required capabilities already present before these deltas

- **Captured output:** `c2e2c8fbe52c2e5e8c65a56c2a547785f76c731e`. At 37e7b727, `reverie/src/backend.rs:259` requires real captured stdout/stderr, status, final global state and typed stats, with no empty default. `reverie-liteinst/src/backend.rs:618` uses the typed preload launcher and preserves raw wait status. Both files are byte-identical between c2e2c8fb and 320412c, and the generic Backend/Tool/Guest files are also unchanged at 3e3e. Existing test: `reverie-liteinst/tests/lifecycle.rs:221`. This is a reusable output contract, **not** evidence that Hermit's CLI selects it or that logs establish strict parity.
- **Refused exec/vfork cleanup:** `af42d9cf7ae604777cd88c5cca5b319460c986e8`. This is **host-hybrid ptrace lifecycle work**, not native in-process exec handoff. At 37e7b727, `reverie-ptrace/src/task.rs:4272` records a vfork refusal; `:4473` returns to session cleanup for refused post-start exec/vfork instead of detaching and losing wait ownership. `reverie-liteinst/src/backend.rs:205` permits hybrid clone/fork while retaining vfork refusal. Tests remain at `reverie-liteinst/tests/hybrid.rs:1202,1251,1282,1316`. The af42-to-old-base diff is not literally empty: later exhaustive event matching and ptrace timer tests exist; the refusal cleanup remains present. Old base versus cached main ptrace task/tracer blobs are identical.
- **Short socket paths:** production `3221c23100d0e219235758f5ff7d579e889c4b63`, tests `6aea377943234aa0a22a0dc2f60c2c76a2f297bb`. These are separate older commits, not a newly discovered part of af42. At 37e7b727, `reverie-liteinst/src/backend.rs:821` uses `tempdir_in("/tmp")` for coordinator/stats sockets rather than inheriting a long TMPDIR. Already included in 320412c and retained by the component.

## Actual new cached changes

Reverie commits `58c4ae9e7045d71cbd99d6d78104d6972d4a1346` and `37e7b727a7f921d9c49bad4db6969297fc20f78a` adjust **CPUID values/index selection in existing KVM/DBT paths**:

- `reverie-kvm/src/cpuid.rs:105` builds/validates the table through one helper; indexed standard leaves 4/7/0xb/0xd receive significant-index flags (`:143`). Redundant explicit zero xstate rows are removed; duplicate-key rejection is exercised through the real table builder. `reverie-kvm/tests/vmcall.rs:154` adds native VM leaf/subleaf observations and exact expected results.
- `reverie-dbt/native/client.c:1190` recognizes those indexed leaves and returns zero for unsupported nonzero subleaves; `reverie-dbt/tests/cpuid_probe.c` adds exact indexed-leaf assertions. These are **not reusable SIGSYS/CPUID-fault continuation machinery** and must not become a second Detcore instruction emulator in LiteInst.

Hermit commit `475f61d862db64de14ab1720c1110587f58ebc3e` advances the Reverie dependency pin from 320412c to 37e7b727. The complete declaration change covers `detcore-dbt/Cargo.toml`, `detcore-model/Cargo.toml`, `detcore-sabre/Cargo.toml`, `detcore/Cargo.toml`, `detcore/tests/testutils/Cargo.toml`, `hermit-cli/Cargo.toml`, `hermit-install/Cargo.toml`, and `liteinst-runtime-build/runtime/Cargo.toml`, plus root and standalone-runtime lockfiles. Other cached Hermit CI/demo/manifest changes are outside this source-routing check; their path list is retained, not treated as backend implementation.

## Route and linkage remain unchanged

At Hermit 500dded1, `hermit-cli/src/lib.rs:2682` calls `run_host_with_preload::<Detcore>`; captured output at `:2896` calls `run_host_with_output_and_preload::<Detcore>`. At Reverie 37e7b727, `reverie-liteinst/src/backend.rs:226` constructs **TracerBuilder**. The CLI's Detcore therefore remains host/ptrace-owned; common `Backend::run_with_output` is a different, already-existing typed path, not the selected CLI route.

The staged artifact remains `liteinst-runtime-build/runtime/Cargo.toml:8`, a constructor-enabled dependency on reverie-liteinst with **no Detcore dependency or typed Tool entry**. Its `src/lib.rs` is dependency-only. `scripts/stage-liteinst-runtime.sh:17,51,62` uses the declared pin, normal nested build, and sibling revision marker; `hermit-cli/build.rs:48` embeds the pin and `hermit-cli/src/lib.rs:804` enforces it. All those implementation bodies are unchanged from 4d8. A 3e3e-derived DSO cannot honestly be labeled 37e7b727 to pass that guard; a normal legacy stage at 37e7b727 does not produce the desired typed Detcore DSO.

At cached Reverie main, the old unpatchable-site refusal remains (`reverie-liteinst/src/runtime.rs:2794`), as do typed exec restrictions (`tool_host.rs:749`) and the old timer setters returning `Ok(())` without arming (`:974,980`). Do not restore these over the component's bounded fixes. The preserved 3e3e has additional SUD/fallback/clock/instruction-control source, but still expressly refuses CPUID/RDTSC/vDSO subscriptions in SUD-only (`tool_host.rs:223`) and SUD-only timer requests (`:1125`). Nothing in this cached delta removes those current qualification boundaries.

## Recommended sequence — no changes performed

1. Continue the separately owned signal/clock/stack work against the frozen component; this cached-main delta supplies no missing handoff mechanism and is not a reason to duplicate or interrupt it. Preserve source-bound reviews and existing SUD-only refusals.
2. When separately authorized, compose the four CPUID files with the complete component in an isolated integration tree. Inspect the result and retain patched/default regressions; there is no API adaptation implied by these cached deltas. Do not wholesale-replace preload/runtime from cached main or replay already-present c2/af/socket commits.
3. Select **one integrated Reverie commit containing both sets**, not 37e7b727 alone. Update all synchronized Hermit declarations and both lockfiles through the existing pin protocol. Preserve genuine binary/source identity and private build targets; no relabeling existing DSOs or changing revision markers manually.
4. Keep typed Detcore staging and deliberate CLI routing as separate reviewed integration work. Before activation, resolve instruction/vDSO subscriptions, initial-loader gap, timer/clock and signal ownership; then validate the exact CLI/DSO pair with existing strict logs, nonzero INFO counts and JSON parity. Output capture alone, generic Tool fixtures, hybrid lifecycle tests and this source comparison grant no Detcore/L2/90%-population credit.

## Scope of verification

Only local `git rev-parse/show/diff/ls-tree/log/grep`, file reads, and external report/evidence writes were used. No tests were executed or inferred green from commit messages. In the four-file CPUID delta, assertions remain exact: duplicate rejection moves to the production table builder and VM/DBT subleaf checks are added; deleting redundant zero table rows is not being reported as a passing test. This is a source-integration check, **not** independent approval of the component, a full CPUID audit, a current remote-main statement, or a landing receipt. Frozen 340 labels and prior failures are unchanged.
