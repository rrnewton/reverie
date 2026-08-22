# Single-variable submodule bumps

Every parent product gitlink advance is an A/B experiment:

1. **A is known green.** Record a durable evidence file containing the full
   parent A SHA. For determinism-related subjects, A needs a powered repeated
   probe; one green run is not evidence against a probabilistic hang.
2. **Change one variable.** Fetch the chosen submodule's `origin/main`, require
   a fast-forward, and create B whose tree changes exactly that one gitlink.
   Do not mix source, policy, generated files, or a second gitlink into B.
3. **Verify B.** Run the declared argv with stdout/stderr in a timestamped
   durable log. A failing B stays isolated for diagnosis; do not amend unrelated
   fixes into it and obscure attribution.
4. **Record the result.** Append the typed A/B record to
   `ignored/ci-hub/submodule-bumps.jsonl`. The operation's wall/CPU JSON and
   verification log live under `ignored/ci-hub/submodule-bumps/<run-id>/`.

Use a clean isolated parent bump branch/worktree. Inspect without mutation:

```bash
make single-submodule-bump ARGS='plan \
  --submodule reverie \
  --base <40-hex-known-green-parent-A> \
  --base-evidence <evidence-containing-A>'
```

Execute only after the plan passes and a verification argv is selected:

```bash
make single-submodule-bump ARGS='run \
  --submodule reverie \
  --base <40-hex-known-green-parent-A> \
  --base-evidence <evidence-containing-A> \
  -- <verification-program> <arg> ...'
```

For a determinism-related bump, add `--verification-kind determinism` and
`--matched-load-calibration <path>`. The reference powered probe is
`experiments/multisect_detcore_misc_20260803/matched.sh`: it co-schedules A, B,
and a known-bad calibrator; counts a wave only when the calibrator is FLAKY; and
classifies mixed results as red. Its established invocation shape is:

```bash
experiments/multisect_detcore_misc_20260803/matched.sh \
  32 20 10 a:<A-test-bin> b:<B-test-bin> z_HEAD:<known-bad-test-bin>
```

The verification argv may invoke that probe through an explicit `bash -lc`
wrapper when paths need expansion. The procedure does **not** trust
`matched.sh`'s process status (the probe prints classifications and normally
exits zero): it parses every wave, discards waves where `z_HEAD` is PASS,
requires A and B to be PASS in every powered wave, and records FLAKY/FAIL as
red. An underpowered run or a red A is not a B pass. A single passing sample
must never be recorded as a green determinism result.

## Result schema

`submodule-bumps.jsonl` is append-only and locked. Schema v1 records:

- identity: `run_id`, `recorded_at`, `submodule`;
- isolated transition: `parent_a`, `parent_b`, `submodule_a`, `submodule_b`,
  `submodule_remote_main`;
- evidence: `base_evidence`, `verification_kind`, `verification_command`,
  `matched_load_calibration`;
- outcome: `verification_command_exit`, semantic `verification_exit`,
  `verdict`, `matched_load_summary`, `log_path`, `cost_record_path`.

`parent_a`/`parent_b` are stable joins for the local/GitHub commit-history
store. Full-state results are never overwritten.

## Agent-utils updates

The parent `agent-utils` path is a symlink to `hermit/agent-utils`, not an
independently pinned parent submodule. This tool therefore accepts only the
parent product gitlinks: `hermit`, `reverie`, and `liteinst2`.

An agent-utils update changes the nested gitlink in Hermit, commits and verifies
that change there, and then advances the parent `hermit` gitlink with the
ordinary A/B procedure above. Do not point the parent symlink at a separate
checkout or try to record an `agent-utils` parent gitlink bump.
