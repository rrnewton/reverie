# wrkslots creation repair and consumers, 2026-09-14

Inventory of the slot-creation repair, collected from tracked sources in
agent-utils, dev-hermit, Hermit, and Reverie. Paths are relative to the named
repository unless absolute; baseline line numbers precede the repair. Runtime
entry points, active callers, and historical references are distinguished below.

## Final publication status

The slot-creation repair is published in all three repositories: agent-utils
`14589e875c08e458d2f8af5a6f78b12128d5a367`, Hermit
`ef48ca7e9dc91d53bed716f3ed519c6fcbd6dbdf` through
https://github.com/rrnewton/hermit/pull/3012, and parent dev-hermit
`5e05bdb419e989181c717557043ab62c013ab369`. The two dependency changes are Hermit's
nested `agent-utils` gitlink and the parent's recorded `hermit` gitlink. Hermit's
Cargo manifest Reverie revision is unchanged.

The Hermit landing used owner-authorized speculative soft-green with no exact-head
qualifying receipt. The post-land verification timer is enabled and active for
that exact landed Hermit SHA; its first check recorded that current main has no
qualifying result and retained the request. The [final landing record](#owner-authorized-speculative-landing)
below contains the full basis, remote content readback, service evidence, and all
additional consumers. Earlier prepared-candidate status sections are retained as
history and are superseded by that final record.

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

## Additional verification at 20:28 UTC

The parent gitlink change is now prepared locally at
`b6d5a93e801534a85b6308b30a0a5c5a268026e6`, based on published
`dfe99fe1091847a008c9000ea223c6f234ee5234`. Only `hermit` changes, from
`7b8be60708c29b4926f36f06be8793523841d6ba` to the reviewed Hermit candidate
`37a12da4e66da01ee47893c5550b0deaf14b0edd`. It has not been pushed and is not
permission to land the dependency PR. The target must be updated if the Hermit
landing receives a different commit identity.

The prepared parent checkout has its recorded Hermit and nested agent-utils
checkouts initialized. Its actual `ci-hub/bin/wrkslots create --help` reports
the assigned-owner creation policy. These checks passed:

- `scripts/check-agent-utils-pin.rs --root /tmp/wrkslots-dev-hermit-20260914 --no-fetch`:
  exit 0, `state=ok`, isolated-clone mode, recorded Hermit `37a12da4`, nested
  provider `14589e87`, clean provider checkout at the same revision, and zero
  commits ahead or behind its fetched `origin/main`.
- `with-proxy python3 scripts/check_hermit_agent_utils_pin.py --root /tmp/wrkslots-dev-hermit-20260914 --json`:
  exit 0, `state=current`, lag 0, the same recorded chain and freshly queried
  agent-utils main.

The parent coherence check does not pass. Its offline check reports Hermit
manifest pin `8c8c0a57649c9ffbf8a7a14291a64320f64b935f` versus the existing parent
Reverie gitlink `f055d883ba9a`. A separate temporary index populated from the
published parent base produced byte-identical output and exit 1. This is an
existing mismatch; the candidate changes no manifest or Reverie gitlink.
The networked product-currency checks and full Hermit receipt remain unproved.
The logs are `/tmp/wrkslots-parent-pin-check-20260914.log`,
`/tmp/wrkslots-parent-pin-provider-check-20260914.json`,
`/tmp/wrkslots-parent-pin-launcher-help-20260914.txt`,
`/tmp/wrkslots-parent-pin-coherence-20260914.log`, and
`/tmp/wrkslots-parent-baseline-coherence-20260914.log`.

A separately launched Codex harness was also attempted, to strengthen the
literal agent demonstration beyond the already successful persistent Python
worker. The installed CLI requested `gpt-6-astra` through its configured model
gateway and received HTTP 403: `Access to this Codex model is currently restricted.`
The CLI exited 1 before any model response or tool action. The coordinator
independently confirmed no second slot directory, branch, or ACTIVE row exists.
No different model, identity, or endpoint was used to work around that refusal.
The successful registered Python-worker demonstration remains the completed
end-to-end result; an additional independent model-harness result is not claimed.

That attempt's files are under `/tmp/wrkslots-harness-demo-20260914/`.
The dedicated tmux session runs `run-recorded.sh`, which invokes `run.sh`.
`run.sh` executes the installed Codex CLI with `prompt.txt`; that invocation
produces `transcript.jsonl` and `launcher.stderr.log`. `run-recorded.sh` records
the CLI exit in `launcher-exit-status.txt`, then waits for `RELEASE_LAUNCHER`
before exiting. The implementation agent also records launcher/process metadata
in `tmux-pane.txt` and `harness-processes.json`, writes `result.json`, and creates
`RELEASE_LAUNCHER` after the failed CLI exits. The coordinator and reviewer read
the evidence and check the actual absence of slot state. These are temporary
demonstration consumers, not production callers. The failed launcher's wrapper
has exited; the existing demonstration owner and first registered slot were retained.

## Parent Hermit gitlink consumers, 2026-09-14

This is a read-only audit of the remaining parent `hermit` gitlink change. It adds to
`ai_docs/wrkslots-consumers-20260914.md`; it does not replace that report's wrkslots,
agent-utils, or inner Hermit inventory. No repository, index, ref, TaskGraph row, or
slot registry was changed by this audit. Only this report was written.

The inspected parent checkout is `/tmp/wrkslots-dev-hermit-20260914`. Its source base
was `dfe99fe1091847a008c9000ea223c6f234ee5234`. During the audit the coordinator
prepared local parent commit `b6d5a93e801534a85b6308b30a0a5c5a268026e6` there. A
read-only comparison shows that its only changed path relative to the source base
is `hermit`. The source inventory below therefore applies to both parent commits.
This records a local candidate, not evidence that either remote main has advanced.
Approval to bypass the full Hermit receipt was pending in the audit dispatch; no
continuation, local commit, or local check is treated as that approval.

The observed parent gitlink changes from Hermit
`7b8be60708c29b4926f36f06be8793523841d6ba` to
`37a12da4e66da01ee47893c5550b0deaf14b0edd`. Reading those exact Hermit trees shows
one changed entry: `agent-utils`, mode `160000`, from
`d14adc47ebc6dd7d1d758d5c3198c465d61ca703` to
`14589e875c08e458d2f8af5a6f78b12128d5a367`. Every other Hermit tree entry is
unchanged. The cached Hermit `origin/main` was still
`7b8be60708c29b4926f36f06be8793523841d6ba` when inspected. No remote query was made.

### Direct readers of the parent gitlink

Paths are relative to dev-hermit. These programs read the recorded gitlink, its
mode, or its exact commit identity. They are different from programs that merely
open a file under `hermit/`.

| Source | Reader and effect |
| --- | --- |
| `scripts/primary_checkout.py` | `indexed_submodule_commit` (471) reads the parent index, including an explicitly supplied `GIT_INDEX_FILE`, then falls back to `HEAD`. `check_pins` (1763) uses the recorded Hermit commit to read its Cargo manifests and both lockfiles. `check_freshness` and `publish_parent_snapshot` also compare the product gitlinks, including published `origin/main:<product>` entries. The `fresh --publish-parent` path may publish changes; `check-pins` is a separate offline reader. |
| `scripts/check_reverie_pin_invariant.py` | `recorded_gitlink` (120), called by `check_legs` (235), reads the parent Hermit and Reverie index entries, then `HEAD`. It checks Hermit's internal Reverie manifest pins, agreement with the parent Reverie gitlink, and, unless `--offline`, currency against each product's remote main. Its `published_gitlink` helper is also imported by the Reverie-only monitor; that use does not read the Hermit gitlink. |
| `scripts/check_hermit_agent_utils_pin.py` | `measure` (165) imports `recorded_gitlink`, follows parent Hermit commit to that commit's `agent-utils` gitlink, and measures its relationship to agent-utils main. `--no-fetch` still performs `ls-remote`; `--offline` skips the entire check. |
| `scripts/check-agent-utils-pin.rs` | `expected_agent_utils_pin` (487) reads `ls-tree HEAD -- hermit`, validates its mode and object, then reads that exact Hermit commit's `agent-utils` gitlink. `check` compares the nested pin, materialized agent-utils `HEAD`, and agent-utils `origin/main`, plus checkout cleanliness and the parent symlink. It reads `HEAD`, not a staged candidate. |
| `scripts/check-parent-gitmodules.sh` | Reads index modes for all three parent product paths, including `hermit`; requires mode `160000`, the prescribed public URL and `update = checkout`. Also validates the mode and exact target of `agent-utils -> hermit/agent-utils`. This is a structure check, not proof of remote publication. |
| `scripts/check-codex-setup.py` | `parent_index_entry`, `indexed_submodule_path_exists`, and the indexed symlink resolver read the parent gitlink and files in the recorded submodule commit. `canonical_submodule_paths` (399) requires configured product roots to be indexed gitlinks. Package and instruction-policy checks also use materialized product files. |
| `scripts/check-gitlink-currency.sh` | Enumerates `HEAD` gitlinks with `ls-tree`, reads `HEAD:<product>`, and checks ancestry and lag against each product's local `origin/main`. It does not fetch. A local candidate not contained in that ref is reported off-main; this cannot be turned into a publication check by changing a local ref. |
| `ci-hub/bin/agent-tool` | `gitlink_at` (74), followed by selection at 222 and 234, reads the selected parent commit's Hermit gitlink and then its nested agent-utils gitlink. Default selection resolves fresh parent main and uses a private fetched ref. Explicit `DEV_HERMIT_TOOL_ROOT` selects that root's `HEAD`. Supported tools are `dagrun`, `pr-landing-planner`, `safe-ci-dag-runner`, and `tick-hub`. |
| `scripts/hermit-health-tick.sh` | The installed `operational-tool` implementation verifies tracked gitlinks and materialized contents, reads exact parent-to-Hermit-to-agent-utils commits at 2213–2234 and 2318–2332, initializes those exact submodules, and records their identities before running a tool. This is both a direct reader and an initializer. |
| `ci-hub/health/wrkslots_audit_result.py` | `_gitlink` and `provenance_objection` (95) validate the producer receipt's parent history and the exact Hermit and agent-utils gitlinks at that producer commit (149 and 155). It does not require an older valid producer receipt to name the reader's new pins. |
| `ci-hub/compatibility-website/export_inputs.py` | `_hermit_reader` (493) reads the reader's `HEAD` Hermit gitlink, requires a real materialized Hermit checkout at exactly that commit, and reports both identities. Changing the pin without materializing it causes refusal. |
| `scripts/health-poll.py` | `_gitlink` (391), used by `check_repo_hygiene` (476), compares all parent product `HEAD` gitlinks with their checkout `HEAD` and cached `origin/main`. This is a read-only prototype, not the deployed health tick. |
| `scripts/parent-main-write` | `gitlink_overlap_matches_target` (136) reads old tree, target tree, and index entries and requires exact mode/commit agreement when preserving an already advanced checkout. Sync and residue handling at 894 and 1032 also treat mode `160000` separately from blobs. These generic parent Git operations consume `hermit` when that path is involved. They can mutate state and are not local audit commands. |
| `ci-hub/health/unpushed_parent_commits.py` | `classify_publication` (246) enumerates changed parent paths and compares `rev-parse <commit>:<path>` identities. A changed `hermit` gitlink is an ordinary changed path in that comparison. The separate `raw_blob` helper is for ledger content, not the Hermit pin. |
| `scripts/single-submodule-bump.rs` | `preflight` reads `HEAD:<selected product>` and fetched product main; `hermit` is one of the permitted parent products. Its run path commits and records the exact A-to-B transition and verification result. Even `plan` fetches, so it is not read-only. |

### Initialization, configuration, and entrypoints

These existing paths pass the parent gitlink to Git initialization or invoke the
readers above. Their own source bytes are unchanged by the candidate.

| Source | Role |
| --- | --- |
| `.gitmodules` | Defines the `hermit`, `reverie`, and `liteinst2` paths, public HTTPS URLs, and `update = checkout`. |
| `agent-utils` | Tracked symlink to `hermit/agent-utils`; it is not an independent parent gitlink. Existing provider consumers in the earlier report resolve through it. |
| `.wrkslots.yml` | Its post-provision hook runs `git submodule update --init --recursive`. For a slot whose selected repository is dev-hermit, that initializes the parent Hermit pin. |
| `Makefile` | `checkout-all` performs recursive initialization; `check-submodules` depends on it; `init-hermit` depends on `check-submodules`. `submodules` invokes `scripts/submodules.sh`. `checkout-fresh` invokes `primary_checkout.py fresh --publish-parent`. Pin-check targets and `lint` invoke the structural and pin readers; `single-submodule-bump` invokes that workflow. Names beginning with `check` do not make these all read-only. |
| `scripts/submodules.sh` | `process_product` (155) initializes a missing parent product at its gitlink, then manages its attached branch. `--with-agent-utils` initializes the nested pin too. Default branch refresh can subsequently move the product checkout; this is not an exact-candidate audit command. |
| `scripts/ci-hub-clone-change.py` | Clone provisioning initializes the parent products (1003), then Hermit's agent-utils (1014); initialization checks reject missing submodules. |
| `scripts/super-validate.sh` | Initializes the parent Hermit pin if the product validator is missing (30), then runs the selected product/demo validation. |
| `.github/workflows/dev-hermit-ci.yml` | Product initialization at 69, 145, and 198; nested agent-utils initialization at 70 and 199; agent-utils pin check at 215. |
| `.github/workflows/compat-envelope.yml` | Both checkout steps (72 and 152) request recursive submodules. |
| `.github/workflows/nightly-demo-sweep.yml` | Checkout at 72 requests recursive submodules; later invokes `scripts/super-validate.sh`. Current trigger is manual `workflow_dispatch`; older comments do not establish automatic execution. |
| `.github/workflows/portability.yml` | Checkout at 36 requests recursive submodules. |
| `README.md` | Documents recursive clone/update and explicit parent Hermit then nested agent-utils initialization. |
| `.githooks/pre-commit` | Invokes `primary_checkout.py check` as a warning, then the blocking offline `check-pins` when Hermit is materialized. The pin check reads the index that the pending commit actually records. |
| `scripts/setup-hooks.sh` | Installs hook dispatch for parent and initialized products; this carries the pre-commit consumer. It reads configured product paths, not the Hermit commit identity itself. |
| `.githooks/hygiene-policy.md` | Documents the checkout and pin checks and parent/inner repository relationship. |
| `ci-hub/history/SUBMODULE-BUMPS.md` | Documents verified parent product pin transitions and explicitly requires nested agent-utils changes to land in Hermit before advancing the parent Hermit gitlink. |
| `ci-hub/bin/health-tick` | Calls `ci-hub/bin/agent-tool tick-hub` and consumes its selected provider. |
| `ci-hub/health/pr_status.py` | Calls `ci-hub/bin/agent-tool` for `pr-landing-planner`. |
| `ci-hub/health/tick-hub.yaml` | Invokes the wrkslots audit receipt reader and `unpushed_parent_commits`; it is also the health configuration selected by the verified tool checkout. |
| `scripts/systemd/hermit-health-tick.service` | Invokes installed `operational-tool` to select `scripts/hermit-health-tick.sh`. |
| `scripts/systemd/hermit-owner-checkout-validate.service` | Invokes installed `operational-tool` to select `scripts/owner-checkout-validate-watch.py`. The selected code uses the parent pin; the owner's actual Hermit validation target is explicitly the canonical checkout. |
| `scripts/systemd/hermit-wrkslots-audit.service` | Invokes installed `operational-tool` to select `ci-hub/bin/wrkslots-audit`. |
| `ci-hub/bin/wrkslots-audit` | Receives and records the exact parent/Hermit/agent-utils provenance supplied by `operational-tool`. |
| `ci-hub/ledger/launch_publish_backlog.sh` | Uses installed `operational-tool` when launching the publish sweep from a verified tool checkout. |
| `scripts/install-owner-checkout-validate-watch.sh` | Installs the owner validator unit and shared operational-tool implementation. |
| `scripts/install-wrkslots-audit.sh` | Installs the audit unit and shared operational-tool implementation. |
| `docs/health-poller.md` | Documents the separate read-only health-poll prototype and its invocation. |

The earlier report already enumerates `ci-hub/bin/wrkslots`, `worktrees/wrkslots`,
the runtime package, its callers, and other provider consumers. The launcher reads
the materialized `hermit/agent-utils/py` path; it does not itself resolve the parent
Hermit gitlink. It becomes a consumer of the newly selected implementation once
that exact nested checkout is materialized.

### Tests and validation wiring

These are unchanged tests or validation entrypoints for the readers and
initializers above. This list records consumers, not a claim that they were run
in this audit or that every listed file directly exercises every gitlink case.

- `scripts/test_primary_checkout.py`
- `scripts/test_reverie_pin_invariant.py`
- `scripts/test_hermit_agent_utils_pin.py`
- `scripts/check-agent-utils-pin.rs` (its in-file Rust tests)
- `scripts/single-submodule-bump.rs` (its in-file Rust tests)
- `scripts/test_check_codex_setup.py`
- `scripts/test_ci_hub_clone_change.py`
- `scripts/test_parent_main_write.py`
- `scripts/test_health_poll.py` (imports the prototype; not evidence of pin-specific coverage)
- `ci-hub/tests/test_agent_tool_pin_equivalence.py`
- `ci-hub/tests/test_compatibility_website_export_inputs.py`
- `ci-hub/health/tests/test_unpushed_parent_commits_budget.py`
- `ci-hub/health/tests/test_wrkslots_audit_result.py`
- `ci-hub/tests/test_health_tick_evaluation_provenance.py`
- `ci-hub/tests/test_health_tick_escalation.py`
- `ci-hub/tests/test_tick_frequency.py`
- `ci-hub/tests/test_wrkslots_audit_job.py`
- `scripts/test_wrkslots_audit_provenance.py`
- `scripts/tests/test_install_owner_checkout_validate_watch.py`
- `scripts/tests/test_install_wrkslots_audit.py`
- `scripts/tests/test_owner_checkout_validate_watch.py`
- `scripts/check-harness-help.py` (registers the gitmodules helper help invocation)
- `scripts/test_python_cli_help.py`
- `scripts/run-directory-tests.sh`
- `ci-hub/tests/run_python_suites.py` (imports and collection depend on materialized nested packages)
- `Makefile`
- `.github/workflows/dev-hermit-ci.yml`

### Other matches and unchanged Hermit program users

A changed gitlink participates in ordinary Git commit, diff, and push operations.
`ci-hub/bin/git-push-verified` includes the pin-changing parent commit in its exact
remote-ref verification. Its tests are `ci-hub/tests/test_git_push_verified.py` and
`scripts/test_git_push_verified.py`. `ci-hub/bin/gh-merge-verified` also supports
mode `160000` in general content comparison, with
`ci-hub/bin/gh-merge-verified-test`; it is not the parent main-only landing path.
These do not interpret or initialize the selected Hermit product.

The following matches do not read this parent pin:

- `ci-hub/health/reverie_pin_lag.py` reads the published parent **Reverie** gitlink through a shared helper.
- `scripts/allocate-worktree.rs` initializes `agent-utils` inside a selected Hermit product worktree. Its product starts are independently selected; it does not initialize parent `hermit` from the parent gitlink. `scripts/allocate-worktree-submodule-test.sh` tests that inner provisioning.
- `scripts/release-worktree.rs` enumerates gitlinks and can reinitialize submodules inside its selected product cleanup target. Its target products are Hermit, Reverie, and LiteInst2, not the parent checkout. It does not select the parent's Hermit commit.
- `scripts/checkout-optional-submodules.rs` handles Reverie and optional inner submodules, not parent Hermit initialization.
- `ci-hub/validate/start_unit.py` and `ci-hub/series/pressure_schedule.py` initialize submodules inside explicitly selected product validation checkouts. Their validation target is not obtained from the parent Hermit gitlink.
- `ci-hub/qualifying_receipt.py` checks a requested product commit's receipt and source definitions. An initialized Hermit source repository is needed, but changing the parent pin does not create a qualifying receipt.
- `ci-hub/ledger/validate_rows.py` has a tracked-source reader that requires mode `100644 blob`; it does not accept `hermit` mode `160000` as that source file.
- `ci-hub/validate/gitmodules_lint.py` reads parent and materialized product `.gitmodules` files. Their bytes do not change here. Its tests and mutation suite continue to check those manifests, not the new parent gitlink value.
- `scripts/list-python-files.sh`, `scripts/list-rust-sources.sh`, and `scripts/list-shell-scripts.sh` explicitly skip directory/gitlink entries rather than read their commits.
- `ci-hub/stress/nightly-stress.workflow.yml` is a proposed workflow for the **Hermit** repository; its recursive checkout is not a live parent workflow.
- `demos/common.sh`, `demos/05-qemu-boot.py`, `demos/06-qemu-resume.py`, `bin/hermit-repeat`, `bin/safehermit`, `scripts/doctor.sh`, and `scripts/install-deps.sh` use Hermit programs, files, or setup paths. None obtains the selected product revision by reading this parent gitlink.

For the exact two Hermit commits inspected above, all Hermit source, manifests,
lockfiles, validation definitions, and binaries tracked outside `agent-utils` are
unchanged by Git tree identity. Generic users of those bytes are therefore not
additional changed source APIs. This does not establish runtime behavior of the
new agent-utils dependency; that evidence remains in the original report and the
separate validation record.

### Coordinator-provided local check evidence

The coordinator supplied these logs after preparing local parent candidate
`b6d5a93e801534a85b6308b30a0a5c5a268026e6`; this audit read them without rerunning
or altering the checks:

- `/tmp/wrkslots-parent-pin-check-20260914.log`: the agent-utils drift check reports `state=ok`, candidate Hermit `37a12da4e66da01ee47893c5550b0deaf14b0edd`, and nested pin, materialized checkout, and cached agent-utils main all `14589e875c08e458d2f8af5a6f78b12128d5a367`; clean detached isolated checkout. The coordinator reports exit 0.
- `/tmp/wrkslots-parent-pin-provider-check-20260914.json`: explicitly records `exit: 0`, `state: current`, the same Hermit and agent-utils commits, and lag 0.
- `/tmp/wrkslots-parent-pin-coherence-20260914.log`: A internal passes with one Reverie revision `8c8c0a57649c` across 20 entries; B coherence fails because the parent Reverie gitlink is `f055d883ba9a`; C and D are skipped by `--offline`. The coordinator reports exit 1.
- `/tmp/wrkslots-parent-baseline-coherence-20260914.log`: the coordinator's separate-index control at published parent base `dfe99fe1091847a008c9000ea223c6f234ee5234` has the same failure. This audit compared both log files byte-for-byte and confirmed equality. Both contain 428 bytes, SHA-256 `e7153e081d1ceec403c94ff5343db8ed805993ad457a4cbf017715cde6a7b0e3`.

The coherence failure is pre-existing and remains a failure. No comparator,
threshold, assertion, exemption, or pin was changed by this audit to make it pass.
The provider checks establish their stated dependency relationship; they do not
establish full Hermit validation or authorization to omit it. These logs contain
no overall elapsed duration.

### Read-only checks for a prepared local candidate

These checks are appropriate once the coordinator has prepared the candidate
checkout/index and materialized the needed object stores. They do not prepare,
commit, or publish a candidate. The commands below describe available checks; the specific coordinator results
above remain the actual observed evidence. No check was run by this audit. Run each with its actual exit status preserved and inspect the
reported commit identities.

1. Read the exact parent index and commit: `git ls-files --stage -- hermit`, `git ls-tree HEAD -- hermit`, and `git diff --cached --raw`. Check the mode, stage, and expected commit, not merely command success. A committed candidate must be compared to its recorded base with `git diff --raw <base> <candidate>`.
2. Read the selected product object directly: `git -C hermit cat-file -e '<Hermit candidate>^{commit}'`, `git -C hermit ls-tree <Hermit candidate> -- agent-utils`, and `git -C hermit diff --raw <old Hermit> <Hermit candidate>`. Require the single intended nested gitlink transition and an available agent-utils commit object. Use `GIT_NO_LAZY_FETCH=1` to prevent a missing promisor object from starting a fetch.
3. Run `GIT_OPTIONAL_LOCKS=0 GIT_NO_LAZY_FETCH=1 bash scripts/check-parent-gitmodules.sh` in the candidate root. This checks index structure, manifest policy, and the exact parent symlink.
4. Run `GIT_OPTIONAL_LOCKS=0 GIT_NO_LAZY_FETCH=1 PYTHONDONTWRITEBYTECODE=1 python3 -B scripts/primary_checkout.py --root . check-pins`. It reads the indexed candidate and both lockfiles. Its pre-existing unevaluable case returns zero with a warning, so require the positive line identifying the intended candidate and inspect any warnings. A zero exit alone is insufficient.
5. Run `GIT_OPTIONAL_LOCKS=0 GIT_NO_LAZY_FETCH=1 PYTHONDONTWRITEBYTECODE=1 python3 -B scripts/check_reverie_pin_invariant.py --root . --offline`. Require A internal and B coherence to be checked successfully. C and D are explicitly skipped in this mode and must be reported as unmeasured.
6. `scripts/check-codex-setup.py --root .` is a read-only setup check; use `python3 -B`, `PYTHONDONTWRITEBYTECODE=1`, `GIT_OPTIONAL_LOCKS=0`, and `GIT_NO_LAZY_FETCH=1`. It needs the materialized canonical packages and policies; this can check the new pin's setup paths without a remote operation.
7. An already built `check-agent-utils-pin` executable can run with `--root <candidate root> --no-fetch`. That checks the committed parent `HEAD` chain against materialized agent-utils and its **cached** main ref. The Rust-script launcher may build/cache an executable, so it is not a strictly read-only invocation if not already built. The simpler raw Git reads in item 2 avoid that issue. Neither form proves fresh remote publication of Hermit.

`check-gitlink-currency.sh` is read-only but compares committed `HEAD` to cached
product main. Before the candidate reaches Hermit main, an off-main finding is
expected evidence of the still-unpublished commit, not permission to move a ref or
weaken the check. The full `check_reverie_pin_invariant.py` command queries remote
main; `check_hermit_agent_utils_pin.py --no-fetch` also still queries remote main.
Its `--offline` mode skips everything and supplies no candidate evidence.

Do not substitute `make check-submodules`, `make checkout-all`, `make lint`,
`single-submodule-bump plan`, `agent-tool` default selection, operational-tool, or
submodule initialization for these read-only checks: they fetch, initialize,
write caches, or mutate state. The exact Hermit main publication and full Hermit
receipt remain separate facts to establish through their authorized paths.

### Search evidence and additions to the earlier report

The audit searched tracked parent sources with `git grep` for `gitlink`,
`submodule update`, recursive checkout configuration, `ls-tree`, indexed gitlink
helpers, mode `160000`, and their caller names. It read the matched production
implementations and distinguished tests, installation, policy, archived captures,
inner product operations, and unrelated blob readers. It did not recursively walk
other worktrees or treat historical ledger text as executable consumers.

The following paths in this report were absent by exact path text from the
existing tracked consumer report when compared. They include direct readers,
initializers, and their test/configuration/documentation consumers; the roles above
are part of the enumeration and should be retained when incorporating the list.
The excluded matches in the preceding section are not included here.

- `.githooks/hygiene-policy.md`
- `.githooks/pre-commit`
- `.github/workflows/compat-envelope.yml`
- `.github/workflows/nightly-demo-sweep.yml`
- `.github/workflows/portability.yml`
- `ci-hub/compatibility-website/export_inputs.py`
- `ci-hub/health/pr_status.py`
- `ci-hub/health/tests/test_unpushed_parent_commits_budget.py`
- `ci-hub/health/unpushed_parent_commits.py`
- `ci-hub/history/SUBMODULE-BUMPS.md`
- `ci-hub/ledger/launch_publish_backlog.sh`
- `ci-hub/tests/run_python_suites.py`
- `ci-hub/tests/test_agent_tool_pin_equivalence.py`
- `ci-hub/tests/test_compatibility_website_export_inputs.py`
- `ci-hub/tests/test_health_tick_escalation.py`
- `docs/health-poller.md`
- `scripts/check-gitlink-currency.sh`
- `scripts/check_reverie_pin_invariant.py`
- `scripts/health-poll.py`
- `scripts/install-owner-checkout-validate-watch.sh`
- `scripts/owner-checkout-validate-watch.py`
- `scripts/run-directory-tests.sh`
- `scripts/setup-hooks.sh`
- `scripts/super-validate.sh`
- `scripts/systemd/hermit-health-tick.service`
- `scripts/test_health_poll.py`
- `scripts/test_reverie_pin_invariant.py`
- `scripts/tests/test_install_owner_checkout_validate_watch.py`
- `scripts/tests/test_owner_checkout_validate_watch.py`

## Owner-authorized speculative landing

The owner explicitly authorized speculative soft-green landing of
https://github.com/rrnewton/hermit/pull/3012 without an exact-head Hermit validation
receipt. This authorization covers activating the reviewed slot-creation repair
through the Hermit and parent gitlinks. It does not convert missing validation into
a passing run or create a receipt for the candidate.

The implementation was adversarially reviewed at agent-utils
`14589e875c08e458d2f8af5a6f78b12128d5a367`. The review retained the ownership and
process-generation checks, required refusal for unrelated owners, and found the
missed lifecycle partition consumer before approval. The corrected partition
check retains disjointness, completeness, and all prior cases. Full agent-utils
`make validate` then passed at that exact commit: 3,367 Python tests, 871 Rust tests,
and 1,491 Python/Rust comparison checks, with the phase durations and coverage
limits recorded above. No new assertion, tolerance, skip, or comparator was changed
to obtain landing permission.

The coordinator-assigned persistent Python owner in a separate tmux pane
demonstrated the actual defect and repair: the original implementation refused the
correct live coordinator PID
because it was not an ancestor; the fixed ordinary launcher created and registered
the assigned Hermit slot, fetched from GitHub, and initialized recursive submodules
without an outer proxy wrapper. The ACTIVE row, exact owner/coordinator process
identities, Git worktree registration, and creation event were independently read
back. The detailed evidence and measured durations remain in this report's real
slot-creation section. These results establish the slot-creation behavior; they
are not presented as a full Hermit validation run.

### Exact pin changes

| Repository and entry | Previous recorded object | Intended or verified replacement |
| --- | --- | --- |
| Hermit `agent-utils`, mode `160000` | `d14adc47ebc6dd7d1d758d5c3198c465d61ca703` | `14589e875c08e458d2f8af5a6f78b12128d5a367`, already reviewed, validated, and landed on agent-utils main |
| dev-hermit `hermit`, mode `160000` | `7b8be60708c29b4926f36f06be8793523841d6ba` | `ef48ca7e9dc91d53bed716f3ed519c6fcbd6dbdf`, verified on freshly fetched Hermit main |

https://github.com/rrnewton/hermit/pull/3012 merged at
`2026-09-14T20:57:17Z` as
`ef48ca7e9dc91d53bed716f3ed519c6fcbd6dbdf`, whose parent is
`7b8be60708c29b4926f36f06be8793523841d6ba`. Its complete tree is
`52f2c3fc67a1956301c62b13636c912642b09e38`, byte-identical to the reviewed candidate.
A fresh-fetch full-tree comparison returned exit 0. The existing
`ci-hub/bin/gh-merge-verified` command also returned exit 0; its receipt
`/tmp/wrkslots-hermit-merge-verified-20260914.json` binds the expected head
`37a12da4e66da01ee47893c5550b0deaf14b0edd`, names `agent-utils` as the sole changed
path, and records that path as arrived unchanged. Its log is
`/tmp/wrkslots-hermit-merge-20260914.log`. The canonical review is recorded at
https://github.com/rrnewton/hermit/pull/3012#issuecomment-5670698307.

The reviewed local Hermit candidate is
`37a12da4e66da01ee47893c5550b0deaf14b0edd`. Comparing it to
`7b8be60708c29b4926f36f06be8793523841d6ba` shows exactly the `agent-utils` gitlink
change above. All other Hermit tree entries are byte-identical, including the Cargo
manifests and lockfiles. The Cargo manifest Reverie revision remains
`8c8c0a57649c9ffbf8a7a14291a64320f64b935f`; it is not changed by this repair.

The parent pin landed on dev-hermit main as
`5e05bdb419e989181c717557043ab62c013ab369`, based on
`68a1b9382c906e349a5ca92e30224ca928190337`. It records the verified landed Hermit
object `ef48ca7e9dc91d53bed716f3ed519c6fcbd6dbdf`. The exact final parent change was
approved in `/tmp/wrkslots-parent-final-review-20260914.md`. The verified push
returned exit 0; `/tmp/wrkslots-parent-final-push-20260914.log` confirms the remote
main SHA. A fresh fetch and content check in
`/tmp/wrkslots-parent-final-readback-20260914.json` verifies the complete parent →
Hermit → agent-utils pin chain, the unchanged Cargo manifest Reverie revision, the
Hermit tree's identity with the reviewed candidate, and `hermit` as the only changed
parent path. The push's warning that GitHub had no current-tip runs remains visible;
publishing the pin is not a claim that GitHub CI or full Hermit validation passed.

### Current gate result and evidence limits

The recorded canonical `validate-status --repo rrnewton/hermit --sha
37a12da4e66da01ee47893c5550b0deaf14b0edd --json --log-paths` result is
`NOT-VALIDATED`, exit 4: zero records, zero qualifying receipts, and zero in-progress
runs. It reports current ledger freshness and authoritative local absence. The
saved result is `/tmp/wrkslots-hermit-pin-validation-status-20260914.json`.

The existing full run of Hermit main
`7b8be60708c29b4926f36f06be8793523841d6ba` ended with
`FINAL_VALIDATE_STATUS: FAILED` after 12 minutes 5 seconds. It executed 12 DAG
nodes, failed one, skipped 258 after the dependency failure, and executed zero
tests. `build.rust_scripts` failed after 55.48 seconds with four `oom_kill` events
at its approximately 2 GiB inner cgroup memory cap. The unchanged committed node
configuration requests eight inner jobs through `CARGO_BUILD_JOBS` and a
2,147,483,648-byte hard memory limit; the outer scheduler width was 16. The durable
log is
`/home/newton/work/dev-hermit/ignored/validate/validate-full-7b8be60708c2-validate-mega-lander-concurrency-solo-7b8be607-20260914t171908z.log`.

The same run also could not retain complete coverage evidence because the
registration audit refused: `ci/dag/validate.json` step
`super.pmu_analyze_hello_race_stress_calibrated_skid` executes `['analyze']` but omits
`integration_test_binaries`. A direct audit of the pin candidate retained that
same refusal; `/tmp/wrkslots-hermit-pin-registration-audit-20260914.stderr` records
the exact diagnostic. The DAG and audit source bytes are unchanged by the nested
agent-utils pin update. No qualifying historical receipt is claimed.

The completed parent provider checks pass for the final landed pin chain:
`/tmp/wrkslots-parent-final-pin-check-20260914.log` reports `state=ok`, and
`/tmp/wrkslots-parent-final-provider-check-20260914.json` records exit 0,
`state=current`, and agent-utils lag 0. Both identify landed Hermit
`ef48ca7e9dc91d53bed716f3ed519c6fcbd6dbdf` and agent-utils
`14589e875c08e458d2f8af5a6f78b12128d5a367`; the materialized agent-utils checkout
is clean and detached in the isolated parent checkout.

The separate parent offline coherence check still fails. It observes one Reverie
manifest revision `8c8c0a57649c` across 20 entries, but the recorded parent Reverie
gitlink is `f055d883ba9a`. The candidate and published-base control logs are
byte-identical, so this is a pre-existing failure. It remains a failure; no pin or
comparison was changed to suppress it. The logs are
`/tmp/wrkslots-parent-pin-coherence-20260914.log` and
`/tmp/wrkslots-parent-baseline-coherence-20260914.log`. Their offline mode explicitly
skips product-main currency. Passing provider checks, unchanged Hermit source
bytes, and the real slot demonstration do not establish a full Hermit receipt.

### Instruction mismatch

Hermit's `AGENTS.md:384-388` is the fourth source the owner identified as teaching
the receipt-required rule: “landing authorization is the parent's exact-head local
receipt queried through `ci-hub validate-status`.” It should point to the owner's
speculative soft-green policy. The owner explicitly authorized this landing without
that exact receipt; no policy or skill file is changed by this pin update.

### Verification after landing and its consumers

The task-specific verification is installed and armed under
`ignored/wrkslots-post-land-20260914/` in the canonical dev-hermit checkout. It adds
no tracked production source change. `request.json` was written at
`2026-09-14T20:59:51.323050+00:00` for the exact landed Hermit commit
`ef48ca7e9dc91d53bed716f3ed519c6fcbd6dbdf`, tree
`52f2c3fc67a1956301c62b13636c912642b09e38`, and nested agent-utils
`14589e875c08e458d2f8af5a6f78b12128d5a367`.

The first service invocation ran from 21:00:21 to 21:01:12 UTC and returned exit 0.
That was a completed poll, not a validation pass: its durable state is `WAITING`,
attempt 0, with `qualifying_receipt_claimed=false`. Fresh canonical status for the
landed main SHA was `NOT-VALIDATED`, exit 4, with zero records, qualifying receipts,
and in-progress runs; ledger freshness was current and absence authoritative.
No full verification has launched. At the 21:02:01 UTC readback the timer was both
enabled and active, the service had `Result=success` and `ExecMainStatus=0`, and the
next scheduled poll was 21:15 UTC. User lingering was enabled. Exact command exits,
installed links and hashes, the first state, and its complete main-status result
are retained in `armed-evidence.json`.

The helper retains the actual landed Hermit SHA, tree, agent-utils gitlink, and PR
URL in `request.json`, and preserves a clean detached clone at that exact commit
in `source/`. It verifies the landed commit is contained in freshly fetched Hermit
main and still records agent-utils `14589e875c08e458d2f8af5a6f78b12128d5a367`.
Before starting a new verification, the helper requires canonical `validate-status`
to report `VALIDATED` for the exact
freshly fetched main SHA with a qualifying record whose `commit` and `sha` both
match that SHA; a second fetch must show that main did not move during the query. Red, missing, unreadable, or moved evidence
leaves this task waiting.

If that green main SHA is the actual landed SHA, its canonical qualifying status
already supplies the requested exact-commit verification. Otherwise the helper
asks existing `ci-hub validate-run` for `--frozen-validate --target
ef48ca7e9dc91d53bed716f3ed519c6fcbd6dbdf -- full`. It persists the unit identity before launch and
reattaches to an existing unit, avoiding duplicate validation. A completed frozen
result must match the requested commit and the recorded run's checkout and start
time, and retain `qualifying_receipt=false` and
`measured_against_superseded_tip=true`. A failure stays a failure. The exact older
tree can still fail its unchanged build or registration checks after newer main
is green; this mechanism does not repair or replace that older tree and does not
turn its frozen result into a historical qualifying receipt.

Every new source, caller, configuration path, and output reader is listed here.
Paths beneath `ignored/wrkslots-post-land-20260914/` refer to that task directory.
All three installed unit links below were read back and resolve to the recorded
service/timer bytes.

| Artifact or existing entrypoint | Consumers and role |
| --- | --- |
| `verify.py` | Direct executable consumer: `wrkslots-post-land-20260914.service` `ExecStart`. Direct import consumer: `test_verify.py`. Direct human invocation: coordinator `--arm ef48ca7e9dc91d53bed716f3ed519c6fcbd6dbdf` and `--status`; the coordinator reads its JSON state for this report. No tracked production module imports it. |
| `test_verify.py` | Invoked by the task's offline Python test command. Imports `verify.py` and exercises exact green evidence, red and moved main, retained terminal results, attachment, durable run identity, full frozen invocation, retries, unknown service state without a duplicate launch, failure retention, and rejection of unbound result rows. Results are retained in `tests.log` and read by the coordinator/reviewer and this report. |
| `wrkslots-post-land-20260914.service` | Consumed by user systemd through the timer or an explicit coordinator service start. It invokes `/usr/bin/python3` on the exact task helper, with the canonical parent as working directory, `PYTHONDONTWRITEBYTECODE=1`, a 5,600-second service timeout, and control-group termination. |
| `wrkslots-post-land-20260914.timer` | Consumed by user systemd; names the service explicitly and schedules a poll every 15 minutes, with persistent scheduling and one-minute accuracy. It invokes no validation directly. |
| `~/.config/systemd/user/wrkslots-post-land-20260914.service` and `~/.config/systemd/user/wrkslots-post-land-20260914.timer` | Installed `systemctl --user link` symlinks to the task files; read by the user systemd manager. The enabled timer also has its installed link under `~/.config/systemd/user/timers.target.wants/`. |
| `request.json` | Written by `verify.py --arm`; read by subsequent helper invocations. Binds the exact landed commit, tree, nested agent-utils pin, retained source, owner-authorized basis, and full PR URL. |
| `source/` | Written by arming; read by the helper's clean-source, Git ancestry and main-ref checks and by canonical `validate-run` as the source repository for the exact target. |
| `state.json` | Written atomically by the helper; read by later helper invocations, `--status`, and the coordinator. It binds the request, attempt/unit, current state, and result paths. |
| `events.jsonl` | Appended by the helper; retained for coordinator/reviewer history. It is not read to decide whether a validation passed. |
| `job.lock` | Shared by helper invocations through `flock`; the service, manual arming, and polling cannot mutate this task concurrently. |
| `main-status.json` and `main-status.stderr` | Written from the canonical status query. The helper reads the parsed status to decide whether to wait or launch; the coordinator/reviewer reads the retained result. |
| `validate-*-launcher.log` | Receives canonical launch/attachment output. Read by the coordinator/reviewer when following the run; it is not parsed into a pass. |
| Installed `operational-tool` | Existing launcher selected by the helper for `ci-hub/ci-hub`; it selects verified current parent tooling. Its source and existing consumers were enumerated in the parent pin inventory; no source change is introduced here. |
| `ci-hub/ci-hub` commands `validate-status` and `validate-run` | Existing commands invoked through `operational-tool`. Status supplies the current main gate; `validate-run` starts or attaches through existing `ci-hub/validate/start_unit.py`. |
| `ci-hub/validate/start_unit.py` | Existing canonical producer and lifecycle path for `--frozen-validate`, `--target`, `--unit`, `--attach`, and full validation. It manages the validation checkout/log/cache and runs Hermit's existing `scripts/validate.rs`; those existing consumers are unchanged. |
| `ignored/validate/runs/*.json` and `ignored/validate/frozen/*.jsonl` for the stored validation unit | Written by the existing canonical validation machinery; read by `verify.py` to bind the exact run and retain its measured result. They remain available to existing validation-status/attachment tools and coordinator inspection. |
| `arm.log`, `install.log`, `enable.log`, and `first-start.log` | Outputs of the coordinator's arming, unit linking/enabling, and first service start. Read by the coordinator/reviewer and summarized in `armed-evidence.json`; they do not establish a full validation pass. |
| `hashes.json` and `armed-evidence.json` | Written by the task's evidence capture; read by the coordinator/reviewer and this report. They record final source/test/unit hashes, command exits, installed links, and the first poll's state. They are not inputs to the decision to launch or pass validation. |
| This report and the coordinator's final message | Human readers of the retained request, status, logs, and result. Reporting does not modify the canonical validation result or produce a qualifying receipt. |

The final offline suite passed ten tests in 0.024 seconds; `tests.log` retains
the result. It checks the helper's decisions with controlled inputs. The installed
service and timer were checked separately as recorded above; neither result is a
full Hermit run. The final sources have SHA-256 values:

- `verify.py`: `5002ee63dff4e5e422645b51bc4c1d466e527ee12b0f218b24cfbe7f7c644100`.
- `test_verify.py`: `b831ee1bd5e261775ab926b183db9fdc5165e2c803bd15cd89c92ad65e763520`.
- `wrkslots-post-land-20260914.service`: `907d2ed1fd3f339dacb7c65189f99abfb321f5fe2f19bcbe2bb5226dc8c8dd9d`.
- `wrkslots-post-land-20260914.timer`: `6095d3e185c6b7dbb3a26751fdf3e66c248f84a3608e657f7cdd8fe06a6b3834`.
