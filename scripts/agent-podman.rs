#!/usr/bin/env rust-script
//! Run and reconcile Podman containers with durable agent ownership.
//!
//! Containers created through this tool are labelled with the creating agent,
//! exact agent invocation, task, and lifetime.  The operational reconciler may
//! remove only an `agent`-lifetime container whose exact invocation is gone.
//! Task-retained and legacy unlabelled containers are report-only.
//!
//! ```cargo
//! [dependencies]
//! fs2 = "0.4"
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! ```

// rust-script does not track #[path] dependencies in its cache key. Keep this
// marker synchronized with agent_snapshot.rs status-contract changes.
// agent-snapshot-schema-1-statuses-v2
#[allow(dead_code)]
#[path = "agent_snapshot.rs"]
mod agent_snapshot;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{exit, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MANAGED_LABEL: &str = "io.dev-hermit.agent-podman";
const MANAGED_VERSION: &str = "v1";
const AGENT_LABEL: &str = "io.dev-hermit.owner-agent";
const INVOCATION_LABEL: &str = "io.dev-hermit.owner-invocation";
const PANE_LABEL: &str = "io.dev-hermit.owner-pane";
const TASK_LABEL: &str = "io.dev-hermit.owner-task";
const LIFETIME_LABEL: &str = "io.dev-hermit.lifetime";
/// Key under which `discover_live_invocations` files the fleet-wide `/proc` sweep.
/// Its PRESENCE is the proof that the sweep completed; see `classify`.
const PROCESS_SCAN_KEY: &str = "@proc:all";
const SNAPSHOT_MAX_AGE_SECS: u64 = 10 * 60;
const QUERY_TIMEOUT_SECS: &str = "3s";
const EVIDENCE_TIMEOUT_SECS: &str = "1s";
const STOP_TIMEOUT_SECS: &str = "13s";
const REMOVE_TIMEOUT_SECS: &str = "3s";
const MAX_PROCESS_EVIDENCE: usize = 10;
const REGISTRATION_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(30);

const USAGE: &str = r#"Usage: agent-podman.rs COMMAND [OPTIONS]

Commands:
  quickstart
      Print the short agent workflow. Pure: no Podman or filesystem access.

  run --task TASK [--lifetime agent|task] -- PODMAN-RUN-ARGS...
  create --task TASK [--lifetime agent|task] -- PODMAN-CREATE-ARGS...
      Run `podman run` or `podman create` with durable ownership labels.
      Lifetime defaults to `agent`; use `task` only for an environment that
      must survive creator recycling and will be explicitly transferred.

  transfer CONTAINER --task TASK [--lifetime agent|task]
      Claim a managed container for the current agent invocation. This is the
      only supported handoff for a task-retained container.

  audit --agents-json PATH [--live-invocations PATH] [--json]
      Classify managed, task-retained, and legacy containers. Never mutates.

  reconcile --agents-json PATH [--live-invocations PATH] [--json] [--apply]
      With --apply, gracefully stop and remove only managed agent-lifetime
      containers whose exact owner invocation is gone. Never force-removes,
      and never removes task-retained or unlabelled containers.

  release-audit --fence-token TOKEN --owner AGENT... [--target ABSOLUTE-PATH...] [--json]
      Release-only authority: refuse any container owned by AGENT or whose
      effective host mount overlaps a target. The caller must already hold the
      container lifecycle lock exclusively; unavailable/incomplete evidence
      fails closed.

Environment:
  DG_AGENT_NAME              Required for run/create/transfer.
  META_3PAI_INVOCATION_ID    Exact agent incarnation (CODING_AGENT_METADATA is
                             accepted as a fallback).
  TMUX_PANE                  Owning pane recorded for diagnosis.
  DEV_HERMIT_CONTAINER_STATE Override the ignored ownership registry path.
  AGENT_PODMAN_BIN           Override podman (tests only).
"#;

const QUICKSTART: &str = r#"agent-podman quickstart

1. Create every agent container through this wrapper, not raw `podman run`:
   ./scripts/agent-podman.rs run --task TASK -- --rm IMAGE COMMAND
2. Detached environments default to the agent's lifetime and are reclaimed
   after that exact agent invocation disappears:
   ./scripts/agent-podman.rs run --task TASK -- -d --name NAME IMAGE COMMAND
3. Use `--lifetime task` only when the environment must outlive the creator.
   Its next owner must run `transfer NAME --task TASK`; it is never auto-reaped.
4. Inspect without mutation:
   ./scripts/agent-podman.rs audit --agents-json ignored/ci-hub/agent-snapshot.json
5. Legacy unlabelled containers are warnings, never automatic deletion targets.
"#;

#[derive(Debug)]
enum AppError {
    Usage(String),
    Message(String),
    Io {
        context: String,
        source: io::Error,
    },
    Json {
        context: String,
        source: serde_json::Error,
    },
}

impl std::fmt::Display for AppError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message) | Self::Message(message) => write!(formatter, "{message}"),
            Self::Io { context, source } => write!(formatter, "{context}: {source}"),
            Self::Json { context, source } => write!(formatter, "{context}: {source}"),
        }
    }
}

type Result<T> = std::result::Result<T, AppError>;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Lifetime {
    Agent,
    Task,
}

impl Lifetime {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "agent" => Ok(Self::Agent),
            "task" => Ok(Self::Task),
            _ => Err(AppError::Usage(format!(
                "invalid lifetime {value:?}; expected agent or task"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Task => "task",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Ownership {
    container_id: String,
    owner_agent: String,
    owner_invocation: String,
    owner_pane: String,
    task: String,
    lifetime: Lifetime,
    updated_at: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct Registry {
    schema_version: u8,
    containers: BTreeMap<String, Ownership>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            schema_version: 1,
            containers: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct AgentIdentity {
    name: String,
    invocation: String,
    pane: String,
}

#[derive(Clone, Debug)]
struct Container {
    id: String,
    name: String,
    state: String,
    labels: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Disposition {
    Live,
    Reclaimable,
    TransferRequired,
    OwnerUnknown,
    Unmanaged,
}

#[derive(Debug, Serialize)]
struct ContainerReport {
    id: String,
    name: String,
    state: String,
    disposition: Disposition,
    owner_agent: Option<String>,
    owner_invocation: Option<String>,
    task: Option<String>,
    lifetime: Option<Lifetime>,
    namespace: Option<String>,
    zombies: usize,
    action: String,
}

#[derive(Debug, Serialize)]
struct ReconcileReport {
    state: String,
    managed: usize,
    live: usize,
    reclaimed: usize,
    reclaimable: usize,
    transfer_required: usize,
    owner_unknown: usize,
    unmanaged: usize,
    zombie_containers: usize,
    zombies: usize,
    details: Vec<ContainerReport>,
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ReleaseAuditDetail {
    id: String,
    name: String,
    owner_agent: Option<String>,
    matching_mounts: Vec<String>,
    reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ReleaseAuditReport {
    state: String,
    inspected: usize,
    stale_registry_entries_removed: usize,
    owner_matches: usize,
    target_mounts: usize,
    details: Vec<ReleaseAuditDetail>,
    container_observation: Vec<ReleaseContainerObservation>,
}

#[derive(Debug, Serialize)]
struct ReleaseContainerObservation {
    id: String,
    name: String,
    state: String,
    ps_labels: BTreeMap<String, String>,
    inspect_labels: BTreeMap<String, String>,
    registry_ownership: Option<Ownership>,
    mount_sources: Vec<String>,
    resolved_mounts: Vec<String>,
}

struct ReleaseInspection {
    labels: BTreeMap<String, String>,
    mount_sources: Vec<String>,
    mounts: Vec<PathBuf>,
}

#[derive(Debug)]
enum Cli {
    Quickstart,
    Launch {
        mode: String,
        task: String,
        lifetime: Lifetime,
        arguments: Vec<String>,
    },
    Transfer {
        container: String,
        task: String,
        lifetime: Lifetime,
    },
    Audit {
        agents_json: PathBuf,
        live_invocations: Option<PathBuf>,
        json: bool,
        apply: bool,
    },
    ReleaseAudit {
        fence_token: String,
        owners: BTreeSet<String>,
        targets: Vec<PathBuf>,
        json: bool,
    },
}

fn main() {
    match run(env::args().skip(1).collect()) {
        Ok(code) => exit(code),
        Err(AppError::Usage(message)) => {
            eprintln!("agent-podman: {message}\n\n{USAGE}");
            exit(2);
        }
        Err(error) => {
            eprintln!("agent-podman: {error}");
            exit(1);
        }
    }
}

fn run(arguments: Vec<String>) -> Result<i32> {
    let command = parse_cli(arguments)?;
    if env::var_os("CI_HUB_DOCS_PARSE_ONLY").is_some() && !matches!(command, Cli::Quickstart) {
        let name = match &command {
            Cli::Quickstart => unreachable!(),
            Cli::Launch { mode, .. } => mode.as_str(),
            Cli::Transfer { .. } => "transfer",
            Cli::Audit { apply: true, .. } => "reconcile",
            Cli::Audit { apply: false, .. } => "audit",
            Cli::ReleaseAudit { .. } => "release-audit",
        };
        println!("state=parse-ok command={name} summary=arguments-valid-no-action-taken");
        return Ok(0);
    }
    match command {
        Cli::Quickstart => {
            print!("{QUICKSTART}");
            Ok(0)
        }
        Cli::Launch {
            mode,
            task,
            lifetime,
            arguments,
        } => launch(&mode, &task, lifetime, &arguments),
        Cli::Transfer {
            container,
            task,
            lifetime,
        } => transfer(&container, &task, lifetime),
        Cli::Audit {
            agents_json,
            live_invocations,
            json,
            apply,
        } => {
            let result = reconcile(&agents_json, live_invocations.as_deref(), json, apply);
            if let Err(error) = &result {
                // tick-hub fires this command on a non-zero exit and captures stdout.
                // Errors raised before ReconcileReport exists used to reach only stderr,
                // leaving its required {summary} field unresolved. Preserve the non-zero
                // result, but make the completed failure actionable to the same parser as
                // ordinary action-required reports. JSON callers keep an unmixed stream.
                if !json {
                    emit_reconcile_error(error);
                }
            }
            result
        }
        Cli::ReleaseAudit {
            fence_token,
            owners,
            targets,
            json,
        } => release_audit(&fence_token, &owners, &targets, json),
    }
}

fn parse_cli(arguments: Vec<String>) -> Result<Cli> {
    let Some(command) = arguments.first().map(String::as_str) else {
        return Err(AppError::Usage("missing command".to_string()));
    };
    if matches!(command, "-h" | "--help" | "help") {
        print!("{USAGE}");
        exit(0);
    }
    if command == "quickstart" {
        if arguments.len() != 1 {
            return Err(AppError::Usage(
                "quickstart accepts no arguments".to_string(),
            ));
        }
        return Ok(Cli::Quickstart);
    }
    if matches!(command, "run" | "create") {
        return parse_launch(command, &arguments[1..]);
    }
    if command == "transfer" {
        return parse_transfer(&arguments[1..]);
    }
    if matches!(command, "audit" | "reconcile") {
        return parse_audit(command, &arguments[1..]);
    }
    if command == "release-audit" {
        return parse_release_audit(&arguments[1..]);
    }
    Err(AppError::Usage(format!("unknown command {command:?}")))
}

fn parse_release_audit(arguments: &[String]) -> Result<Cli> {
    let mut fence_token = None;
    let mut owners = BTreeSet::new();
    let mut targets = Vec::new();
    let mut json = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--fence-token" => {
                index += 1;
                fence_token = arguments
                    .get(index)
                    .filter(|value| !value.trim().is_empty())
                    .cloned();
            }
            "--owner" => {
                index += 1;
                let owner = arguments
                    .get(index)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| AppError::Usage("--owner needs a value".to_string()))?;
                owners.insert(owner.clone());
            }
            "--target" => {
                index += 1;
                let target = arguments
                    .get(index)
                    .map(PathBuf::from)
                    .ok_or_else(|| AppError::Usage("--target needs a value".to_string()))?;
                if !normalized_absolute_path(&target) {
                    return Err(AppError::Usage(format!(
                        "--target must be a normalized absolute path: {}",
                        target.display()
                    )));
                }
                targets.push(target);
            }
            "--json" => json = true,
            value => return Err(AppError::Usage(format!("unexpected option {value:?}"))),
        }
        index += 1;
    }
    if owners.is_empty() {
        return Err(AppError::Usage(
            "release-audit requires at least one --owner".to_string(),
        ));
    }
    targets.sort();
    targets.dedup();
    Ok(Cli::ReleaseAudit {
        fence_token: required_nonempty("--fence-token", fence_token)?,
        owners,
        targets,
        json,
    })
}

fn parse_launch(mode: &str, arguments: &[String]) -> Result<Cli> {
    let mut task = None;
    let mut lifetime = Lifetime::Agent;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--task" => {
                index += 1;
                task = arguments.get(index).cloned();
            }
            "--lifetime" => {
                index += 1;
                lifetime = Lifetime::parse(
                    arguments
                        .get(index)
                        .ok_or_else(|| AppError::Usage("--lifetime needs a value".to_string()))?,
                )?;
            }
            "--" => {
                let podman_arguments = arguments[index + 1..].to_vec();
                if podman_arguments.is_empty() {
                    return Err(AppError::Usage("podman arguments are empty".to_string()));
                }
                reject_reserved_arguments(&podman_arguments)?;
                return Ok(Cli::Launch {
                    mode: mode.to_string(),
                    task: required_nonempty("--task", task)?,
                    lifetime,
                    arguments: podman_arguments,
                });
            }
            value => return Err(AppError::Usage(format!("unexpected option {value:?}"))),
        }
        index += 1;
    }
    Err(AppError::Usage(
        "run/create needs `-- PODMAN-ARGS...`".to_string(),
    ))
}

fn parse_transfer(arguments: &[String]) -> Result<Cli> {
    let container = arguments
        .first()
        .filter(|value| !value.starts_with('-'))
        .cloned()
        .ok_or_else(|| AppError::Usage("transfer needs CONTAINER".to_string()))?;
    let mut task = None;
    let mut lifetime = Lifetime::Task;
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--task" => {
                index += 1;
                task = arguments.get(index).cloned();
            }
            "--lifetime" => {
                index += 1;
                lifetime = Lifetime::parse(
                    arguments
                        .get(index)
                        .ok_or_else(|| AppError::Usage("--lifetime needs a value".to_string()))?,
                )?;
            }
            value => return Err(AppError::Usage(format!("unexpected option {value:?}"))),
        }
        index += 1;
    }
    Ok(Cli::Transfer {
        container,
        task: required_nonempty("--task", task)?,
        lifetime,
    })
}

fn parse_audit(command: &str, arguments: &[String]) -> Result<Cli> {
    let mut agents_json = None;
    let mut live_invocations = None;
    let mut json = false;
    let mut apply = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--agents-json" => {
                index += 1;
                agents_json = arguments.get(index).map(PathBuf::from);
            }
            "--live-invocations" => {
                index += 1;
                live_invocations = arguments.get(index).map(PathBuf::from);
            }
            "--json" => json = true,
            "--apply" if command == "reconcile" => apply = true,
            "--apply" => {
                return Err(AppError::Usage(
                    "audit is read-only; use reconcile --apply".to_string(),
                ))
            }
            value => return Err(AppError::Usage(format!("unexpected option {value:?}"))),
        }
        index += 1;
    }
    Ok(Cli::Audit {
        agents_json: agents_json
            .ok_or_else(|| AppError::Usage("--agents-json is required".to_string()))?,
        live_invocations,
        json,
        apply,
    })
}

fn required_nonempty(name: &str, value: Option<String>) -> Result<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::Usage(format!("{name} is required")))
}

fn reject_reserved_arguments(arguments: &[String]) -> Result<()> {
    let reserved_labels = [
        MANAGED_LABEL,
        AGENT_LABEL,
        INVOCATION_LABEL,
        PANE_LABEL,
        TASK_LABEL,
        LIFETIME_LABEL,
    ];
    for (index, argument) in arguments.iter().enumerate() {
        if argument == "--cidfile" || argument.starts_with("--cidfile=") {
            return Err(AppError::Usage(
                "--cidfile is reserved for lifecycle registration".to_string(),
            ));
        }
        if reserved_labels.iter().any(|key| argument.contains(key))
            && (argument == "--label"
                || argument.starts_with("--label=")
                || index > 0 && arguments[index - 1] == "--label")
        {
            return Err(AppError::Usage(
                "io.dev-hermit lifecycle labels are reserved".to_string(),
            ));
        }
    }
    Ok(())
}

fn current_identity() -> Result<AgentIdentity> {
    let name = env::var("DG_AGENT_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::Message("DG_AGENT_NAME is required".to_string()))?;
    let invocation = env::var("META_3PAI_INVOCATION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(invocation_from_agent_metadata)
        .ok_or_else(|| {
            AppError::Message(
                "META_3PAI_INVOCATION_ID or CODING_AGENT_METADATA invocation_id is required"
                    .to_string(),
            )
        })?;
    let pane = env::var("TMUX_PANE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "outside-tmux".to_string());
    Ok(AgentIdentity {
        name,
        invocation,
        pane,
    })
}

fn invocation_from_agent_metadata() -> Option<String> {
    let metadata = env::var("CODING_AGENT_METADATA").ok()?;
    metadata.split(',').find_map(|field| {
        field
            .trim()
            .strip_prefix("invocation_id=")
            .map(str::to_string)
    })
}

fn launch(mode: &str, task: &str, lifetime: Lifetime, arguments: &[String]) -> Result<i32> {
    let identity = current_identity()?;
    let state_path = state_path()?;
    let cid_directory = state_path
        .parent()
        .ok_or_else(|| AppError::Message("ownership state has no parent".to_string()))?;
    fs::create_dir_all(cid_directory).map_err(|source| AppError::Io {
        context: format!("create {}", cid_directory.display()),
        source,
    })?;
    let cidfile = cid_directory.join(format!(
        ".agent-podman.{}.{}.cid",
        std::process::id(),
        now_secs()
    ));
    // Container creation and durable registration are one lifecycle operation.
    // `release-worktree.rs` takes this exact lifecycle lock exclusively before
    // proving that no owner container can retain or newly acquire a slot path.
    let mut lifecycle_lock = Some(open_lifecycle_lock_shared()?);
    let labels = ownership_labels(&identity, task, lifetime);
    let mut command = Command::new(podman_bin());
    command.arg(mode);
    for (key, value) in labels {
        command.arg("--label").arg(format!("{key}={value}"));
    }
    command.arg("--cidfile").arg(&cidfile).args(arguments);
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());
    let mut child = command.spawn().map_err(|source| AppError::Io {
        context: format!("start podman {mode}"),
        source,
    })?;

    let mut registered_id = None;
    let register_visible_id = |id: &str| -> Result<()> {
        let ownership = Ownership {
            container_id: id.to_string(),
            owner_agent: identity.name.clone(),
            owner_invocation: identity.invocation.clone(),
            owner_pane: identity.pane.clone(),
            task: task.to_string(),
            lifetime,
            updated_at: now_secs(),
        };
        update_registry(|registry| {
            registry.containers.insert(id.to_string(), ownership);
        })?;
        wait_for_release_visibility(id, REGISTRATION_VISIBILITY_TIMEOUT)
    };
    let status = loop {
        if registered_id.is_none() {
            if let Ok(raw) = fs::read_to_string(&cidfile) {
                let id = raw.trim();
                if !id.is_empty() {
                    if let Err(error) = register_visible_id(id) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error);
                    }
                    registered_id = Some(id.to_string());
                    drop(lifecycle_lock.take());
                }
            }
        }
        if let Some(status) = child.try_wait().map_err(|source| AppError::Io {
            context: format!("wait for podman {mode}"),
            source,
        })? {
            break status;
        }
        thread::sleep(Duration::from_millis(50));
    };

    if registered_id.is_none() {
        if let Ok(raw) = fs::read_to_string(&cidfile) {
            let id = raw.trim();
            if !id.is_empty() {
                register_visible_id(id)?;
                registered_id = Some(id.to_string());
                drop(lifecycle_lock.take());
            }
        }
    }
    // A command that never produced a cidfile cannot leave a registered
    // container. Retain the lifecycle lock until the child has terminated and
    // that absence is known. A visible container's final cleanup reacquires the
    // same lock so release cannot observe registry mutation outside lifecycle
    // authority.
    drop(lifecycle_lock.take());
    let _ = fs::remove_file(&cidfile);
    if let Some(id) = registered_id {
        let _cleanup_lifecycle_lock = open_lifecycle_lock_shared()?;
        if !container_exists_authoritative(&id)? {
            update_registry(|registry| {
                registry.containers.remove(&id);
            })?;
        }
    }
    Ok(exit_code(status))
}

fn transfer(container: &str, task: &str, lifetime: Lifetime) -> Result<i32> {
    let _lifecycle_lock = open_lifecycle_lock_exclusive()?;
    let identity = current_identity()?;
    let labels = inspect_labels(container)?;
    if labels.get(MANAGED_LABEL).map(String::as_str) != Some(MANAGED_VERSION) {
        return Err(AppError::Message(format!(
            "refusing to claim unlabelled legacy container {container}; recreate it through agent-podman"
        )));
    }
    let id = inspect_id(container)?;
    let ownership = Ownership {
        container_id: id.clone(),
        owner_agent: identity.name,
        owner_invocation: identity.invocation,
        owner_pane: identity.pane,
        task: task.to_string(),
        lifetime,
        updated_at: now_secs(),
    };
    update_registry(|registry| {
        registry.containers.insert(id.clone(), ownership);
    })?;
    println!(
        "state=transferred container={} task={} lifetime={} summary=ownership-transferred-to-current-agent-invocation",
        id,
        task,
        lifetime.as_str()
    );
    Ok(0)
}

fn ownership_labels(
    identity: &AgentIdentity,
    task: &str,
    lifetime: Lifetime,
) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        (MANAGED_LABEL, MANAGED_VERSION.to_string()),
        (AGENT_LABEL, identity.name.clone()),
        (INVOCATION_LABEL, identity.invocation.clone()),
        (PANE_LABEL, identity.pane.clone()),
        (TASK_LABEL, task.to_string()),
        (LIFETIME_LABEL, lifetime.as_str().to_string()),
    ])
}

fn reconcile(
    agents_path: &Path,
    live_invocations_path: Option<&Path>,
    json_output: bool,
    apply: bool,
) -> Result<i32> {
    // A mutating reconciliation takes the lifecycle fence exclusively. This
    // serializes classify-through-remove against create registration and
    // transfer, as well as against release. Read-only audits do not mutate.
    let _lifecycle_lock = if apply {
        Some(open_lifecycle_lock_exclusive()?)
    } else {
        None
    };
    let agents = load_live_agents(agents_path)?;
    let invocations = match live_invocations_path {
        // An explicitly supplied evidence file IS an assertion of completeness by
        // its author -- that is what `--live-invocations` means and it is how the
        // fixtures inject a controlled process table. Stamp the completed-scan key
        // so `classify` can still tell it apart from "no evidence was gathered".
        // The tick never takes this path; it takes the sweep below.
        Some(path) => {
            let mut loaded = load_live_invocations(path)?;
            let union: BTreeSet<String> = loaded.values().flatten().cloned().collect();
            loaded.entry(PROCESS_SCAN_KEY.to_string()).or_insert(union);
            loaded
        }
        None => discover_live_invocations(&agents),
    };
    let registry_path = state_path()?;
    let registry_was_present = registry_path.is_file();
    let registry = read_registry_unlocked(&registry_path)?;
    let containers = list_containers()?;
    let mut reclaimed_ids = BTreeSet::new();

    let mut report = ReconcileReport {
        state: "ok".to_string(),
        managed: 0,
        live: 0,
        reclaimed: 0,
        reclaimable: 0,
        transfer_required: 0,
        owner_unknown: 0,
        unmanaged: 0,
        zombie_containers: 0,
        zombies: 0,
        details: Vec::new(),
        errors: Vec::new(),
    };

    let mut reclaim_candidates: Vec<(usize, Container)> = Vec::new();
    for (container_index, container) in containers.into_iter().enumerate() {
        let (zombies, namespace) = if container_index < MAX_PROCESS_EVIDENCE {
            container_process_evidence(&container)
        } else {
            (0, None)
        };
        if zombies > 0 {
            report.zombie_containers += 1;
            report.zombies += zombies;
        }
        let ownership = effective_ownership(&container, &registry);
        let Some(ownership) = ownership else {
            report.unmanaged += 1;
            report.details.push(ContainerReport {
                id: container.id,
                name: container.name,
                state: container.state,
                disposition: Disposition::Unmanaged,
                owner_agent: None,
                owner_invocation: None,
                task: None,
                lifetime: None,
                namespace,
                zombies,
                action: "report-only: legacy/unlabelled container is never auto-removed"
                    .to_string(),
            });
            continue;
        };
        report.managed += 1;
        let disposition = classify(&ownership, &invocations);
        let action = match disposition {
            Disposition::Live => {
                report.live += 1;
                "none: exact owner invocation is live".to_string()
            }
            Disposition::Reclaimable => {
                report.reclaimable += 1;
                "eligible: exact owner invocation is gone".to_string()
            }
            Disposition::TransferRequired => {
                report.transfer_required += 1;
                "transfer required: task-retained container is never auto-removed".to_string()
            }
            Disposition::OwnerUnknown => {
                report.owner_unknown += 1;
                "report-only: current owner invocation could not be established".to_string()
            }
            Disposition::Unmanaged => unreachable!(),
        };
        let detail_index = report.details.len();
        if apply && disposition == Disposition::Reclaimable {
            reclaim_candidates.push((detail_index, container.clone()));
        }
        report.details.push(ContainerReport {
            id: container.id,
            name: container.name,
            state: container.state,
            disposition,
            owner_agent: Some(ownership.owner_agent),
            owner_invocation: Some(ownership.owner_invocation),
            task: Some(ownership.task),
            lifetime: Some(ownership.lifetime),
            namespace,
            zombies,
            action,
        });
    }
    if apply && !reclaim_candidates.is_empty() {
        let outcomes = thread::scope(|scope| {
            reclaim_candidates
                .into_iter()
                .map(|(index, container)| {
                    scope.spawn(move || {
                        let outcome = gracefully_remove(&container);
                        (index, container, outcome)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join())
                .collect::<Vec<_>>()
        });
        for outcome in outcomes {
            let (index, container, result) = outcome
                .map_err(|_| AppError::Message("container cleanup worker panicked".to_string()))?;
            match result {
                Ok(()) => {
                    report.reclaimable -= 1;
                    report.reclaimed += 1;
                    reclaimed_ids.insert(container.id);
                    report.details[index].action =
                        "reclaimed: graceful stop and remove completed".to_string();
                }
                Err(error) => {
                    report.errors.push(format!("{}:{error}", container.name));
                    report.details[index].action = format!("reclaim-failed: {error}");
                }
            }
        }
    }
    if apply && (!reclaimed_ids.is_empty() || (!registry_was_present && report.managed == 0)) {
        // A no-managed-container apply may durably bootstrap the explicit
        // empty authority. Never infer or recreate live managed ownership from
        // immutable creation labels when the current-owner registry is absent.
        update_registry(|current| {
            for id in &reclaimed_ids {
                current.containers.remove(id);
            }
        })?;
    }
    let attention = report.reclaimable > 0
        || report.transfer_required > 0
        || report.owner_unknown > 0
        || report.unmanaged > 0
        || !report.errors.is_empty();
    report.state = if attention {
        "action-required".to_string()
    } else {
        "ok".to_string()
    };
    emit_report(&report, json_output)?;
    Ok(i32::from(attention))
}

/// Reclaim rests on PROCESS EVIDENCE ALONE. The ORC agent census is deliberately
/// not consulted.
///
/// The removed branch was `!live_agents.contains_key(owner_agent) -> Reclaimable`.
/// It was reached exactly when the process walk had already FAILED to find the
/// owner, and it then let a JSON file decide whether to destroy a container. Two
/// independent ways that file lies: it is written by ORC and can be stale or
/// degenerate (on 2026-08-08 it briefly held a single synthetic `worker` row), and
/// a headless agent has no tmux pane, so nothing about it ever appears in either
/// the census or the pane-rooted walk. Measured on this box the same day: 15
/// invocations were reachable from the 17 tmux panes while 33 were live in
/// `/proc` -- 18 live invocations, among them a running `validate.sh full`, were
/// invisible to the pane walk and would have been classified on the census alone.
///
/// `PROCESS_SCAN_KEY` present means the fleet-wide `/proc` scan COMPLETED. Its
/// absence is a NO-RESULT, not a negative, and must never authorize deletion --
/// the same distinction that `unverifiable`-rendered-as-drift got wrong elsewhere
/// in this tree.
fn classify(
    ownership: &Ownership,
    live_invocations: &BTreeMap<String, BTreeSet<String>>,
) -> Disposition {
    // Exact incarnation is alive somewhere in this user's process table.
    if live_invocations
        .values()
        .any(|invocations| invocations.contains(&ownership.owner_invocation))
    {
        return Disposition::Live;
    }
    // We could not look. Refuse.
    if !live_invocations.contains_key(PROCESS_SCAN_KEY) {
        return Disposition::OwnerUnknown;
    }
    // The scan completed and the owning incarnation is absent from it, so the
    // exact process that created this container is provably gone.
    if ownership.lifetime == Lifetime::Agent {
        Disposition::Reclaimable
    } else {
        Disposition::TransferRequired
    }
}

fn effective_ownership(container: &Container, registry: &Registry) -> Option<Ownership> {
    if container.labels.get(MANAGED_LABEL).map(String::as_str) != Some(MANAGED_VERSION) {
        return None;
    }
    if let Some(ownership) = registry.containers.get(&container.id) {
        return Some(ownership.clone());
    }
    Some(Ownership {
        container_id: container.id.clone(),
        owner_agent: container.labels.get(AGENT_LABEL)?.clone(),
        owner_invocation: container.labels.get(INVOCATION_LABEL)?.clone(),
        owner_pane: container
            .labels
            .get(PANE_LABEL)
            .cloned()
            .unwrap_or_default(),
        task: container.labels.get(TASK_LABEL)?.clone(),
        lifetime: Lifetime::parse(container.labels.get(LIFETIME_LABEL)?).ok()?,
        updated_at: 0,
    })
}

fn normalized_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn verify_release_fence(fence_token: &str) -> Result<()> {
    let path = lifecycle_lock_path()?;
    let recorded = fs::read_to_string(&path).map_err(|source| AppError::Io {
        context: format!("read release lifecycle token {}", path.display()),
        source,
    })?;
    if recorded.trim() != fence_token {
        return Err(AppError::Message(
            "release lifecycle token does not match the held fence".to_string(),
        ));
    }
    let probe = open_lifecycle_lock_file()?;
    // A second shared acquisition succeeds when the alleged holder has only a
    // shared lock, but blocks when another process owns the exclusive release
    // fence. This distinguishes the exact lock mode instead of treating any
    // unrelated lifecycle user as release authority.
    match FileExt::try_lock_shared(&probe) {
        Ok(()) => {
            let _ = FileExt::unlock(&probe);
            Err(AppError::Message(
                "release-audit requires an independently held exclusive lifecycle fence"
                    .to_string(),
            ))
        }
        Err(source) if source.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Err(source) => Err(AppError::Io {
            context: format!("probe release lifecycle fence {}", path.display()),
            source,
        }),
    }
}

fn inspect_release_container(container: &Container) -> Result<ReleaseInspection> {
    let output = bounded_podman(&["container", "inspect", &container.id], QUERY_TIMEOUT_SECS)?;
    if !output.status.success() {
        return Err(AppError::Message(format!(
            "podman inspect failed for {}: {}",
            container.id,
            podman_failure_reason(&output, QUERY_TIMEOUT_SECS)
        )));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|source| AppError::Json {
        context: format!("parse podman inspect JSON for {}", container.id),
        source,
    })?;
    let rows = value.as_array().ok_or_else(|| {
        AppError::Message(format!(
            "podman inspect JSON for {} is not an array",
            container.id
        ))
    })?;
    if rows.len() != 1 {
        return Err(AppError::Message(format!(
            "podman inspect returned {} rows for {}",
            rows.len(),
            container.id
        )));
    }
    let row = rows[0].as_object().ok_or_else(|| {
        AppError::Message(format!(
            "podman inspect row for {} is not an object",
            container.id
        ))
    })?;
    let inspected_id = string_field(row, &["Id", "ID"]).ok_or_else(|| {
        AppError::Message(format!("podman inspect row for {} has no ID", container.id))
    })?;
    if inspected_id != container.id {
        return Err(AppError::Message(format!(
            "podman inspect identity changed: requested {}, received {inspected_id}",
            container.id
        )));
    }
    let config = row
        .get("Config")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            AppError::Message(format!(
                "podman inspect row for {} has no Config object",
                container.id
            ))
        })?;
    let labels = match config.get("Labels") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(Value::Object(labels)) => labels
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_string()))
                    .ok_or_else(|| {
                        AppError::Message(format!(
                            "podman inspect label {key} for {} is not a string",
                            container.id
                        ))
                    })
            })
            .collect::<Result<BTreeMap<_, _>>>()?,
        Some(_) => {
            return Err(AppError::Message(format!(
                "podman inspect labels for {} are not an object",
                container.id
            )))
        }
    };
    let mounts = row.get("Mounts").and_then(Value::as_array).ok_or_else(|| {
        AppError::Message(format!(
            "podman inspect row for {} has no Mounts array",
            container.id
        ))
    })?;
    let mut sources = Vec::new();
    let mut mount_sources = Vec::new();
    for (index, mount) in mounts.iter().enumerate() {
        let object = mount.as_object().ok_or_else(|| {
            AppError::Message(format!(
                "podman inspect mount {index} for {} is not an object",
                container.id
            ))
        })?;
        let source = object
            .get("Source")
            .ok_or_else(|| {
                AppError::Message(format!(
                    "podman inspect mount {index} for {} has no Source field",
                    container.id
                ))
            })?
            .as_str()
            .ok_or_else(|| {
                AppError::Message(format!(
                    "podman inspect mount {index} Source for {} is not a string",
                    container.id
                ))
            })?;
        mount_sources.push(source.to_string());
        if source.is_empty() {
            continue;
        }
        let source = PathBuf::from(source);
        if !normalized_absolute_path(&source) {
            return Err(AppError::Message(format!(
                "podman inspect mount {index} for {} has non-normalized source {}",
                container.id,
                source.display()
            )));
        }
        // Retain both Podman's lexical source and the resolved filesystem
        // identity. Resolution is authority, not enrichment: after a path
        // fence, a dangling alias could otherwise erase the only observable
        // binding between a live mount and the moved worktree.
        let canonical = fs::canonicalize(&source).map_err(|error| {
            AppError::Message(format!(
                "podman inspect mount {index} source {} for {} cannot be resolved: {error}",
                source.display(),
                container.id
            ))
        })?;
        if !normalized_absolute_path(&canonical) {
            return Err(AppError::Message(format!(
                "podman inspect mount {index} for {} resolves outside normalized absolute path syntax: {}",
                container.id,
                canonical.display()
            )));
        }
        sources.push(source);
        sources.push(canonical);
    }
    sources.sort();
    sources.dedup();
    Ok(ReleaseInspection {
        labels,
        mount_sources,
        mounts: sources,
    })
}

fn release_audit(
    fence_token: &str,
    owners: &BTreeSet<String>,
    targets: &[PathBuf],
    json_output: bool,
) -> Result<i32> {
    verify_release_fence(fence_token)?;
    // The lifecycle fence excludes every supported create/transfer/reconcile
    // mutation.  Take the narrower registry lock as well so the ownership map
    // and the engine census form one observation.
    let _registry_lock = open_lock()?;
    let registry_path = state_path()?;
    let metadata = fs::symlink_metadata(&registry_path).map_err(|source| AppError::Io {
        context: format!(
            "release ownership registry is unavailable at {}",
            registry_path.display()
        ),
        source,
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::Message(format!(
            "release ownership registry is not an exact file: {}",
            registry_path.display()
        )));
    }
    let mut registry = read_registry_unlocked(&registry_path)?;
    if registry.schema_version != 1 {
        return Err(AppError::Message(format!(
            "release ownership registry has unsupported schema {}",
            registry.schema_version
        )));
    }
    for (id, ownership) in &registry.containers {
        if ownership.container_id != *id {
            return Err(AppError::Message(format!(
                "container registry key {id} does not bind payload ID {}",
                ownership.container_id
            )));
        }
    }
    let containers = list_containers()?;
    let initial_ids: BTreeSet<String> = containers
        .iter()
        .map(|container| container.id.clone())
        .collect();
    if initial_ids.len() != containers.len() {
        return Err(AppError::Message(
            "podman ps returned duplicate container IDs".to_string(),
        ));
    }
    let registry_ids: BTreeSet<String> = registry.containers.keys().cloned().collect();
    let missing_from_census: Vec<String> = registry_ids.difference(&initial_ids).cloned().collect();
    let mut stale_registry_ids = Vec::new();
    for id in &missing_from_census {
        match container_exists_authoritative(id) {
            Ok(false) => stale_registry_ids.push(id.clone()),
            Ok(true) => {
                return Err(AppError::Message(format!(
                    "release ownership registry container {id} exists but is absent from the exact engine census"
                )))
            }
            Err(error) => {
                return Err(AppError::Message(format!(
                    "release ownership registry container {id} has uncertain engine existence: {error}"
                )))
            }
        }
    }
    for id in &stale_registry_ids {
        registry.containers.remove(id);
    }
    if !stale_registry_ids.is_empty() {
        write_registry_unlocked(&registry_path, &registry)?;
    }

    let mut details = Vec::new();
    let mut container_observation = Vec::new();
    let mut owner_matches = 0usize;
    let mut target_mounts = 0usize;
    for container in &containers {
        let registry_ownership = registry.containers.get(&container.id);
        let registry_owner = registry_ownership.map(|ownership| ownership.owner_agent.clone());
        let inspection = inspect_release_container(container)?;
        let managed = inspection.labels.get(MANAGED_LABEL).map(String::as_str);
        let label_owner = inspection.labels.get(AGENT_LABEL).cloned();
        if registry_owner.is_some() && managed != Some(MANAGED_VERSION) {
            return Err(AppError::Message(format!(
                "live container {} has registry ownership but no matching managed label",
                container.id
            )));
        }
        if managed == Some(MANAGED_VERSION) && label_owner.is_none() {
            return Err(AppError::Message(format!(
                "managed container {} has no owner-agent label",
                container.id
            )));
        }
        if managed == Some(MANAGED_VERSION) && registry_ownership.is_none() {
            return Err(AppError::Message(format!(
                "managed live container {} ({}) has no durable current-owner registry entry",
                container.id, container.name
            )));
        }
        if registry_owner.as_ref().is_some_and(String::is_empty) {
            return Err(AppError::Message(format!(
                "container {} has an empty registry owner",
                container.id
            )));
        }
        // `transfer` intentionally advances current ownership in the durable
        // registry without rewriting immutable creation labels. The registry
        // is therefore the current-owner authority for every managed live
        // container; labels remain a conservative fallback only for unmanaged
        // containers that happen to carry an owner label.
        let owner = registry_owner.or(label_owner);
        let matching_mounts: Vec<String> = inspection
            .mounts
            .iter()
            .filter(|source| targets.iter().any(|target| paths_overlap(source, target)))
            .map(|source| source.display().to_string())
            .collect();
        let mut reasons = Vec::new();
        if owner.as_ref().is_some_and(|owner| owners.contains(owner)) {
            owner_matches += 1;
            reasons.push("recorded-owner-container".to_string());
        }
        if !matching_mounts.is_empty() {
            target_mounts += 1;
            reasons.push("target-overlapping-mount".to_string());
        }
        if !reasons.is_empty() {
            details.push(ReleaseAuditDetail {
                id: container.id.clone(),
                name: container.name.clone(),
                owner_agent: owner,
                matching_mounts,
                reasons,
            });
        }
        container_observation.push(ReleaseContainerObservation {
            id: container.id.clone(),
            name: container.name.clone(),
            state: container.state.clone(),
            ps_labels: container.labels.clone(),
            inspect_labels: inspection.labels,
            registry_ownership: registry_ownership.cloned(),
            mount_sources: inspection.mount_sources,
            resolved_mounts: inspection
                .mounts
                .into_iter()
                .map(|path| path.display().to_string())
                .collect(),
        });
    }
    container_observation.sort_by(|left, right| left.id.cmp(&right.id));

    // Completeness bracket: a disappearing or newly appearing container makes
    // this observation unusable rather than silently shrinking its coverage.
    let final_containers = list_containers()?;
    let final_ids: BTreeSet<String> = final_containers
        .iter()
        .map(|container| container.id.clone())
        .collect();
    if final_ids.len() != final_containers.len() || final_ids != initial_ids {
        return Err(AppError::Message(
            "podman container census changed during release-audit".to_string(),
        ));
    }

    let report = ReleaseAuditReport {
        state: if details.is_empty() {
            "ok".to_string()
        } else {
            "refused".to_string()
        },
        inspected: containers.len(),
        stale_registry_entries_removed: stale_registry_ids.len(),
        owner_matches,
        target_mounts,
        details,
        container_observation,
    };
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|source| AppError::Json {
                context: "serialize release audit".to_string(),
                source,
            })?
        );
    } else {
        println!(
            "state={} inspected={} stale_registry_entries_removed={} owner_matches={} target_mounts={}",
            report.state,
            report.inspected,
            report.stale_registry_entries_removed,
            report.owner_matches,
            report.target_mounts
        );
        for detail in &report.details {
            println!(
                "container={} name={} owner={} reasons={} mounts={}",
                detail.id,
                detail.name,
                detail.owner_agent.as_deref().unwrap_or("-"),
                detail.reasons.join(","),
                detail.matching_mounts.join(",")
            );
        }
    }
    Ok(i32::from(report.state != "ok"))
}

fn load_live_agents(path: &Path) -> Result<BTreeMap<String, String>> {
    let raw = fs::read(path).map_err(|source| AppError::Io {
        context: format!("read agent snapshot {}", path.display()),
        source,
    })?;
    let snapshot = agent_snapshot::AgentSnapshot::parse(&raw).map_err(AppError::Message)?;
    // ⚠️ A FILE THAT EXISTS AND PARSES READS AS FRESH, AND THAT IS THE WHOLE
    // HAZARD HERE. The envelope form carries `captured_at` and was age-checked;
    // the BARE-ARRAY form set `captured_at = None` and the check below was
    // `if let Some(..)`, so a snapshot with no timestamp skipped staleness
    // ENTIRELY and was treated as ageless. This function decides which agents are
    // absent, and `reconcile --apply` gracefully_remove()s a container whose owner
    // looks absent -- so an undated snapshot could authorise destroying a LIVE
    // agent's container, and headless agents have no tmux pane to rescue them.
    //
    // An undated snapshot is now REFUSED rather than trusted. That is the
    // fail-closed direction: refusing costs a red probe, trusting costs a
    // container. The bare-array form is not otherwise supported and never had a
    // producer -- `_persist_agent_snapshot` in
    // `ci-hub/health/operational_health.py` always writes the envelope.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|source| AppError::Message(format!("read system clock: {source}")))?
        .as_secs_f64();
    snapshot
        .ensure_fresh(now, SNAPSHOT_MAX_AGE_SECS as f64)
        .map_err(AppError::Message)?;
    let mut live = BTreeMap::new();
    for agent in &snapshot.agents {
        if agent.status.is_live() {
            live.insert(
                agent.name.clone(),
                agent.tmux_pane_id.clone().unwrap_or_default(),
            );
        }
    }
    // ⚠️ EMPTY IS NOT STALE, AND THE STALENESS GUARD ABOVE DOES NOT COVER IT.
    //
    // A census with a CURRENT `captured_at` and no live agents in it passes every
    // check above and then means, to the reclaim predicate, "no agent owns
    // anything" -- which authorises removing EVERY managed container. That is
    // absence of evidence read as evidence of absence, and it is the one shape the
    // dated-snapshot guard cannot catch, because the snapshot is not stale.
    //
    // Measured 2026-08-26: `{"schema_version":1,"captured_at":<now>,"agents":[]}`
    // returned rc=0 `state=ok` "all 0 managed container(s) have live owners" --
    // and it returned ok ONLY because this box happened to have zero managed
    // containers that minute. With containers present the same input nominates all
    // of them. Nothing currently produces an empty census, which is luck and not
    // design: the producer is scheduled by nothing, and the day it is wired a
    // failed fetch that yields `agents: []` becomes a fleet-wide reclaim.
    //
    // Same direction and same cost as the staleness guard it sits beside:
    // refusing costs a red probe, trusting costs every container. Deliberately
    // NOT overridable, because the staleness guard is not either, and two adjacent
    // guards with different escape hatches is how the next reader picks the wrong
    // one.
    //
    // ⚠️ IT KEYS ON THE DERIVED LIVE SET, NOT ON THE RAW ARRAY LENGTH. A snapshot
    // listing thirty agents that are ALL terminal reduces to the same empty live
    // set and authorises the same reclaim, so checking `array.is_empty()` would
    // leave that half open.
    if live.is_empty() {
        return Err(AppError::Message(format!(
            "agent snapshot lists no live agents ({} entr{} present, all unnamed or \
             terminal): refusing rather than reading an empty census as \"every owner \
             is gone\" -- this decides which agents are absent, and reconcile --apply \
             removes their containers",
            snapshot.agents.len(),
            if snapshot.agents.len() == 1 {
                "y"
            } else {
                "ies"
            }
        )));
    }
    Ok(live)
}

fn load_live_invocations(path: &Path) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let raw = fs::read_to_string(path).map_err(|source| AppError::Io {
        context: format!("read live invocations {}", path.display()),
        source,
    })?;
    serde_json::from_str(&raw).map_err(|source| AppError::Json {
        context: format!("parse live invocations {}", path.display()),
        source,
    })
}

fn discover_live_invocations(
    agents: &BTreeMap<String, String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let pane_pids = tmux_pane_pids();
    let process_tree = process_tree();
    let mut discovered: BTreeMap<String, BTreeSet<String>> = agents
        .iter()
        .map(|(name, pane)| {
            let invocations = pane_pids
                .get(pane)
                .map(|pid| descendant_invocations(*pid, &process_tree))
                .unwrap_or_default();
            (name.clone(), invocations)
        })
        .collect();
    // Also scan panes not present in the ORC snapshot. During recycling there
    // can be a short interval where listAgents omits an invocation whose process
    // still exists; that interval must never authorize deletion.
    for (pane, pid) in pane_pids {
        discovered
            .entry(format!("@pane:{pane}"))
            .or_insert_with(|| descendant_invocations(pid, &process_tree));
    }
    // Fleet-wide sweep. Everything above is rooted at a tmux pane and therefore
    // cannot see a headless agent, which has no pane by construction. `classify`
    // treats the presence of this key as proof that the scan ran, so it is
    // inserted ONLY on success.
    if let Some(all) = all_process_invocations() {
        discovered.insert(PROCESS_SCAN_KEY.to_string(), all);
    }
    discovered
}

/// Every invocation id in this user's process table, or `None` if `/proc` could
/// not be enumerated at all. Same shape as `residue_sweep.py`'s `process_cwds`:
/// walk `/proc`, keep same-uid entries, and raise rather than silently return an
/// empty set -- an unreadable `/proc` must read as "we do not know", never as
/// "nobody is alive".
fn all_process_invocations() -> Option<BTreeSet<String>> {
    use std::os::unix::fs::MetadataExt;
    let entries = fs::read_dir("/proc").ok()?;
    // Our own uid without pulling in a libc dependency for one call.
    let uid = fs::metadata("/proc/self").ok()?.uid();
    let mut invocations = BTreeSet::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // Only same-uid processes are readable, and only ours are ours to judge.
        match fs::metadata(entry.path()) {
            Ok(meta) if meta.uid() == uid => {}
            _ => continue,
        }
        let Ok(raw) = fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        for field in raw.split(|byte| *byte == 0) {
            let text = String::from_utf8_lossy(field);
            if let Some(value) = text.strip_prefix("META_3PAI_INVOCATION_ID=") {
                if !value.is_empty() {
                    invocations.insert(value.to_string());
                }
            } else if let Some(metadata) = text.strip_prefix("CODING_AGENT_METADATA=") {
                for item in metadata.split(',') {
                    if let Some(value) = item.trim().strip_prefix("invocation_id=") {
                        if !value.is_empty() {
                            invocations.insert(value.to_string());
                        }
                    }
                }
            }
        }
    }
    Some(invocations)
}

fn tmux_pane_pids() -> BTreeMap<String, u32> {
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}\t#{pane_pid}"])
        .output();
    let Ok(output) = output else {
        return BTreeMap::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (pane, pid) = line.split_once('\t')?;
            Some((pane.to_string(), pid.parse().ok()?))
        })
        .collect()
}

fn process_tree() -> BTreeMap<u32, Vec<u32>> {
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return children;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(after_name) = stat.rsplit_once(") ").map(|(_, rest)| rest) else {
            continue;
        };
        let Some(ppid) = after_name
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        children.entry(ppid).or_default().push(pid);
    }
    children
}

fn descendant_invocations(root: u32, tree: &BTreeMap<u32, Vec<u32>>) -> BTreeSet<String> {
    let mut pending = vec![root];
    let mut seen = BTreeSet::new();
    let mut invocations = BTreeSet::new();
    while let Some(pid) = pending.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Ok(raw) = fs::read(format!("/proc/{pid}/environ")) {
            for field in raw.split(|byte| *byte == 0) {
                let text = String::from_utf8_lossy(field);
                if let Some(value) = text.strip_prefix("META_3PAI_INVOCATION_ID=") {
                    if !value.is_empty() {
                        invocations.insert(value.to_string());
                    }
                } else if let Some(metadata) = text.strip_prefix("CODING_AGENT_METADATA=") {
                    for item in metadata.split(',') {
                        if let Some(value) = item.trim().strip_prefix("invocation_id=") {
                            if !value.is_empty() {
                                invocations.insert(value.to_string());
                            }
                        }
                    }
                }
            }
        }
        pending.extend(tree.get(&pid).into_iter().flatten().copied());
    }
    invocations
}

fn list_containers() -> Result<Vec<Container>> {
    let output = bounded_podman(&["ps", "-a", "--format", "json"], QUERY_TIMEOUT_SECS)?;
    if !output.status.success() {
        return Err(AppError::Message(format!(
            "podman ps failed: {}",
            podman_failure_reason(&output, QUERY_TIMEOUT_SECS)
        )));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|source| AppError::Json {
        context: "parse podman ps JSON".to_string(),
        source,
    })?;
    let entries = value
        .as_array()
        .ok_or_else(|| AppError::Message("podman ps JSON is not an array".to_string()))?;
    entries.iter().map(parse_container).collect()
}

fn parse_container(value: &Value) -> Result<Container> {
    let object = value
        .as_object()
        .ok_or_else(|| AppError::Message("podman container row is not an object".to_string()))?;
    let id = string_field(object, &["Id", "ID"])
        .ok_or_else(|| AppError::Message("podman container row has no ID".to_string()))?;
    let name = match object.get("Names") {
        Some(Value::Array(names)) => names
            .first()
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_string(),
        Some(Value::String(name)) => name.clone(),
        _ => id.clone(),
    };
    let state = string_field(object, &["State", "Status"]).unwrap_or_else(|| "unknown".to_string());
    let labels = object
        .get("Labels")
        .and_then(Value::as_object)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Ok(Container {
        id,
        name,
        state: state.to_ascii_lowercase(),
        labels,
    })
}

fn string_field(object: &Map<String, Value>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        object
            .get(*name)
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn container_process_evidence(container: &Container) -> (usize, Option<String>) {
    if container.state != "running" {
        return (0, None);
    }
    let zombies = bounded_podman(&["top", &container.id, "hpid,state"], EVIDENCE_TIMEOUT_SECS)
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .skip(1)
                .filter(|line| {
                    line.split_whitespace()
                        .nth(1)
                        .is_some_and(|state| state.starts_with('Z'))
                })
                .count()
        })
        .unwrap_or(0);
    let namespace = bounded_podman(
        &["inspect", "--format", "{{.State.Pid}}", &container.id],
        EVIDENCE_TIMEOUT_SECS,
    )
    .ok()
    .filter(|output| output.status.success())
    .and_then(|output| String::from_utf8(output.stdout).ok())
    .and_then(|pid| fs::read_link(format!("/proc/{}/ns/pid", pid.trim())).ok())
    .map(|path| path.to_string_lossy().to_string());
    (zombies, namespace)
}

fn gracefully_remove(container: &Container) -> Result<()> {
    let mut stop_error = None;
    if container.state == "running" {
        let stop = bounded_podman(&["stop", "--time", "10", &container.id], STOP_TIMEOUT_SECS)?;
        if !stop.status.success() {
            stop_error = Some(format!(
                "podman stop failed: {}",
                podman_failure_reason(&stop, STOP_TIMEOUT_SECS)
            ));
        }
    }
    let remove = bounded_podman(&["rm", &container.id], REMOVE_TIMEOUT_SECS)?;
    if !remove.status.success() {
        let stop_context = stop_error
            .map(|error| format!("{error}; "))
            .unwrap_or_default();
        return Err(AppError::Message(format!(
            "{stop_context}podman rm failed: {}",
            podman_failure_reason(&remove, REMOVE_TIMEOUT_SECS)
        )));
    }
    Ok(())
}

fn inspect_labels(container: &str) -> Result<BTreeMap<String, String>> {
    let output = bounded_podman(
        &["inspect", "--format", "{{json .Config.Labels}}", container],
        QUERY_TIMEOUT_SECS,
    )?;
    if !output.status.success() {
        return Err(AppError::Message(format!(
            "podman inspect failed for {container}: {}",
            podman_failure_reason(&output, QUERY_TIMEOUT_SECS)
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|source| AppError::Json {
        context: format!("parse labels for {container}"),
        source,
    })
}

fn inspect_id(container: &str) -> Result<String> {
    let output = bounded_podman(
        &["inspect", "--format", "{{.Id}}", container],
        QUERY_TIMEOUT_SECS,
    )?;
    if !output.status.success() {
        return Err(AppError::Message(format!(
            "podman inspect failed for {container}: {}",
            podman_failure_reason(&output, QUERY_TIMEOUT_SECS)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn container_exists_authoritative(container: &str) -> Result<bool> {
    let output = bounded_podman(&["container", "exists", container], QUERY_TIMEOUT_SECS)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        code => Err(AppError::Message(format!(
            "podman container exists was inconclusive for {container} (exit {}): {}",
            code.map_or_else(|| "signal".to_string(), |code| code.to_string()),
            podman_failure_reason(&output, QUERY_TIMEOUT_SECS)
        ))),
    }
}

fn container_release_visible(container: &str) -> Result<bool> {
    if !container_exists_authoritative(container)? {
        return Ok(false);
    }
    let containers = list_containers()?;
    let matches = containers
        .iter()
        .filter(|candidate| candidate.id == container)
        .count();
    if matches > 1 {
        return Err(AppError::Message(format!(
            "podman census duplicated newly registered container {container}"
        )));
    }
    if matches == 1 {
        let candidate = containers
            .iter()
            .find(|candidate| candidate.id == container)
            .expect("exactly one matching census row");
        inspect_release_container(candidate)?;
        return Ok(true);
    }
    Ok(false)
}

fn wait_for_release_visibility(container: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let last_observation = match container_release_visible(container) {
            Ok(true) => return Ok(()),
            Ok(false) => "container absent from exact engine census".to_string(),
            Err(error) => error.to_string(),
        };
        if Instant::now() >= deadline {
            return Err(AppError::Message(format!(
                "new container {container} was registered but not release-visible within {}s; ownership registry retained: {last_observation}",
                timeout.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Why a bounded podman call failed, naming the BOUND when the bound is the reason.
///
/// ⚠️ A CHILD KILLED BY ITS BOUND REPORTS NOTHING, AND THAT IS WHY THIS EXISTS.
/// `bounded_podman` runs the child under `timeout --signal=TERM --kill-after=2s`,
/// so an over-running child is TERMed before it writes a word and `timeout` exits
/// 124 (137 when it had to escalate to KILL). Every caller here printed only
/// stderr, so the operator saw
///
///     agent-podman: podman ps failed:
///
/// with nothing after the colon -- identical in shape to a podman that crashed
/// silently. Reproduced deterministically 2026-08-27 by pointing AGENT_PODMAN_BIN
/// at a script that merely sleeps: nothing was wrong with it and the report was
/// indistinguishable from a fault.
///
/// That matters beyond tidiness. `ci-hub/tests/test_agent_podman.py` is
/// intermittently red under load, and the FIRST question about any such failure is
/// whether the call was killed by its bound or failed on its merits. Those have
/// opposite fixes, and the output could not tell them apart, so the question could
/// not be asked of the evidence that already existed.
fn podman_failure_reason(output: &std::process::Output, bound: &str) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match output.status.code() {
        // `timeout` reserves both: 124 after the TERM, 137 when --kill-after fired.
        Some(code @ (124 | 137)) => {
            let escalated = if code == 137 {
                "; did not exit on TERM"
            } else {
                ""
            };
            let tail = if stderr.is_empty() {
                String::new()
            } else {
                format!("; it also wrote: {stderr}")
            };
            format!("KILLED BY ITS {bound} BOUND (timeout exit {code}{escalated}), not a podman fault{tail}")
        }
        Some(code) if stderr.is_empty() => format!("exit {code}, and it wrote nothing"),
        None if stderr.is_empty() => "killed by a signal, and it wrote nothing".to_string(),
        _ => stderr,
    }
}

fn bounded_podman(arguments: &[&str], timeout_secs: &str) -> Result<std::process::Output> {
    Command::new("timeout")
        .args([
            "--signal=TERM",
            "--kill-after=2s",
            timeout_secs,
            &podman_bin(),
        ])
        .args(arguments)
        .output()
        .map_err(|source| AppError::Io {
            context: format!("run bounded podman {}", arguments.join(" ")),
            source,
        })
}

fn podman_bin() -> String {
    env::var("AGENT_PODMAN_BIN").unwrap_or_else(|_| "podman".to_string())
}

fn state_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("DEV_HERMIT_CONTAINER_STATE") {
        return Ok(PathBuf::from(path));
    }
    if let Some(root) = env::var_os("DEV_HERMIT_PARENT") {
        return Ok(PathBuf::from(root).join("ignored/ci-hub/agent-containers.json"));
    }
    let mut candidate = env::current_dir().map_err(|source| AppError::Io {
        context: "read current directory".to_string(),
        source,
    })?;
    for _ in 0..=4 {
        if candidate.join("ci-hub").is_dir() && candidate.join("scripts/agent-podman.rs").is_file()
        {
            return Ok(candidate.join("ignored/ci-hub/agent-containers.json"));
        }
        if !candidate.pop() {
            break;
        }
    }
    let source = PathBuf::from(file!());
    if let Some(root) = source.parent().and_then(Path::parent) {
        if root.join("ci-hub").is_dir() {
            return Ok(root.join("ignored/ci-hub/agent-containers.json"));
        }
    }
    Err(AppError::Message(
        "cannot locate dev-hermit within four parents; set DEV_HERMIT_PARENT".to_string(),
    ))
}

fn lifecycle_lock_path() -> Result<PathBuf> {
    let state = state_path()?;
    let parent = state
        .parent()
        .ok_or_else(|| AppError::Message("ownership state has no parent".to_string()))?;
    Ok(parent.join("agent-container-lifecycle.lock"))
}

fn open_lifecycle_lock_file() -> Result<File> {
    let path = lifecycle_lock_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| AppError::Io {
            context: format!("create {}", parent.display()),
            source,
        })?;
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|source| AppError::Io {
            context: format!("open {}", path.display()),
            source,
        })
}

fn open_lifecycle_lock_shared() -> Result<File> {
    let file = open_lifecycle_lock_file()?;
    FileExt::lock_shared(&file).map_err(|source| AppError::Io {
        context: "lock container lifecycle shared".to_string(),
        source,
    })?;
    Ok(file)
}

fn open_lifecycle_lock_exclusive() -> Result<File> {
    let file = open_lifecycle_lock_file()?;
    FileExt::lock_exclusive(&file).map_err(|source| AppError::Io {
        context: "lock container lifecycle exclusive".to_string(),
        source,
    })?;
    Ok(file)
}

fn lock_path() -> Result<PathBuf> {
    let state = state_path()?;
    Ok(state.with_extension("lock"))
}

fn open_lock() -> Result<File> {
    let path = lock_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| AppError::Io {
            context: format!("create {}", parent.display()),
            source,
        })?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|source| AppError::Io {
            context: format!("open {}", path.display()),
            source,
        })?;
    file.lock_exclusive().map_err(|source| AppError::Io {
        context: format!("lock {}", path.display()),
        source,
    })?;
    Ok(file)
}

fn read_registry_unlocked(path: &Path) -> Result<Registry> {
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|source| AppError::Json {
            context: format!("parse {}", path.display()),
            source,
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(source) => Err(AppError::Io {
            context: format!("read {}", path.display()),
            source,
        }),
    }
}

fn update_registry(action: impl FnOnce(&mut Registry)) -> Result<()> {
    let _lock = open_lock()?;
    let path = state_path()?;
    let mut registry = read_registry_unlocked(&path)?;
    action(&mut registry);
    write_registry_unlocked(&path, &registry)
}

fn durable_registry_replace(
    path: &Path,
    bytes: &[u8],
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "registry has no parent"))?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .as_nanos();
    let temporary = parent.join(format!(
        ".agent-containers.{}.{nonce}.tmp",
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        before_rename()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_registry_unlocked(path: &Path, registry: &Registry) -> Result<()> {
    let text = serde_json::to_string_pretty(registry).map_err(|source| AppError::Json {
        context: "serialize ownership registry".to_string(),
        source,
    })?;
    durable_registry_replace(path, format!("{text}\n").as_bytes(), || Ok(())).map_err(|source| {
        AppError::Io {
            context: format!("durably replace {}", path.display()),
            source,
        }
    })
}

fn emit_report(report: &ReconcileReport, json_output: bool) -> Result<()> {
    if json_output {
        let text = serde_json::to_string_pretty(report).map_err(|source| AppError::Json {
            context: "serialize reconciliation report".to_string(),
            source,
        })?;
        println!("{text}");
        return Ok(());
    }
    // ONE `key=value` PAIR PER LINE. tick-hub's `capture: true` parses stdout with
    // parse_kv_lines, which is strictly line-oriented: emitted as a single
    // space-separated line, all ten fields collapsed into ONE key whose value was
    // the rest of the line, `summary` was never captured, and the action rendered
    // a LITERAL `{summary}` naming nothing. Measured 2026-08-08 -- a producer can
    // emit a perfectly good summary and still lose it to line discipline.
    let summary = if report.state == "ok" {
        format!(
            "all {} managed container(s) have live owners; nothing to reclaim",
            report.managed
        )
    } else {
        format!(
            "container lifecycle needs action: {} reclaimable, {} transfer-required, \
             {} owner-unknown, {} unmanaged, {} zombie(s) in {} container(s) \
             (REPORT-ONLY: reclaim with `scripts/agent-podman.rs reconcile \
             --agents-json ignored/ci-hub/agent-snapshot.json --apply`)",
            report.reclaimable,
            report.transfer_required,
            report.owner_unknown,
            report.unmanaged,
            report.zombies,
            report.zombie_containers
        )
    };
    println!("state={}", report.state);
    println!("summary={summary}");
    println!("managed={}", report.managed);
    println!("live={}", report.live);
    println!("reclaimed={}", report.reclaimed);
    println!("reclaimable={}", report.reclaimable);
    println!("transfer_required={}", report.transfer_required);
    println!("owner_unknown={}", report.owner_unknown);
    println!("unmanaged={}", report.unmanaged);
    println!("zombie_containers={}", report.zombie_containers);
    println!("zombies={}", report.zombies);
    for detail in &report.details {
        println!(
            "DETAIL: name={} id={} disposition={:?} owner={} invocation={} task={} lifetime={} namespace={} zombies={} action={}",
            detail.name,
            short_id(&detail.id),
            detail.disposition,
            detail.owner_agent.as_deref().unwrap_or("unknown"),
            detail.owner_invocation.as_deref().unwrap_or("unknown"),
            detail.task.as_deref().unwrap_or("unknown"),
            detail.lifetime.map(Lifetime::as_str).unwrap_or("unknown"),
            detail.namespace.as_deref().unwrap_or("none"),
            detail.zombies,
            detail.action
        );
    }
    for error in &report.errors {
        println!("ERROR: container-lifecycle {error}");
    }
    Ok(())
}

fn emit_reconcile_error(error: &AppError) {
    // Captured values are line-oriented and are interpolated before tick-hub
    // checks for unresolved placeholders. Collapse arbitrary I/O context to one
    // line and neutralize braces so an error path cannot manufacture a second
    // template placeholder.
    let summary = format!("container lifecycle check could not complete: {error}")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('{', "(")
        .replace('}', ")");
    println!("state=error");
    println!("summary={summary}");
}

fn short_id(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(unix)]
fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}

#[cfg(not(unix))]
fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ownership(lifetime: Lifetime) -> Ownership {
        Ownership {
            container_id: "abc".to_string(),
            owner_agent: "hermit-1".to_string(),
            owner_invocation: "inv-old".to_string(),
            owner_pane: "%1".to_string(),
            task: "task-1".to_string(),
            lifetime,
            updated_at: 1,
        }
    }

    /// A COMPLETED fleet-wide scan that observed `found`. The distinction between
    /// this and `BTreeMap::new()` is the whole point: one says "we looked and the
    /// owner is gone", the other says "we did not look".
    fn completed_scan(found: &[&str]) -> BTreeMap<String, BTreeSet<String>> {
        BTreeMap::from([(
            PROCESS_SCAN_KEY.to_string(),
            found.iter().map(|s| (*s).to_string()).collect(),
        )])
    }

    // ---- POSITIVE SIDE: reclamation still fires, or we have lost the capability ----

    #[test]
    fn retired_agent_container_is_reclaimable() {
        assert_eq!(
            classify(&ownership(Lifetime::Agent), &completed_scan(&["inv-other"])),
            Disposition::Reclaimable
        );
    }

    #[test]
    fn task_container_requires_transfer_when_creator_is_gone() {
        assert_eq!(
            classify(&ownership(Lifetime::Task), &completed_scan(&[])),
            Disposition::TransferRequired
        );
    }

    #[test]
    fn exact_invocation_not_reused_agent_name_defines_liveness() {
        assert_eq!(
            classify(&ownership(Lifetime::Agent), &completed_scan(&["inv-old"])),
            Disposition::Live
        );
        // Same agent NAME, new incarnation: the old container is still garbage.
        assert_eq!(
            classify(&ownership(Lifetime::Agent), &completed_scan(&["inv-new"])),
            Disposition::Reclaimable
        );
    }

    // ---- NEGATIVE SIDE: no live owner is ever reclaimed ----

    #[test]
    fn missing_invocation_evidence_never_authorizes_removal() {
        // No scan ran at all. Previously this returned Reclaimable whenever the
        // census also lacked the agent, which is how an empty snapshot could
        // authorize deleting a live agent's container.
        assert_eq!(
            classify(&ownership(Lifetime::Agent), &BTreeMap::new()),
            Disposition::OwnerUnknown
        );
    }

    #[test]
    fn live_process_evidence_overrides_transient_orc_absence() {
        let process_evidence = BTreeMap::from([(
            "@pane:%1".to_string(),
            BTreeSet::from(["inv-old".to_string()]),
        )]);
        assert_eq!(
            classify(&ownership(Lifetime::Agent), &process_evidence),
            Disposition::Live
        );
    }

    #[test]
    fn headless_owner_invisible_to_the_pane_walk_is_never_reclaimed() {
        // THE REGRESSION THIS RESCOPE EXISTS FOR, modelled as the OLD code would
        // have seen it: panes enumerated fine but the owner is headless so its
        // pane set is empty, and the census does not list it either. The old
        // classify fell through to `!live_agents.contains_key(..)` and returned
        // Reclaimable -- destroying a live agent's container. There is no
        // completed sweep here, so the only safe answer is "we do not know".
        let pane_only: BTreeMap<String, BTreeSet<String>> =
            BTreeMap::from([("@pane:%1".to_string(), BTreeSet::new())]);
        assert_eq!(
            classify(&ownership(Lifetime::Agent), &pane_only),
            Disposition::OwnerUnknown
        );
    }

    #[test]
    fn the_fleet_wide_sweep_rescues_an_owner_the_pane_walk_missed() {
        // Same headless owner, but now the /proc sweep ran and found it. This is
        // the evidence source the rescope adds: measured on this box 2026-08-08,
        // the pane walk saw 15 invocations and the sweep saw 33.
        let mut evidence: BTreeMap<String, BTreeSet<String>> =
            BTreeMap::from([("@pane:%1".to_string(), BTreeSet::new())]);
        evidence.insert(
            PROCESS_SCAN_KEY.to_string(),
            BTreeSet::from(["inv-old".to_string()]),
        );
        assert_eq!(
            classify(&ownership(Lifetime::Agent), &evidence),
            Disposition::Live
        );
    }

    #[test]
    fn a_degenerate_agent_census_cannot_authorize_removal() {
        // Exactly the 2026-08-08 shape: the snapshot held one synthetic row and
        // named no real agent. classify no longer takes the census at all, so the
        // only thing that decides is whether the sweep saw the invocation.
        assert_eq!(
            classify(
                &ownership(Lifetime::Agent),
                &completed_scan(&["inv-old", "unrelated-inv"])
            ),
            Disposition::Live
        );
    }

    #[test]
    fn wrapper_rejects_cidfile_override() {
        assert!(reject_reserved_arguments(&["--cidfile=/tmp/x".to_string()]).is_err());
    }

    #[test]
    fn quickstart_is_nonempty_and_consequence_led() {
        assert!(QUICKSTART.contains("never automatic deletion targets"));
        assert!(QUICKSTART.lines().count() < 20);
    }

    #[test]
    fn registry_replace_failure_preserves_previous_authority() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = env::temp_dir().join(format!(
            "agent-podman-registry-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("agent-containers.json");
        fs::write(&path, b"old-authority\n").unwrap();

        let result = durable_registry_replace(&path, b"new-authority\n", || {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "planted pre-rename failure",
            ))
        });

        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old-authority\n");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
