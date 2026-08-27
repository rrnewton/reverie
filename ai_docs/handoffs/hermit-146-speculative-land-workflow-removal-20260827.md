# Speculative-land workflow removal evidence

Task: `stop-whatever-recreates-the-speculative-land-obligation-workflow`

## Live incident

- The ORC workflow `hermit-dev-speculative-land-remediation-v1` ran
  `hermitSpeculativeLandObligations` every 15 seconds, not every five minutes.
- Its command ran `ci-hub/remediation/land_and_arm.py recover --observe-timeout 5`
  followed by `ci-hub/bin/speculative-land watch --once --gate`.
- The resulting detached `protocol.py _local-run` and `protocol.py watch`
  processes wrote under `ignored/ci-hub/obligations/`.
- The active local-run processes and ten surviving watcher pairs were terminated.

## Re-registration evidence

- ORC logged deletion of `hermit-dev-speculative-land-remediation-v1` at
  `2026-08-27T14:45:33.110027Z` with `reason="workflow was killed"`.
- At `2026-08-27T14:47:59.243649Z`, `reload_runtime_with_boot` loaded
  `/home/newton/work/dev-hermit/.orc/config.js` and registered the same workflow.
- At `2026-08-27T14:47:59.251109Z`, ORC recorded
  `declaration_action="added"` for that workflow.
- At `2026-08-27T14:47:59.403159Z`, the re-created workflow started
  `hermitSpeculativeLandObligations` again.

The persistent source path is:

1. `.orc/config.js` imports `.orc/plugins/hermit-dev/index.ts`.
2. Module load calls `registerPluginSurface()`.
3. `registerPluginSurface()` registers both the script and
   `hermit-dev-speculative-land-remediation-v1`.
4. The workflow loops and sleeps for 15 seconds after each invocation.

Killing the live workflow therefore stops only that instance. A later config reload
registers it again. The separate config-load failure owner established that externally
killing this workflow does not create or change the conflicting module-origin row, so
that proposed causal link is rejected.

## Coordination

Agent `hermit-145` owns the config-load failure in the same source file. Its work is
limited to the policy reload function and the activation exposure block. This removal
does not need either region; it is limited to the speculative-land command/constants,
heartbeat, script/workflow registrations, and directly associated status/tests.

## Removal

- Removed both registered ORC scripts, their commands, the 15-second workflow,
  and its heartbeat implementation. Startup retains only the workflow name in
  the existing legacy-workflow cleanup list so a persisted pre-removal instance
  is killed when the new plugin loads.
- Removed the `ci-hub` arm, inherit, watch, wake, cancel, authorize-revert, and
  resolve commands; the obligation record schema and store; the
  `land_and_arm.py` recovery driver; and the `bin/speculative-land` wrapper.
- Removed the framework-specific tests and replaced them with a negative
  regression test that requires the production registrations, commands,
  implementations, and files to remain absent.
- Updated current documentation and retained only clearly marked historical
  measurements or incident evidence.

## Validation before landing

- ORC plugin tests: 109 checks passed (33 removal, 27 permission prompt, 49
  script argument passing).
- Python: 254 tests passed across history, remediation, landing, and stress.
- Rust: 349 of 350 passed in the full run; the sole process-group timing test
  then passed alone (1/1). The duplicate test-attribute warning in
  `qualifying_receipt.rs` predates and is outside this deletion.
- `git diff --check`, `rustfmt --check`, Python compilation, and executable help
  checks passed. The remaining command surfaces expose no speculative-land or
  obligation-tracking commands.
