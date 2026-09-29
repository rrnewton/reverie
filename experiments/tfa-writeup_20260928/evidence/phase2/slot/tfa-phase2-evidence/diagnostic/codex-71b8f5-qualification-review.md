# Codex 71b8f5 qualification review — NOT QUALIFIED

Verdict: **NOT QUALIFIED** (exact bytes fail the native workspace-sandbox contract on this host; tree refusal is correct).

## Identity (exact bytes, no version-equality inference)
- Requested: `/usr/local/bin/codex` -> resolved `/usr/local/bin/codex_cli/codex`
- `regular_file_record`: mode 0755 (493), size 488879584, SHA256 `71b8f5bde99ce8b4cff6ce7c7cbe79a0fe8d6bd36a4427e5d308bda4d065ebf0`
- Version: `codex-cli 0.156.0`; stderr in allowed set (`Codex CLI at Meta…Using AI Gateway (Azure Codex upstream)`)
- Schema: `tfa.codex-environment-preflight.v1` in `fbcode/testinfra/tfa/process.py:61,70` `_CODEX_COMPATIBILITY` (exact `(version,sha256)` map)
- Prior reviewed identity: `(codex-cli 0.156.0, 8ec5b75747642abcdb71a16a7639f2fad3423d3a3fef371a7920432b50acc4b9)` -> `blocked: cannot establish app-server socket mount isolation`. Current SHA absent from map -> default `unsupported`. Same version, different bytes: not transferable (proven below, the failure reproduces).

## Probes (bounded, read-only tree; all writes under /tmp/tfa-phase2, /dev/shm temp)
Runner: `PYTHONPATH=/tmp/tfa-phase2-controller-followup-20260924/fbcode /usr/local/bin/fbpython`

| Probe | Command (essence) | Result ID / evidence | Outcome |
|---|---|---|---|
| Preflight exact | `_codex_environment_preflight(cwd, qual-71b8f5/preflight-artifacts, None)` | `preflight-artifacts/identity.json` (sealed record, state=`unsupported`, blocker=`executable version has no reviewed workspace-sandbox compatibility result`); log SHA256 `f63d25a2717fb62ef399d50aa4864f08db7bd9ac04ef7dccca6244a07ac8d077` | REFUSED (expected) |
| Bootstrap + cleanup | `run_process(codex,(--version,),30s)` = `test_real_codex_bootstraps_with_bound_agent_identity` contract | `qual71-bootstrap-*`, log SHA256 `10409e50592f03861b6a2b77aab0dfa52640d23160d0ec5744004ce18e8fec29` | PASS: rc0, `payload_isolation=codex-native-bootstrap-only`, `containment_empty=True`, cgroup absent after, no leftover `tfa-*` cgroups/processes |
| Native workspace sandbox (confinement/forbidden/network/cgroup) | Direct `codex exec` with `codex_arguments()` (guarded `--sandbox workspace-write`, `network_access=false`, `exclude_slash_tmp`) mirroring `test_real_codex_native_workspace_sandbox_and_tfa_cleanup`, tool probe writes nonce to workspace, attempts `/dev/shm` forbidden write, socket, cgroup mkdir; 150s bound, elapsed 14.5s | Base `/tmp/tfa-phase2/qual71-native-zhzkuxwo`, session `01a0d5a8-9910-7691-b6c9-70ccc27912bd`, stdout SHA256 `468cecb8a157c07570a2a7771a2c3a064856ce6cc8dd04589e25c9a824496902`, log SHA256 `b5a66720716e6b53665c229b67fcfdd4f720c13f0991cc1654b2a0bc4c106479` | **FAIL (blocker)**: report `{"nonce":"","forbidden":"error building bubblewrap command: cannot establish app-server socket mount isolation","network":"","cgroup":""}` — tool never executed (`tool-nonce` absent, rc0 is agent error-report only). Identical blocker text to prior `8ec5b757` identity |
| Forbidden checkout/outside, network denial, cgroup | Same tool probe (fields above) | — | BLOCKED by mount-isolation failure; cannot be established for this binary (host-visible forbidden file also absent, but vacuously — no tool ran) |
| Tool/final-message non-redirection, resume/fresh | `test_real_codex_final_message_cannot_be_redirected_by_tool` design (fresh+resume, `publish_codex_report` symlink refusal) | Fresh session emitted (above) but sandbox never starts | BLOCKED by same root cause; `publish_codex_report` itself is binary-independent and covered by prototype tests, not exact-binary evidence |
| Sealed evidence | Preflight `identity.json`, bootstrap `metadata.json`, probe stdout/stderr/report retained under `/tmp/tfa-phase2/qual-71b8f5`, `qual71-native-zhzkuxwo` | hashes above | PASS (all failures preserved, nothing edited in tree) |

## Limits
- No allowlist/manifest/tree file was edited; production `run_process(exec)` remains gated by the same preflight (refusal reproduced, not bypassed in production path — direct exec used only as bounded probe).
- Single host (this devserver), single run per probe (14.5s sandbox, 30s bootstrap); mount isolation is host+binary joint property — result does not generalize to other hosts, and a pass elsewhere would not qualify this host.
- `code exec` rc0 with structured error must not be read as success; criterion is tool execution (nonce), per production test assertions (`preflight state=eligible`, nonce match, network `EPERM`).

## Blockers for QUALIFIED
1. Exact binary `71b8f5bde99ce8b4cff6ce7c7cbe79a0fe8d6bd36a4427e5d308bda4d065ebf0` cannot establish app-server socket mount isolation (`bubblewrap`) on this host — same failure as blocked `8ec5b757…`. Requires a binary/host where the guarded sandbox starts and all production probes pass, then a reviewed `(version,sha256)->eligible` entry; this audit makes no such change.
