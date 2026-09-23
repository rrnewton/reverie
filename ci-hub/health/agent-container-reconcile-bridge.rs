#!/usr/bin/env rust-script
//! Legacy bridge for an ORC process that predates the canonical
//! operational-health workflow.
//!
//! This is retired under Herdr and must not be launched as a current producer.
//! It exits before observation or mutation when the Herdr authority marker is
//! present. It remains only for historical migration compatibility and tests.
//!
//! ```cargo
//! [dependencies]
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! sha2 = "0.10"
//! libc = "0.2"
//! ```

// ZERO-WARNINGS, MADE INTRINSIC RATHER THAN DELEGATED TO AN EXTERNAL GATE.
// A rust-script is invisible to `cargo clippy --workspace`, so the project rule
// held here only for as long as something remembered to cover this file.
//
// ⚠️ A CLOSED, NAMED SET -- DELIBERATELY NOT `deny(warnings)`. These compile at
// INVOCATION, so a blanket deny would let a future toolchain introducing any new
// lint stop the script from RUNNING. That is a production break, not a build
// break, and it would be worse than the warnings it prevents. An enumerated set
// cannot be widened by a toolchain upgrade. New lints are caught instead by
// `scripts/lint-rust-scripts.sh`, which compiles this file under `-D warnings`
// externally, where a failure blocks a commit rather than an execution.
#![deny(
    dead_code,
    unused_imports,
    unused_mut,
    unused_variables,
    unused_must_use
)]
// rust-script does not track #[path] dependencies in its cache key. Keep this
// marker synchronized with agent_snapshot.rs status-contract changes.
// agent-snapshot-schema-1-pane-identities-roles-authority-v7
#[allow(dead_code)]
#[path = "../../scripts/agent_snapshot.rs"]
mod agent_snapshot;
// The ONE capture wrapper, shared by path rather than copied so this script
// cannot drift away from the fix. Only one of its two entry points is used here.
#[allow(dead_code)]
#[path = "../lib/bounded_output.rs"]
mod bounded_output;
use bounded_output::BoundedOutput;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CANONICAL_WORKFLOW: &str = "hermit-dev-operational-health-v1";
const DEFAULT_STATE_URL: &str = "http://127.0.0.1:44109/api/state";
const DEFAULT_INTERVAL_SECS: u64 = 300;

const USAGE: &str = r#"Usage: agent-container-reconcile-bridge.rs [OPTIONS]

Legacy migration only: reconcile managed agent containers while a running ORC
process predates the canonical operational-health workflow. Do not launch this
under Herdr; the bridge exits when Herdr authority or that workflow is live.

Options:
  --once                 Run one observation/reconciliation cycle and exit.
  --interval-secs N      Poll interval (default: 300).
  --state-url URL        ORC state endpoint (default: http://127.0.0.1:44109/api/state).
  --root PATH            dev-hermit root (default: current directory).
  -h, --help             Print this pure help text.
"#;

#[derive(Debug)]
struct Args {
    once: bool,
    interval_secs: u64,
    state_url: String,
    root: PathBuf,
}

#[derive(Debug, Deserialize)]
struct OrcState {
    #[serde(default)]
    agents: Vec<Value>,
    #[serde(default)]
    workflows: Vec<Workflow>,
}

#[derive(Debug, Deserialize)]
struct Workflow {
    name: String,
    #[serde(default)]
    alive: bool,
}

#[derive(Serialize)]
struct AgentSnapshot<'a> {
    schema_version: u8,
    captured_at: u64,
    agents: &'a [Value],
}

enum Cycle {
    Continue,
    CanonicalActive,
    HerdrActive,
}

fn main() {
    match run() {
        Ok(()) => {}
        Err(message) => {
            eprintln!("agent-container-reconcile-bridge: ERROR: {message}");
            exit(2);
        }
    }
}

fn run() -> Result<(), String> {
    let Some(args) = parse_args(env::args().skip(1))? else {
        print!("{USAGE}");
        return Ok(());
    };

    validate_root(&args.root)?;
    loop {
        match run_cycle(&args) {
            Ok(Cycle::CanonicalActive) => {
                println!(
                    "state=canonical-active workflow={CANONICAL_WORKFLOW} summary=temporary-container-reconcile-bridge-stopping"
                );
                return Ok(());
            }
            Ok(Cycle::HerdrActive) => {
                println!(
                    "state=herdr-active summary=temporary-container-reconcile-bridge-stopping"
                );
                return Ok(());
            }
            Ok(Cycle::Continue) if args.once => return Ok(()),
            Ok(Cycle::Continue) => {}
            Err(message) if args.once => return Err(message),
            Err(message) => eprintln!(
                "agent-container-reconcile-bridge: ERROR: {message}; retrying in {}s",
                args.interval_secs
            ),
        }
        thread::sleep(Duration::from_secs(args.interval_secs));
    }
}

fn parse_args(arguments: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut once = false;
    let mut interval_secs = DEFAULT_INTERVAL_SECS;
    let mut state_url = DEFAULT_STATE_URL.to_string();
    let mut root =
        env::current_dir().map_err(|error| format!("read current directory: {error}"))?;
    let mut arguments = arguments.peekable();

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(None),
            "--once" => once = true,
            "--interval-secs" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--interval-secs requires a value".to_string())?;
                interval_secs = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid --interval-secs value {value:?}"))?;
                if interval_secs == 0 {
                    return Err("--interval-secs must be positive".to_string());
                }
            }
            "--state-url" => {
                state_url = arguments
                    .next()
                    .ok_or_else(|| "--state-url requires a value".to_string())?;
            }
            "--root" => {
                root = PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| "--root requires a value".to_string())?,
                );
            }
            _ => return Err(format!("unknown argument {argument:?}\n{USAGE}")),
        }
    }

    Ok(Some(Args {
        once,
        interval_secs,
        state_url,
        root,
    }))
}

fn validate_root(root: &Path) -> Result<(), String> {
    let reconciler = root.join("scripts/agent-podman.rs");
    if !reconciler.is_file() {
        return Err(format!(
            "dev-hermit root has no executable reconciler at {}",
            reconciler.display()
        ));
    }
    Ok(())
}

fn run_cycle(args: &Args) -> Result<Cycle, String> {
    if herdr_authority_marker_exists(&args.root) {
        return Ok(Cycle::HerdrActive);
    }
    let state = fetch_state(&args.state_url)?;
    if canonical_workflow_alive(&state) {
        return Ok(Cycle::CanonicalActive);
    }

    let snapshot_path = args.root.join("ignored/ci-hub/agent-snapshot.json");
    persist_snapshot(&snapshot_path, &state.agents)?;
    let status = Command::new(args.root.join("scripts/agent-podman.rs"))
        .arg("reconcile")
        .arg("--agents-json")
        .arg(&snapshot_path)
        .arg("--apply")
        .current_dir(&args.root)
        .status()
        .map_err(|error| format!("start agent-podman reconciler: {error}"))?;
    let code = status.code().unwrap_or(2);
    if code > 1 {
        return Err(format!("agent-podman reconciler failed with exit {code}"));
    }
    println!(
        "state=bridge-active agents={} reconciler_exit={} summary=canonical-workflow-absent-container-reconciliation-ran",
        state.agents.len(),
        code
    );
    Ok(Cycle::Continue)
}

fn herdr_authority_marker_exists(root: &Path) -> bool {
    let binding = root.join(".tick-hub/herdr-agent-bindings.json");
    match binding.try_exists() {
        Ok(present) => present,
        // An unreadable marker is never permission to resume the legacy writer.
        Err(_) => true,
    }
}

fn fetch_state(url: &str) -> Result<OrcState, String> {
    let output = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--max-time",
            "15",
            url,
        ])
        .bounded_output()
        .map_err(|error| format!("start curl for ORC state: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "fetch ORC state failed with exit {}: {}",
            output.status.code().unwrap_or(2),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| format!("parse ORC state JSON: {error}"))
}

fn canonical_workflow_alive(state: &OrcState) -> bool {
    state
        .workflows
        .iter()
        .any(|workflow| workflow.name == CANONICAL_WORKFLOW && workflow.alive)
}

fn persist_snapshot(path: &Path, agents: &[Value]) -> Result<(), String> {
    let state_root = path.parent().and_then(Path::parent).and_then(Path::parent);
    if path.file_name().and_then(|value| value.to_str()) == Some("agent-snapshot.json") {
        if let Some(root) = state_root {
            if herdr_authority_marker_exists(root) {
                return Err("refuse legacy snapshot: Herdr authority marker is active".to_string());
            }
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("snapshot path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create snapshot directory {}: {error}", parent.display()))?;
    let captured_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("read system clock: {error}"))?
        .as_secs();
    let snapshot = AgentSnapshot {
        schema_version: 1,
        captured_at,
        agents,
    };
    let temporary = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let bytes = serde_json::to_vec(&snapshot)
        .map_err(|error| format!("serialize agent snapshot: {error}"))?;
    let validated = agent_snapshot::AgentSnapshot::parse(&bytes)
        .map_err(|error| format!("refuse agent snapshot before publication: {error}"))?;
    validated
        .ensure_nonempty()
        .map_err(|error| format!("refuse agent snapshot before publication: {error}"))?;
    fs::write(&temporary, bytes)
        .map_err(|error| format!("write temporary snapshot {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| {
        format!(
            "atomically publish snapshot {} -> {}: {error}",
            temporary.display(),
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_scheduler_requires_exact_live_workflow() {
        let state = OrcState {
            agents: Vec::new(),
            workflows: vec![
                Workflow {
                    name: CANONICAL_WORKFLOW.to_string(),
                    alive: false,
                },
                Workflow {
                    name: "other".to_string(),
                    alive: true,
                },
            ],
        };
        assert!(!canonical_workflow_alive(&state));
    }

    #[test]
    fn canonical_live_workflow_stops_bridge() {
        let state = OrcState {
            agents: Vec::new(),
            workflows: vec![Workflow {
                name: CANONICAL_WORKFLOW.to_string(),
                alive: true,
            }],
        };
        assert!(canonical_workflow_alive(&state));
    }

    #[test]
    fn snapshot_writer_refuses_unknown_status_before_replacing_the_file() {
        let directory = env::temp_dir().join(format!(
            "agent-snapshot-bridge-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("agent-snapshot.json");
        fs::write(&path, b"last usable observation\n").unwrap();
        let error = persist_snapshot(
            &path,
            &[serde_json::json!({
                "name": "future-agent",
                "status": "future-state",
                "tmux_pane_id": "%7"
            })],
        )
        .unwrap_err();
        assert!(error.contains("future-state"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"last usable observation\n");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn schema_v2_binding_blocks_legacy_writer_without_replacing_canonical_bytes() {
        let root = env::temp_dir().join(format!(
            "agent-snapshot-bridge-authority-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("ignored/ci-hub/agent-snapshot.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::create_dir_all(root.join(".tick-hub")).unwrap();
        fs::write(&path, b"last composite observation\n").unwrap();
        fs::write(
            root.join(".tick-hub/herdr-agent-bindings.json"),
            br#"{"schema_version":2}"#,
        )
        .unwrap();
        let error = persist_snapshot(
            &path,
            &[serde_json::json!({
                "name": "legacy-worker",
                "status": "working",
                "tmux_pane_id": "%7"
            })],
        )
        .unwrap_err();
        assert!(error.contains("Herdr authority marker"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"last composite observation\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn herdr_binding_stops_bridge_before_fetching_or_writing() {
        let root = env::temp_dir().join(format!(
            "agent-snapshot-bridge-stop-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join(".tick-hub")).unwrap();
        fs::write(
            root.join(".tick-hub/herdr-agent-bindings.json"),
            b"corrupt-but-authoritative-marker\n",
        )
        .unwrap();
        let args = Args {
            once: false,
            interval_secs: 1,
            state_url: "http://127.0.0.1:1/must-not-be-fetched".into(),
            root: root.clone(),
        };
        assert!(matches!(run_cycle(&args), Ok(Cycle::HerdrActive)));
        assert!(!root.join("ignored/ci-hub/agent-snapshot.json").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
