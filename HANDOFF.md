# Logging component committed; fallback integration awaits review

Do not land or activate. Reviewed logging commit:
`d8775b7bbdf6811fab407637235ed38c85981f03` (exact-head narrow checks succeeded).
Fallback `7b51e521fcacbbe876c93d1cd0893c442c1a52f6` is applied, uncommitted and
unstaged. Merged 55 logging tests, 2 runtime controls and 10 driver controls pass.
All three unchanged sampled typed/no-ptrace guest contracts now succeed, with
actual shared-Detcore ftruncate Ok(0) events and retained normal local INFO/RPC.
Original-harness fmt remains red; separate Clippy passes. No PMU integration.
External tracing dependency publication/pins, full CLI and L2 remain pending.

Checked source, exact commands, limitations and execution hashes:
`/home/newton/work/dev-hermit/ignored/liteinst-logging-fallback-integration-20260904/task-note.md`
and adjacent `checked-source.sha256`, `checked-source.tar`, `evidence.sha256`.
New archive SHA256: `1bb538793313e2d7d458ceb581d61326d46dbeaa85c9e3ed26e66bb24affae7e`.
Prior source/evidence remains frozen in the earlier sibling evidence directories.
