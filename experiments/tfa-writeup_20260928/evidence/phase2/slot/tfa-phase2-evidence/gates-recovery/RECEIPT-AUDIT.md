# TFA phase-two Muse follow-ups: recovery gate audit

Recovered by claude-opus-5-5 after the prior worker disappeared. The earlier
gates-followup/production.out has no .rc or .finished receipt and stops after
two signed-Codex failures; it is INTERRUPTED and is not used as evidence.

Tree (all gates, verified before/after each mutation and after production):
  process.py              43577df8a3418d18844bd586cfaccad97dc5ddfc0c808dee913120b6868e67d6
  payload_wrapper.py      160c7455e2a44d3ffa26bb8a78265e6f66a52023bcb7dc011a4fdbc411b8c824 (6819 bytes)
  hermit.py (unmodified)  cf717ee4263b3dad96c04679864b471a3822bd440df160c81146e3dc2c01147e
  tests/test_prototype.py 9bec3dbf0a56437e595ec3c38900dafec68068ce3eb7cfb37da827563d389930
Parent: 795572d1224646221b707d57bcbdb774ad615a2c

rc captured before filtering (gate() in run_gates.sh):
  focused       rc=0   5.8s  Pass 9 / Fail 0
  unit          rc=0  13.1s  Pass 159 / Fail 0 (157 at parent + 2 new tests)
  manifest      rc=0   0.8s  {"schema_version": 1, "cases": 3}
  pyre          rc=0   1.3s  No type errors found
  arc f         rc=0   3.2s  No lint issues; tree unchanged
  arc lint -a   rc=0  35.6s  No lint issues; tree unchanged
  mutant-argv   rc=32  8.4s  EXPECTED RED: empty-argv refusal deleted ->
                             test_wrapper_refuses_empty_upstream_argv "SystemExit not raised"
  mutant-order  rc=32  7.5s  EXPECTED RED: reviewed state evaluated before provenance ->
                             both packaged-Hermit tests see BLOCKED, not PROVENANCE_MISMATCH
  production    rc=32 405.9s Pass 36 / Fail 4 (08:19:35Z-08:26:21Z)

Production classification: BLOCKED, not green. All 4 failures are
test_real_codex_{native_workspace_sandbox_and_tfa_cleanup,
final_message_cannot_be_redirected_by_tool, candidate_handoff_uses_controller_buck,
reviewer_final_message_cannot_be_redirected}; each raises the identical
fail-closed preflight ProcessError (tfa.codex-environment-preflight.v3;
codex-cli 0.156.0; state=blocked): owner-managed Codex lacks daemon-socket
fixes 7cec3e110ce6dc5c14704dfa06f6db9c23a530fc and
a708fc683934d7d7ea7560354968fedd2be28581. Same 36/40 set as gates-hardening.
Signed Codex release unchanged, so 36/40 stands.
