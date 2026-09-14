# wrkslots creation repair and consumers, 2026-09-14

Inventory of the slot-creation repair, collected from tracked sources in
agent-utils, dev-hermit, Hermit, and Reverie. Paths are relative to the named
repository unless absolute; baseline line numbers precede the repair. Runtime
entry points, active callers, and historical references are distinguished below.

## Landed implementation and real slot creation

The implementation landed on
[agent-utils main at 14589e875c08e458d2f8af5a6f78b12128d5a367](https://github.com/rrnewton/agent-utils/commit/14589e875c08e458d2f8af5a6f78b12128d5a367).
The verified push succeeded, and a fresh fetch followed by a content diff of
all seven changed files against `origin/main` was empty. The primary
`hermit/agent-utils` checkout was clean before activation and is now clean at
that landed revision.

A non-coordinator agent created the real registered slot
`/home/newton/work/dev-hermit/worktrees/slots/wrkslots-create-demo-20260914`
on 2026-09-14 at 20:08 UTC. The persistent owner PID was 287750, start ticks
157538416; the live coordinator PID was 2073905, start ticks 157042304.
The owner's actual ancestry was `287750 -> 287506 -> 3180881 -> 1`.
Neither the owner nor its creation command descended from the coordinator.

The same owner first invoked the original implementation, `d14adc47`, against
the same project and assignment. It refused with exit 3 in 0.405 seconds:
`coordinator PID 2073905 is not in the invoking process ancestry`.
The slot directory, branch, and ACTIVE row remained absent. The fixed ordinary
`ci-hub/bin/wrkslots create` command then returned exit 0 in 33.438 seconds;
all creation checks finished in 34.176 seconds. Its input environment had no
proxy variables or `GIT_*` settings, and no outer `with-proxy` was used.
A live `/proc` sample also confirmed no proxy variables in the creation process.

The command fetched the real GitHub Hermit repository, created the assigned
branch at `7b8be60708c29b4926f36f06be8793523841d6ba`, and ran the configured
`git submodule update --init --recursive` hook. Both agent-utils and
third-party/rr were initialized and the new checkout was clean. Independent
readback confirmed ACTIVE generation 1, the exact live owner/coordinator
identities, Git's worktree registration, and the `slot-created` action in
`worktrees/EVENTS.devbig014/00000000000000006225.json` (event SHA-256
`327fb494dccd6c261daed4da0ced2e64f1e220fcdaf9498c60950d98fbef41a9`).
The command reported and preserved 79 unrelated existing registry/storage
findings. It did not repair or remove other slots.

The retained evidence directory is `/tmp/wrkslots-demo-20260914/`:
`baseline-result.json`, `baseline.stderr.log`, `before-create.json`,
`create-process.json`, `running-processes.json`, `create-result.json`,
`create.stdout.json`, `create.stderr.log`, `registered-row.json`,
`registered-events.json`, `git-checks.json`, and `verified.json`.
The registered slot is now used to prepare the project's agent-utils pin update.

## Safety property and history

The useful property is accurate ownership: an agent should not bind an unrelated
live process as a slot owner, renew somebody else's ownership, or use a reused PID
as if it were the recorded process. Slot deletion separately requires owner
absence and the existing process, filesystem, Git, and handoff evidence. A process
being an ancestor does not establish that it has the coordinator role.

The history establishes how these concerns became coupled:

- [agent-utils commit 6bfbf5160d37c609031d16a75baaa9521f1aaf01](https://github.com/rrnewton/agent-utils/commit/6bfbf5160d37c609031d16a75baaa9521f1aaf01),
  dated 2026-08-16, introduced `_capture_caller_process` and the `/proc` ancestry
  walk. Creation used it for both owner and coordinator, then rechecked their exact
  process identities before publishing. The same helper protected owner actions;
  removal also required the recorded coordinator. Its commit description emphasizes
  independent owner-absence and process-use checks before removal.
- [agent-utils commit 45b24f8bbccf6e971d97de041b874bc6f8e770b6](https://github.com/rrnewton/agent-utils/commit/45b24f8bbccf6e971d97de041b874bc6f8e770b6),
  dated 2026-08-28, added `--coordinator-authorized` as a reminder and recorded
  provenance, explicitly saying it was not a permission boundary between processes
  running under the same user. It let a later participant recover durable work
  without requiring the original coordinator, but retained creation's unconditional
  coordinator-ancestry check.
- [agent-utils commit 55c5a8fac3494bdf903c86390fb9e73c88f17eb5](https://github.com/rrnewton/agent-utils/commit/55c5a8fac3494bdf903c86390fb9e73c88f17eb5),
  dated 2026-09-11, let an invoking coordinator assign another live child as owner,
  while retaining self-binding, refusal of unrelated owners, and process-generation
  checks. This still required the coordinator to be an ancestor of creation, so it
  did not admit an assigned agent in a separate tmux pane.

The repair keeps those owner relationships and exact identity checks. An assigned
agent may bind itself or an ancestor of its command while recording a live
coordinator in a separate process tree. An unbound slot, or a slot assigned to
another owner, still requires an invoking coordinator; that other owner must
also descend from it. Both process identities are checked after the mutation lock
is acquired and before publication. The flag records the assignment convention;
the process checks establish ownership. Registration, heartbeat, removal, and
recovery authorization are unchanged.

The network defect was independent. `_GitVcs._run` invokes Git internally after
removing inherited `GIT_*` settings and disabling global/system Git configuration.
A caller's separately wrapped pre-check therefore did not affect the later fetch.
The repaired command invokes the installed `with-proxy` for fetch, push, remote
readback, and provisioning hooks, including recursive submodule setup. It preserves
Git configuration isolation. A wrapper failure remains a failure, with no direct
retry; a host without the wrapper uses the caller's network environment.

## Review and completed validation

The reviewed and validated agent-utils change is
`d14adc47ebc6dd7d1d758d5c3198c465d61ca703..14589e875c08e458d2f8af5a6f78b12128d5a367`.
The supplemental `WORKTREES.md` policy was reviewed as Git blob
`580c9aae5a458c7c3271521c208fcc5e2cd93e21`, based on parent commit
`9a8dbb208b2d8a29cc1adc9e5c5d631ff3be50e7`.

Adversarial review found one missed consumer before approval:
`py/tests/test_packaging_infrastructure.py` hardcoded the lifecycle test counts
and exact set of tests run outside the PID namespace. The added cases changed
644 total / 97 ordinary / 547 mapped into 653 / 98 / 555. The final revision
updates those exact counts and adds the test name for owner and coordinator in
separate processes, preserving disjointness, complete coverage, and every existing
expected name. Independent
verification of that guard passed: 1 passed, 15 deselected in 1.17 seconds.
No unresolved review finding remains at the reviewed revision.

The review explicitly checked for weakened assertions, widened tolerances,
skipped cases, relaxed comparisons, and failures relabelled as successes. None
was introduced to make the change pass. The old test for an owner process generation
change retains its refusal, preserved checkout and journal, and empty ACTIVE row assertions; its
replacement fixture changes identity only after checkout creation and leaves
real ancestry checks active. The creation policy change is the explicit requested
replacement of the unsatisfiable coordinator condition, with positive and negative
owner cases covered. Marking the new process test `ordinary_environment` selects
the existing host-visible test phase rather than skipping it.

`with-proxy make validate` completed at the exact revision above with exit 0,
reported by coordinator execution session 91670. The inspected log ends with
`validate: OK`:

`/tmp/wrkslots-agent-utils-validate-final-20260914.log`

The log is 91,248 bytes, SHA-256
`9b4707e68e1d61745cc9fa50932ed184ac021381101b5ca0099ce5df493afa5a`.
Its test summaries give these counts and durations:

| Validation phase | Result | Reported test duration |
| --- | --- | --- |
| Python suite excluding the separate lifecycle file | 2,714 passed; 2 warnings | 504.98 seconds (8 minutes 24.98 seconds) |
| Lifecycle tests inside the PID namespace | 555 passed; 98 selected for the other phase | 824.88 seconds (13 minutes 44.88 seconds) |
| Lifecycle tests outside the PID namespace | 98 passed; 555 selected for the other phase | 203.78 seconds (3 minutes 23.78 seconds) |
| Rust unit and integration tests | 871 passed; 0 failed; 0 ignored | 89.46 seconds summed across the 38 Rust result summaries |

The three disjoint Python phases total 3,367 passing tests and 1,533.64 seconds
(25 minutes 33.64 seconds). Python plus Rust totals 4,238 passing tests. The two
Python warnings came from the existing supervisor-crash tests intentionally
raising exceptions in their worker threads; their tests passed and the warnings
remain visible in the log. The log does not record total command wall time, so
these phase durations are not presented as the complete validation duration or
as evidence of a performance change.

The same completed command also passed documentation/link checks, Rust formatting,
setup/build, lint and strict type checks, repository hygiene and validation-selection
checks, and all package checks: 7 Python distributions and 4 Rust crates. The six
Python/Rust comparison results report 1,491 checks in total: dagrun 763,
cpuset-alloc 58, tick-hub 106, pr-landing-planner 286, herdr-run 228, and
herdr-agent 50. The log retains the comparison suite's existing coverage notes:
herdr-run's external fake-server protocol checks were not run, cases requiring a
live pane are covered by unit tests in each implementation, and NUL cannot be
represented in a POSIX command argument. Those limitations are not new exemptions
introduced by this change.

The new network tests use local Git repositories and a controlled executable
wrapper. They cover routing of network commands, preserved Git isolation, recursive
submodules and child environments, wrapper failure without direct retry, hook
failure and recovery, and operation without an installed wrapper.

## Consumers affected by the finalized repair

The coordinator finalized creation-only authorization, network wrapping only for
fetch/push/ls-remote and post-provision hooks, common documentation, tests, and
WORKTREES.md policy. Under that scope:

- The final creation helper `_assert_create_owner_authorized` is called directly by
  `_cmd_create` and `_assert_create_processes`; the latter is called twice by
  `_cmd_create`, after the lock and before publication. Its public entry point is
  `create`; all eight executable creation consumers enumerated below reach it.
  `_recover_create` consumes the resulting existing journal/identity representation.
  The shared `_capture_caller_process` and `_assert_caller_process` consumers listed
  below retain their current behavior when those helpers themselves are unchanged.
- The network helper is called by `_GitVcs._run` for only `fetch`, `push`, and
  `ls-remote`, and separately by `_run_post_provision_hooks`. The exact direct network
  method consumers are `_GitVcs.fetch_remote`, `_GitVcs.push_salvage`, and
  `_GitVcs.remote_ref_sha`. Every direct method caller is listed in the helper section.
  Commands reaching these network methods are `create`, `recover-unbound-owner`,
  `unpushed`, `finish`, `remove`, `remove-validate-batch`,
  `recover-ownerless-agent-worktree`, `recover-absent-agent-row`,
  `recover-ownerless-validate-batch`, `classify-ownerless-validate-batch`, and `recover`.
  Creation/recovery hooks add no additional public command beyond that set.
- `_run_post_provision_hooks` has exactly two direct callers, `_cmd_create` and
  `_recover_create`. Its configured project consumer is `.wrkslots.yml`'s recursive
  submodule update. The package permits any configured hook; the hook environment
  variables are consumed by project commands rather than a fixed built-in list.
- README and USER_GUIDE changes feed `scripts/embed_userguides.py`, the installed
  guide output loaded by `py/wrkslots/cli.py`, their package symlinks, setuptools package
  data, `--userguide`, and `py/wrkslots/tests/test_cli.py`'s equality check.
- The affected package tests are `py/wrkslots/tests/test_cli.py`,
  `py/wrkslots/tests/test_lifecycle.py`, and the new
  `py/wrkslots/tests/test_network.py`. `Makefile`, `scripts/validate.py`,
  `scripts/check_python_packages.py`, and `py/tests/test_packaging_infrastructure.py`
  consume their result/selection contract. That last file also enumerates exact
  lifecycle counts and the ordinary-environment function set: the final revision
  changes 644/97/547 to 653/98/555 and adds the new test with owner and coordinator in separate processes. Actual read-only
  pytest collection confirmed those values during review. New tests do not imply new production
  callers.
- WORKTREES.md is read as policy by coordinators/agents and referenced by the retired
  allocation interfaces. Its delegated-agent creation instructions must agree with
  the package's command help and canonical common docs. The old skill text remains
  an enumerated policy consumer unless the owner-authorized fix updates it separately.
- Hermit and parent Git pins are a separate deployment role, not a new authorization
  or network API. Their checkout/verification consumers are appended below so a
  later pin update can be checked independently of the scoped behavior change.

## Runtime and deployment

The implementation used by this project is `agent-utils/py/wrkslots/cli.py` inside
`dev-hermit/hermit/agent-utils`. `dev-hermit/agent-utils` is a symlink to
`hermit/agent-utils`; it is not the separate `/home/newton/work/agent-utils` checkout.

The entry points and installation consumers are:

- `dev-hermit/ci-hub/bin/wrkslots`: sets PYTHONPATH to its own checkout's
  `hermit/agent-utils/py`, then invokes `python3 -m wrkslots`. It resolves mutable
  project state from the parent Git common directory unless `--project-root` was
  explicitly supplied. Creation falls through directly; removal/recovery has a
  separate systemd host-process handoff and ancestry checks in this launcher.
- `dev-hermit/worktrees/wrkslots`: existing untracked init-generated symlink to
  `../hermit/agent-utils/py/wrkslots/__main__.py`; bypasses the project launcher.
- `agent-utils/py/wrkslots/__main__.py`: imports `wrkslots.cli.main`; supports direct
  execution and `python3 -m wrkslots`.
- `agent-utils/py/wrkslots.py`: compatibility shim for older initialized projects;
  imports the same `wrkslots.cli.main`.
- `agent-utils/py/bin/wrkslots`: tracked symlink to `../wrkslots/__main__.py`.
- `agent-utils/common/bin/wrkslots`: tracked symlink to `engine-resolver`.
- `agent-utils/common/bin/engine-resolver`: selects the Python entry point by
  command basename; no Rust wrkslots implementation exists.
- `agent-utils/setup`: wires the repository's top-level bin link to common/bin.
- `agent-utils/py/wrkslots/pyproject.toml`: installed console script
  `wrkslots = "wrkslots.cli:main"`; includes README, USER_GUIDE, examples and typing.
- `agent-utils/py/wrkslots/cli.py::_cmd_init`: creates the project command link;
  repairs a legacy flat `py/wrkslots.py` link to the package entry point.
- `dev-hermit/hermit` Git submodule pins the Hermit tree; `hermit/agent-utils`
  gitlink pins the implementation used by the project launcher. Both pin levels
  matter for committed deployment. The separate standalone checkout's origin/main
  agrees with the runtime implementation at the inspected baseline, although its
  checked-out branch is much older.
- `hermit/.gitmodules` describes the agent-utils submodule source.
- `hermit/.github/workflows/ci-dag.yml`, `ci-portable.yml`, `ci-privileged.yml`,
  and `docs.yml` initialize the agent-utils gitlink. These consume the repository
  pin generally, not wrkslots creation specifically.

## Executable creation consumers

There are eight tracked production callers across the searched parent and product
repositories. This is the enumerated set, not an estimate:

1. `dev-hermit/ci-hub/validate/start_unit.py::prepare_fresh_checkout` (command at
   line 1794): validation checkout, JSON creation result parsed by
   `parse_wrkslots_create_result`; coordinator and owner are `os.getpid()`.
2. `dev-hermit/ci-hub/series/pressure_schedule.py::run_campaign` (line 432):
   scheduled pressure-test validation checkout; both identities are `os.getpid()`.
3. `dev-hermit/ci-hub/stress/burst-build.sh` (line 58): both identities are `$$`.
4. `dev-hermit/ci-hub/stress/matched-burst.sh` (line 90): both identities are `$$`.
5. `dev-hermit/ci-hub/stress/stress-burst` (line 115): --build-at checkout;
   both identities are `$$`.
6. `dev-hermit/multisect/multisect-probe` (line 120): probe validation checkout;
   both identities are `$$`; permits PROBE_WRKSLOTS override.
7. `dev-hermit/multisect/multisect-dag-probe` (line 106): DAG probe validation
   checkout; both identities are `$$`; permits PROBE_WRKSLOTS override.
8. `hermit/ci/check-derived-current-after-merge.sh` (line 158): derived-file
   validation checkout; both identities are `$$`; discovers the parent launcher.

These callers already claim coordinator authorization for their owned disposable
checkouts. They do not pass an external ORC coordinator PID. All also run removal
through the wrkslots interface afterward. No tracked reverie source matched
wrkslots, WRKSLOTS, or coordinator-pid in the searched checkout.

## Creation policy, instructions, and generated documentation

- `dev-hermit/WORKTREES.md`: primary creation policy and copyable command; explicitly
  permits a delegated subagent to use the command after coordinator assignment,
  while still showing an external COORDINATOR_PID.
- `dev-hermit/AGENTS.md`: explicit write destination, separate slot/validation roots,
  liveness before moving/removing a slot; clean detached worktree instructions for
  parent and agent-utils landings.
- `dev-hermit/scripts/slot-init.sh`: retired allocator; prints a wrkslots create
  command including --coordinator-authorized and --coordinator-pid.
- `dev-hermit/scripts/allocate-worktree.rs`: retired allocator in configured projects;
  refuses and tells the caller to ask the coordinator to run wrkslots create.
- `dev-hermit/scripts/check-agent-slot.sh`: reads status for assignment; its remedies
  tell agents to ask the coordinator for creation and assignment.
- `dev-hermit/scripts/release-worktree.rs`: retired removal interface; directs callers
  to wrkslots remove/recover.
- `agent-utils/common/docs/wrkslots/README.md`: quick start and creation guidance.
- `agent-utils/common/docs/wrkslots/USER_GUIDE.md`: lifecycle, owner relation, explicit
  authorization reminder, remote identity, and recovery rules.
- `agent-utils/common/docs/wrkslots/RELATED_WORK.md`: design background, not a caller.
- `agent-utils/py/wrkslots/README.md` and `USER_GUIDE.md`: tracked symlinks to common docs.
- `agent-utils/py/wrkslots/cli.py`: loads the installed USER_GUIDE.md through
  `_load_userguide`; quickstart, argparse help, and actionable refusal text are
  additional human-facing consumers. The guide is not copied into a Python string.
- `agent-utils/scripts/embed_userguides.py`: canonical documentation and package-document
  symlink checker; wrkslots common documents are linked into its package, so edits
  do not require a separate generated Python string update.
- `agent-utils/skills/wrkslots/SKILL.md`: creation command and owner/coordinator policy,
  post-provision hooks, and lifecycle guidance. Inspected as a consumer; not changed.
- `agent-utils/skills/README.md`: advertises/installs the wrkslots skill.
- `agent-utils/skills/wrkslots/agents/openai.yaml`: prompt advertising the skill.
- `agent-utils/py/README.md`: package installation table and command.
- `dev-hermit/ci-hub/validate/README.md`, `ci-hub/stress/README.md`,
  `multisect/README.md`, `multisect/DESIGN.md`, and
  `ci-hub/landing/staged-freshness.sh`: describe registered disposable checkouts.
- `dev-hermit/multisect/probes/detcore_misc.env`: describes the source repository
  whose commits wrkslots materializes for each repetition.
- `hermit/hermit-cli/src/lib.rs`: worktree path handling comment refers to wrkslots;
  not a creation API caller.

## Project configuration and downstream lifecycle consumers

`dev-hermit/.wrkslots.yml` configures flat layout, worktrees/slots, origin/main,
agent liveness, and the post-provision hook `git submodule update --init --recursive`.
The hook launches Git separately from `_GitVcs._run`, through `/bin/sh -c`, and copies
os.environ. Fixing only fetch_remote would not automatically make that hook use a
proxy. Changing the project launcher environment affects both code paths.

The project launcher also serves non-create consumers; changing its global behavior
therefore reaches all of these exact paths:

- `ci-hub/bin/wrkslots-audit` invokes audit and publishes the audit result.
- `ci-hub/health/wrkslots_doctor_findings.py` invokes doctor.
- `scripts/check-agent-slot.sh` invokes status.
- All eight production creation callers above invoke later lifecycle operations;
  `ci-hub/validate/start_unit.py` also invokes status and recovery/batch cleanup.
- `ci-hub/health/removal_handoff_writer.py` supplies the launcher removal handoff.
- `scripts/systemd/hermit-wrkslots-audit.service` runs the audit through the installed
  operational-tool bootstrap; `scripts/systemd/hermit-wrkslots-audit.timer` schedules it.
- `scripts/install-wrkslots-audit.sh` installs/verifies the audit units, bootstrap,
  and GitHub credential helper.
- `ci-hub/health/tick-hub.yaml` consumes the audit result and invokes the doctor gate.
- `ci-hub/health/wrkslots_audit_contract.py` and `wrkslots_audit_result.py` parse
  downstream audit evidence.

The unchanged registry/schema has additional readers: `ci-hub/lib/worktree_registry.py`,
`ci-hub/health/slot_agent_parity.py`, `ci-hub/health/slot_disk_residue.py`,
`ci-hub/health/residue_sweep.py`, `ci-hub/validate/run_registry.py`,
`ci-hub/validate/migrate_run_identities.py`,
`ci-hub/ledger/validation_cleanup_publication.py`, and `ci-hub/ci-hub.rs`.
`ci-hub/bin/git-push-verified` contains a registry-preservation warning/example.
`multisect/multisect` retains historical worktrees rather than bypassing lifecycle.
Their source references are listed in the complete tracked-match appendix below.
Old coordinator_lease fields in the retired allocator's worktree-state.json are
not the packaged wrkslots registry contract and were not counted as current creation
consumers merely because they use the same field name.

## Git and process helper consumers

`_GitVcs._run` removes every inherited GIT_* variable, disables global and system Git
configuration, and retains other environment entries including standard proxy
variables. Its network methods are fetch_remote, remote_ref_sha (ls-remote), and
push_salvage (push). Thus a proxy set only through GIT_CONFIG_* or global git config
will not survive this boundary. Post-provision hooks copy os.environ separately.

The exact function callers below were derived from Python's AST, not text estimates.
They describe baseline d14adc47ebc6dd7d1d758d5c3198c465d61ca703; the final callers of the changed helpers
 are listed separately near the end. All are in agent-utils/py/wrkslots/cli.py. Transitive command lists are static call-graph
results; command dispatch/recovery decisions still determine which branch runs.

### _capture_caller_process

Direct callers:

- `_capture_remove_processes` at line 7582.
- `_cmd_create` at line 9846.
- `_cmd_register` at line 10675.
- `_cmd_import_existing` at line 10773.
- `_cmd_adopt` at line 10963.
- `_cmd_recover_unbound_owner` at line 11023.
- `_cmd_heartbeat` at line 11086.
- `_cmd_read_handoff` at line 11187.
- `_cmd_finish` at line 14383.
- `_cmd_recover_ownerless_agent_worktree` at line 19877.
- `_cmd_recover_ownerless_agent_cache` at line 20147.
- `_cmd_recover_absent_agent_row` at line 20596.
- `_cmd_recover_absent_validate_rows` at line 23225.
- `_cmd_recover_ownerless_validate_batch` at line 23414.
- `_cmd_recover` at line 23913.

Commands reaching this function: `create`, `register`, `import-existing`, `adopt`, `recover-unbound-owner`, `heartbeat`, `read-handoff`, `finish`, `remove`, `remove-validate-batch`, `recover-ownerless-agent-worktree`, `recover-ownerless-agent-cache`, `recover-absent-agent-row`, `recover-absent-validate-rows`, `recover-ownerless-validate-batch`, `recover`.

### _assert_caller_process

Direct callers:

- `_capture_caller_process` at line 7515.
- `_assert_remove_processes` at line 7616.
- `_assert_registration_owner_authorized` at line 7675.
- `_assert_create_owner_authorized` at line 7693.
- `_cmd_create` at line 9846.
- `_register_existing` at line 10609.
- `_cmd_import_existing` at line 10773.
- `_assert_owner_auth` at line 10940.
- `_cmd_adopt` at line 10963.
- `_cmd_recover_unbound_owner` at line 11023.
- `_cmd_read_handoff` at line 11187.
- `_cmd_remove` at line 14444.
- `_recover_finish` at line 15809.
- `_recover_legacy_validate_remove` at line 16209.
- `_recover_ownerless_validation` at line 19257.
- `_recover_ownerless_agent` at line 19795.
- `_recover_ownerless_agent_cache` at line 20079.
- `_recover_absent_agent_row` at line 20497.
- `_recover_absent_validate_rows` at line 23033.
- `_cmd_recover` at line 23913.

Commands reaching this function: `create`, `register`, `import-existing`, `adopt`, `recover-unbound-owner`, `heartbeat`, `read-handoff`, `finish`, `remove`, `remove-validate-batch`, `recover-ownerless-agent-worktree`, `recover-ownerless-agent-cache`, `recover-absent-agent-row`, `recover-absent-validate-rows`, `recover-ownerless-validate-batch`, `recover`.

### _assert_create_owner_authorized

Direct callers:

- `_cmd_create` at line 9846.

Commands reaching this function: `create`.

### _run_post_provision_hooks

Direct callers:

- `_cmd_create` at line 9846.
- `_recover_create` at line 15384.

Commands reaching this function: `create`, `recover-ownerless-validate-batch`, `recover`.

### fetch_remote

Direct callers:

- `_assert_checkout_safe` at line 8317.
- `_salvage_one_checkout` at line 8592.
- `_salvage_ownerless_checkout` at line 8666.
- `_assert_salvage_still_matches` at line 8757.
- `_create_plan` at line 9421.
- `_cmd_unpushed` at line 12516.
- `_validation_checkout_facts` at line 17406.
- `_assert_ownerless_salvage_still_matches` at line 19746.

Commands reaching this function: `create`, `recover-unbound-owner`, `unpushed`, `finish`, `remove`, `remove-validate-batch`, `recover-ownerless-agent-worktree`, `recover-ownerless-validate-batch`, `classify-ownerless-validate-batch`, `recover`.

### remote_ref_sha

Direct callers:

- `push_salvage` at line 7072.
- `_assert_salvage_still_matches` at line 8757.
- `_assert_ownerless_salvage_still_matches` at line 19746.
- `_recover_absent_agent_row` at line 20497.

Commands reaching this function: `remove`, `remove-validate-batch`, `recover-ownerless-agent-worktree`, `recover-absent-agent-row`, `recover-ownerless-validate-batch`, `recover`.

### push_salvage

Direct callers:

- `_salvage_one_checkout` at line 8592.
- `_salvage_ownerless_checkout` at line 8666.
- `_recover_absent_agent_row` at line 20497.

Commands reaching this function: `remove`, `remove-validate-batch`, `recover-ownerless-agent-worktree`, `recover-absent-agent-row`, `recover-ownerless-validate-batch`, `recover`.

### _run

Direct callers at the baseline, with the actual call lines:

- `_GitVcs.repository_root` at line 6681.
- `_GitVcs.common_directory` at line 6688.
- `_GitVcs.verify_ref` at line 6696.
- `_GitVcs.check_branch_name` at line 6704.
- `_GitVcs.branch_exists` at line 6709.
- `_GitVcs.add_worktree` at line 6723.
- `_GitVcs.listed_worktrees` at line 6730.
- `_GitVcs.worktree_registration` at line 6742.
- `_GitVcs.head` at line 6793.
- `_GitVcs.branch` at line 6800.
- `_GitVcs.status` at line 6818.
- `_GitVcs.tracked_cache_paths` at lines 6839, 6840.
- `_GitVcs.submodule_paths` at lines 6856, 6857.
- `_GitVcs.initialized_submodules` at line 6869.
- `_GitVcs.assert_ordinary_index` at lines 6889, 6895.
- `_GitVcs.assert_ordinary_history` at line 6908.
- `_GitVcs.fetch_remote` at line 6923.
- `_GitVcs.remote_url_sha256` at line 6937.
- `_GitVcs.assert_remote_url` at line 6947.
- `_GitVcs.operation_paths` at line 6968.
- `_GitVcs.remote_refs_containing` at line 6983.
- `_GitVcs.ref_exists` at line 6998.
- `_GitVcs.commit_paths` at line 7008.
- `_GitVcs.is_ancestor` at line 7025.
- `_GitVcs.remove_worktree` at line 7048.
- `_GitVcs.remote_ref_sha` at line 7053.
- `_GitVcs.push_salvage` at line 7083.
- `_GitVcs.delete_branch_at` at line 7100.
- `_GitVcs.repair_worktree` at line 7103.
- `_salvage_candidate` at lines 8417, 8457, 8459, 8460, 8473, 8479, 8493, 8499, 8525.

## Tests and validation wiring

Creation/ownership/network behavior is exercised by:

- `agent-utils/py/wrkslots/tests/test_lifecycle.py`: command helper auto-adds creation
  authorization and slot type for normal tests; dedicated refusal cases use the raw
  command. Current tests explicitly accept coordinator-descended owners, reject
  unrelated live owners, and check PID generation changes.
- `agent-utils/py/wrkslots/tests/test_cli.py`: installed CLI, compatibility shim,
  quickstart, help wording, packaged guide equality. It asserts the current phrase
  "another child of the invoking coordinator" in create help.
- `agent-utils/py/wrkslots/tests/conftest.py`: lifecycle-test environment/markers.
- `agent-utils/py/wrkslots/tests/e2e_stress.py`, `test_e2e_stress.py`, `mock_agent.py`:
  real subprocess lifecycle stress coverage and agent behavior.
- `agent-utils/py/tests/test_packaging_infrastructure.py`: lifecycle test partitions,
  CLI packaging and Makefile checks.
- `agent-utils/py/tests/test_repo_dispatch_wiring.py`: repository command dispatch.
- `agent-utils/scripts/check_python_packages.py`: wrkslots packaging smoke tests.
- `agent-utils/scripts/check_deps.py`: package dependency checks.
- `agent-utils/scripts/validate.py`: changed-path validation selection.
- `agent-utils/Makefile`: Python suite and ordinary/namespace lifecycle partitions.
- `dev-hermit/scripts/test_wrkslots_launcher.py`: project launcher, host removal handoff,
  and integration against the real package/lifecycle fixture. A test named
  nonancestor_coordinator currently supplies a nonexistent PID, so it checks unreadable
  process identity rather than a live sibling process.
- `dev-hermit/ci-hub/validate/tests/test_start_unit.py`: exact launcher, create arguments,
  JSON result, failure propagation, registry identity and cleanup.
- `dev-hermit/ci-hub/series/tests/test_pressure_schedule.py`: pressure-test lifecycle.
- `dev-hermit/ci-hub/tests/test_worktree_interface.py`: project interface and retired
  allocator wiring, stress/probe wrappers.
- `dev-hermit/ci-hub/tests/test_agent_facing_messages.py`: actionable refusal guidance.
- `dev-hermit/scripts/check-agent-slot-test.sh`: status-based assignment guard.

All remaining matching downstream tests, installation tests, ledger/registry tests,
and historical references are enumerated below rather than summarized by a count.

## Existing proxy patterns inspected

- `dev-hermit/ci-hub/bin/agent-tool::run_networked`: prepend with-proxy if present,
  otherwise invoke the command directly.
- `dev-hermit/ci-hub/bin/gh-merge-verified::gh_argv`: same conditional prefix for GitHub.
- `dev-hermit/ci-hub/lib/fetch_ref.py::_git`: explicit use_proxy default true on network
  fetch/read; sanitized Git environment.
- `dev-hermit/ci-hub/lib/validate_lock.rs`: conditional with-proxy around fetch.

No network operation was performed for this inventory.

## Search provenance and complete tracked-match inventory

The search was `git grep -l` for wrkslots, WRKSLOTS, and coordinator-pid in each
repository's tracked worktree sources; tracked symlinks were checked separately with
`git ls-files -s`. Focused helper-name searches and an AST walk supplied the function
lists. The search did not recursively traverse slot or validation worktrees. Dated
records below are historical references rather than live API consumers; they remain
listed so no match is hidden behind an estimated caller count. The registered slot
state itself was not read or changed.

### dev-hermit

Checked-out HEAD: `e4752ebaa9bc15352217f49ee4581359c594b747`.

- `.gitignore`
- `HANDOFF_codex_coord_to_orc_20261014.md`
- `SALVAGE-RECLAIM-TABLE-20260819.md`
- `WORKTREES.md`
- `ai_docs/SLOT_RECLAIM_LEASE_GATE_20260815.md`
- `ai_docs/backlog-triage-2026-09-07/actionable-ranked.tsv`
- `ai_docs/backlog-triage-2026-09-07/partition.tsv`
- `ai_docs/evidence/branch-deletions-20260910/deleted-branches.tsv`
- `ai_docs/evidence/tmp-worktree-unpushed-inventory-20260827/before.hermit_agent-utils.txt`
- `ai_docs/worktree-tooling-migration-plan-20260824.md`
- `ci-hub/bin/git-push-verified`
- `ci-hub/bin/wrkslots`
- `ci-hub/bin/wrkslots-audit`
- `ci-hub/ci-hub.rs`
- `ci-hub/health/removal_handoff_writer.py`
- `ci-hub/health/residue_sweep.py`
- `ci-hub/health/slot_agent_parity.py`
- `ci-hub/health/slot_disk_residue.py`
- `ci-hub/health/tests/test_operational_health.py`
- `ci-hub/health/tests/test_slot_disk_residue.py`
- `ci-hub/health/tests/test_wrkslots_audit_result.py`
- `ci-hub/health/tests/test_wrkslots_doctor_findings.py`
- `ci-hub/health/tick-hub.yaml`
- `ci-hub/health/wrkslots_audit_contract.py`
- `ci-hub/health/wrkslots_audit_result.py`
- `ci-hub/health/wrkslots_doctor_findings.py`
- `ci-hub/landing/staged-freshness.sh`
- `ci-hub/ledger/tests/test_validation_cleanup_publication.py`
- `ci-hub/ledger/validation_cleanup_publication.py`
- `ci-hub/lib/tests/test_worktree_registry.py`
- `ci-hub/lib/worktree_registry.py`
- `ci-hub/series/pressure_schedule.py`
- `ci-hub/series/tests/test_pressure_schedule.py`
- `ci-hub/stress/README.md`
- `ci-hub/stress/burst-build.sh`
- `ci-hub/stress/matched-burst.sh`
- `ci-hub/stress/stress-burst`
- `ci-hub/tests/test_agent_facing_messages.py`
- `ci-hub/tests/test_health_tick_evaluation_provenance.py`
- `ci-hub/tests/test_slot_agent_parity.py`
- `ci-hub/tests/test_tick_frequency.py`
- `ci-hub/tests/test_worktree_interface.py`
- `ci-hub/tests/test_wrkslots_audit_job.py`
- `ci-hub/validate/README.md`
- `ci-hub/validate/migrate_run_identities.py`
- `ci-hub/validate/run_registry.py`
- `ci-hub/validate/start_unit.py`
- `ci-hub/validate/tests/test_migrate_run_identities.py`
- `ci-hub/validate/tests/test_run_registry.py`
- `ci-hub/validate/tests/test_start_unit.py`
- `ledger/hermit/devbig014/2026-08.jsonl`
- `ledger/hermit/devbig014/2026-09.jsonl`
- `ledger/hermit/devbig030/2026-09.jsonl`
- `ledger/reverie/devbig014/2026-09.jsonl`
- `multisect/DESIGN.md`
- `multisect/README.md`
- `multisect/multisect`
- `multisect/multisect-dag-probe`
- `multisect/multisect-probe`
- `multisect/probes/detcore_misc.env`
- `scripts/allocate-worktree.rs`
- `scripts/check-agent-slot-test.sh`
- `scripts/check-agent-slot.sh`
- `scripts/install-wrkslots-audit.sh`
- `scripts/release-worktree.rs`
- `scripts/slot-init.sh`
- `scripts/systemd/hermit-wrkslots-audit.service`
- `scripts/systemd/hermit-wrkslots-audit.timer`
- `scripts/test_install_github_helper_source.py`
- `scripts/test_parent_main_write.py`
- `scripts/test_wrkslots_audit_provenance.py`
- `scripts/test_wrkslots_launcher.py`
- `scripts/tests/test_hermit_box_run.py`
- `scripts/tests/test_install_wrkslots_audit.py`
- `series/hermit/devbig014/2026-09.jsonl`

### runtime agent-utils

Checked-out HEAD: `d14adc47ebc6dd7d1d758d5c3198c465d61ca703`.

- `Makefile`
- `common/bin/engine-resolver`
- `common/docs/wrkslots/README.md`
- `common/docs/wrkslots/RELATED_WORK.md`
- `common/docs/wrkslots/USER_GUIDE.md`
- `py/README.md`
- `py/tests/test_packaging_infrastructure.py`
- `py/tests/test_repo_dispatch_wiring.py`
- `py/tick_hub/cadence.py`
- `py/wrkslots.py`
- `py/wrkslots/__main__.py`
- `py/wrkslots/cli.py`
- `py/wrkslots/examples/liveness_probe.py`
- `py/wrkslots/pyproject.toml`
- `py/wrkslots/tests/conftest.py`
- `py/wrkslots/tests/e2e_stress.py`
- `py/wrkslots/tests/mock_agent.py`
- `py/wrkslots/tests/test_cli.py`
- `py/wrkslots/tests/test_e2e_stress.py`
- `py/wrkslots/tests/test_lifecycle.py`
- `scripts/check_deps.py`
- `scripts/check_python_packages.py`
- `scripts/embed_userguides.py`
- `skills/README.md`
- `skills/wrkslots/SKILL.md`
- `skills/wrkslots/agents/openai.yaml`

### hermit

Checked-out HEAD: `7b8be60708c29b4926f36f06be8793523841d6ba`.

- `ci/check-derived-current-after-merge.sh`
- `hermit-cli/src/lib.rs`

### reverie

Checked-out HEAD: `0771253923fac12de6b1e1352227318548165c70`.

No tracked matches.

### standalone agent-utils

Checked-out HEAD: `cc99ffd7965800ecf8629094bbec7b9b146504aa`.

- `py/tests/test_wrkslots.py`
- `py/wrkslots.py`
- `skills/README.md`
- `skills/wrkslots/SKILL.md`
- `skills/wrkslots/agents/openai.yaml`

## Pin propagation and general submodule consumers

These are existing consumers of the whole agent-utils pin, retained here as a
separate unchanged deployment role. They were not counted as wrkslots callers just
because they initialize, verify, compile, package, or invoke another agent-utils tool.
The matched paths are exact; code references to unrelated tools do not mean those
tools use the changed creation or network helper.

### Parent scripts and installed tools that mention the pin

- `.github/workflows/dev-hermit-ci.yml`
- `ci-hub/bin/agent-tool`
- `ci-hub/bin/git-push-verified`
- `ci-hub/bin/health-tick`
- `ci-hub/bin/wrkslots`
- `scripts/allocate-worktree-submodule-test.sh`
- `scripts/allocate-worktree.rs`
- `scripts/check-agent-slot.sh`
- `scripts/check-agent-utils-pin.rs`
- `scripts/check-codex-setup.py`
- `scripts/check-harness-help.py`
- `scripts/check-parent-gitmodules.sh`
- `scripts/check_hermit_agent_utils_pin.py`
- `scripts/checkout-hermit-pr-latest-reverie.sh`
- `scripts/ci-hub-clone-change.py`
- `scripts/hermit-box-run`
- `scripts/hermit-health-staleness-test.sh`
- `scripts/hermit-health-staleness.sh`
- `scripts/hermit-health-tick.sh`
- `scripts/lint-memory-skill-sync.rs`
- `scripts/list-python-files.sh`
- `scripts/list-rust-scripts.rs`
- `scripts/list-rust-sources.sh`
- `scripts/list-shell-scripts.sh`
- `scripts/memory-skill-contradiction-scan.rs`
- `scripts/parent-main-write`
- `scripts/primary_checkout.py`
- `scripts/single-submodule-bump.rs`
- `scripts/submodules.sh`
- `scripts/systemd/hermit-owner-checkout-validate.service`
- `scripts/systemd/hermit-wrkslots-audit.service`
- `scripts/test_check_codex_setup.py`
- `scripts/test_ci_hub_clone_change.py`
- `scripts/test_hermit_agent_utils_pin.py`
- `scripts/test_lint_orchestration.py`
- `scripts/test_parent_main_write.py`
- `scripts/test_primary_checkout.py`
- `scripts/test_python_cli_help.py`
- `scripts/test_wrkslots_audit_provenance.py`
- `scripts/test_wrkslots_launcher.py`
- `scripts/tests/test_hermit_box_run.py`

### Hermit nonhistorical tracked consumers of agent-utils

- `.githooks/pre-push`
- `.github/workflows/ci-dag.yml`
- `.github/workflows/ci-portable.yml`
- `.github/workflows/ci-privileged.yml`
- `.github/workflows/docs.yml`
- `.gitmodules`
- `Cargo.toml`
- `ci/audit-test-binary-registration.py`
- `ci/compat-envelope/pressure-test.rs`
- `ci/compat-envelope/scorecard.rs`
- `ci/configure-build-jobs.sh`
- `ci/dag/README.md`
- `ci/dag/validate.json`
- `ci/lint-checks-node.sh`
- `ci/manifest-plan/Cargo.toml`
- `ci/manifest-plan/src/bin/test-harness.rs`
- `ci/manifest-plan/src/validation_dag.rs`
- `ci/manifest-plan/src/validation_dag_static.rs`
- `ci/nextest-test-results.rs`
- `ci/run-dag.sh`
- `ci/structured-test-results.rs`
- `ci/validate-timeout-layers-test.sh`
- `ci/verify-submodules.sh`
- `docs/DIRECTORY_ACTIONS.md`
- `docs/MERGE_QUEUE.md`
- `docs/PR_SWEEP_VERDICTS.md`
- `scripts/check-dagrun-naming.sh`
- `scripts/check-detcore-backend-abstraction.sh`
- `scripts/check-merge-gate-policy.sh`
- `scripts/lib/validate_plan.rs`
- `scripts/test-authority-obtained-once.sh`
- `scripts/test-pre-push-submodule-diagnosis.sh`
- `scripts/validate.rs`

## Final callers of the changed helpers and validation consumer

Final reviewed agent-utils head: `14589e875c08e458d2f8af5a6f78b12128d5a367`.
The older detailed helper inventory above describes the starting implementation.
The final helper callers below were read from the final source AST.

- `_with_proxy`: `_GitVcs._run` at line 6678, `_run_post_provision_hooks` at line 9845.
- `_assert_create_processes`: `_cmd_create` at line 9901, `_cmd_create` at line 10025.
- `_assert_create_owner_authorized`: `_assert_create_processes` at line 7744, `_cmd_create` at line 9898.

`py/tests/test_packaging_infrastructure.py::test_wrkslots_lifecycle_partitions_are_disjoint_and_complete`
is an actual changed test consumer in the final diff: its exact collection expectations
are 653 total, 98 ordinary, 555 mapped, and its ordinary-name set includes
`test_create_binds_owner_running_beside_its_assigned_coordinator`. Existing disjointness,
union completeness, and expected names remain. Independent read-only verification of
that exact guard passed (1 passed, 15 deselected in 1.17s).

## Consumers confirmed during review

`agent-utils/py/tests/test_packaging_infrastructure.py::test_wrkslots_lifecycle_partitions_are_disjoint_and_complete`
checks the exact lifecycle test count and ordinary/mapped process-namespace
partition. The added cases require 653 total, 98 ordinary, and 555 mapped tests
(previously 644, 97, and 547), plus the new test name for owner and coordinator in separate processes in the
ordinary set. The disjointness, completeness, and existing membership assertions
remain. `Makefile::test` and `scripts/validate.py` run this test through pytest
discovery. `scripts/check_client_names.py` also reads the lifecycle test source
for repository hygiene. The network tests import existing lifecycle fixtures.

The parent `WORKTREES.md` document is linked by `README.md` and historical
`ai_docs/handoffs/hermit-141-20260827.md` and
`ai_docs/transient/fresh-machine-bringup-audit_20260729.md`. The latter two are
historical records, not active allocation entry points.

## Additional direct consumers of the changed assignment check

`_require_coordinator_authorized` is called by `_cmd_create`, `_cmd_register`,
`_cmd_import_existing`, `_cmd_recover_ownerless_agent_worktree`,
`_cmd_recover_ownerless_agent_cache`, `_cmd_recover_absent_agent_row`,
`_cmd_recover_absent_validate_rows`, `_cmd_recover_ownerless_validate_batch`,
and `_cmd_recover`. Only the worktree-creation refusal message changes; all
other actions retain the same requirement and message.

The new `_assert_create_processes` is called twice by `_cmd_create`, under the
mutation lock and before publication. `_assert_create_owner_authorized` is
called by `_cmd_create` and `_assert_create_processes`.

## Project activation status

The Hermit dependency update is prepared and reviewed at
`37a12da4e66da01ee47893c5550b0deaf14b0edd` in
https://github.com/rrnewton/hermit/pull/3012. The branch was pushed with
verification; a fresh fetch and content diff of the gitlink were identical.
GitHub readback confirms that the draft PR contains only the `agent-utils`
gitlink, advancing `d14adc47` to landed `14589e87`. Its pre-commit checks passed.

The project's recorded Hermit and agent-utils pins have not advanced on main.
The current primary tool checkout is activated locally, while a fresh checkout
of the current project pins still selects the old implementation. Hermit
`AGENTS.md` requires a qualifying exact-head local full-validation receipt for
landing. That receipt is not available for this update. Its unchanged
`python3 ci/audit-test-binary-registration.py --json` exits 2 because
`super.pmu_analyze_hello_race_stress_calibrated_skid` executes `analyze` without
declaring `integration_test_binaries`. Both that audit and its graph are
byte-identical to current main. The existing prerequisite repair is
https://github.com/rrnewton/hermit/pull/3010, still open without a qualifying
receipt at the 20:07 UTC check. The wrkslots repair does not lower this gate
or claim a passing Hermit full-validation result.

Independent review approved the dependency update's correctness and the parent
policy/report bytes; that approval does not claim the missing landing receipt.
The review is `/tmp/wrkslots-activation-review-20260914.md`. Durable activation
still requires landing the Hermit dependency update and then advancing the
parent Hermit gitlink. Every tracked reference to the Hermit agent-utils pin
is listed in the dependency-consumer appendix above; the prepared change adds
no other file or API.

## Demonstration process and evidence consumers

The delegated implementation agent launched `owner.py`, a persistent Python
worker in a separate tmux pane, through `start-owner.sh`. It was not a
separately launched ORC or Codex harness. This proves real creation by the
agent's worker outside the coordinator's process ancestry, with live process
identities and complete Git and registry state; it does not assert that an
additional model harness was started.

Those scripts live only under `/tmp/wrkslots-demo-20260914/`. The start script
is consumed by the dedicated tmux session; it invokes `owner.py`. That worker
reads `authorization.json` and the extracted baseline package, invokes the old
module and the fixed project launcher, and writes the evidence files listed
above. The coordinator and independent reviewer read those files. They are
verification artifacts, with no production installation consumer. The worker
remains alive while the registered slot holds the dependency update.
