# Anchored selection migration handoff

Task: `migrate-anchored-test-selection-off-run-dag-file-override`

The implementation is committed and independently approved at exact head
`72f16fe22a0fdd639832b69bfe4992b2699af784`, based on dev-hermit
`98945ac415701de4e603828f81c57c8cb67f4933`.

Only `scripts/anchored-test-selection.rs` changed. It replaces raw
`RUN_DAG_FILE_OVERRIDE` subset execution with validate's constructed-plan
`--selected` path, requires explicit `result_manifests` arrays, checks exact
closure/order, and restricts passthrough to execution-only options. Parent
help/self-test parsing stops at `--`.

Verification:

- Built-in self-test: 74 passed, 0 failed.
- `git diff --check`: passed.
- `rustfmt --edition 2021 --check scripts/anchored-test-selection.rs`: passed.
- Both `-h` and `--help`: passed.
- Subprocess negative controls for passthrough `--probe-host-capability`,
  `--help`, and `--self-test`: each exited 2 and never invoked the fake
  validator.
- Independent exact-head review: approved with no code findings.
- Full `with-proxy make lint` ran all gates. Task-relevant gates passed; the
  aggregate remained red on unrelated pre-existing parent-main hook/help,
  pin-drift, skill-layout, error-proxy, and path-portability findings. Exact
  counts are in the TaskGraph progress note.

Mandatory landing prerequisite:

- Current parent main pins Hermit
  `5279d6b16f4452d1b740091cbf20cf3aaeab4d0e`, whose constructed plans omit
  explicit `result_manifests`. The focused integration check selected 14/72
  steps and refused before execution on `pre.submodules`, exit 2 in 25.10s.
- Hermit PR https://github.com/rrnewton/hermit/pull/2971 remains open at
  `943a7f6da971c529bbdb051c7cf633a1759086e9`; it adds those declarations.
- Do not land this parent commit until that PR and the corresponding dev-hermit
  gitlink update are on main. TaskGraph currently records the PR landing task as
  blocked by this task; a coordination note describing the circular ordering
  constraint was added to that task.

Resume steps:

1. Read this handoff through `ci-hub/bin/wrkslots read-handoff` using the exact
   registered owner/generation required by the tool.
2. Fetch parent main and verify its `hermit` gitlink contains PR 2971's merged
   structured-result change.
3. Rebase this one commit. Its SHA will change, so obtain a fresh independent
   exact-head review.
4. Rerun the 74-check self-test and a selected constructed-plan integration
   check against the new pin.
5. Land with `ci-hub/bin/git-push-verified origin HEAD:main`, fetch main, and
   verify `scripts/anchored-test-selection.rs` by content.
6. Record the landing evidence, remove this handoff, finish the slot lifecycle,
   and close the TaskGraph task.
