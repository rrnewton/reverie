# Provenance

Author: agentcloud worker `sub-re-poc` (session 968f8eb6-9a77-4b33-8faa-c34565fb3217, OnDemand twshared4569.04.vcn5, fbsource a11f6222bbad), 2026-09-30.
Each tracked file below is copied byte for byte from a private paste, by coord2 on 2026-10-01.

| Path | Paste |
|---|---|
| `REPORT.md` | P2529915309 |
| `raw-evidence.txt` | P2529899569 |
| full logs (1.4 MB, not tracked: over the repository size limit; local copy in ignored/re-poc-20260930/ on devbig030) | P2529974260 |

Result: passes-on-RE. The unmodified fbcode test `hermit_run_default__custombin_minimal_hello` (`hermit run`, Reverie ptrace backend) ran on linux-remote-execution as a remote, non-cached test action and passed. RE workers allow ptrace, namespaces and perf. The remaining failures are harness path or input issues, plus a CPU-model problem: a Sapphire Rapids branch-counter event. See REPORT.md.
