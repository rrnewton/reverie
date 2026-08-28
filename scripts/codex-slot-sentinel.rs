#!/usr/bin/env rust-script
//! Canonical authority for coordinator-held Codex slot sentinels.
//!
//! A sentinel is a per-slot systemd user service. It is coordination state,
//! never evidence that one Codex thread is live: Codex threads can share an OS
//! process, tmux pane, and cgroup. Allocation records the exact systemd
//! incarnation and release revokes only that incarnation before running its
//! independent global process/container scans.
//!
//! ```cargo
//! [dependencies]
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! ```

#[allow(dead_code)]
#[path = "worktree_registry.rs"]
mod worktree_registry;

use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{exit, Command, Output};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ENVIRONMENT_KEY: &str = "DEV_HERMIT_SLOT_LEASE";
const QUERY_TIMEOUT: &str = "5s";
const MUTATION_TIMEOUT: &str = "10s";

const USAGE: &str = r#"Usage:
  codex-slot-sentinel.rs plan --slot SLOT --working-directory ABSOLUTE-PATH
  codex-slot-sentinel.rs launch --plan-json JSON
  codex-slot-sentinel.rs verify --lease-json JSON
  codex-slot-sentinel.rs revoke --lease-json JSON [--recover]
  codex-slot-sentinel.rs prove-revoked --lease-json JSON

All commands print one JSON result. `revoke` stops only the exact recorded
systemd incarnation. `--recover` resumes an already-journaled revocation; it
never turns an absent pre-journal unit into release authority.
"#;

use worktree_registry::{SentinelLease, SentinelPlan};

#[derive(Debug)]
struct UnitState {
    id: String,
    load_state: String,
    active_state: String,
    sub_state: String,
    invocation_id: String,
    main_pid: u32,
    cgroup: String,
    environment: String,
    working_directory: String,
    transient: String,
    service_type: String,
    restart: String,
    kill_mode: String,
}

#[derive(Clone, Debug)]
struct EvidenceRoots {
    proc_root: PathBuf,
    cgroup_root: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
struct RevocationProof {
    observed_at_unix_ns: u128,
    unit_id: String,
    load_state: String,
    active_state: String,
    sub_state: String,
    invocation_id: String,
    recorded_main_pid: u32,
    recorded_main_pid_starttime: u64,
    observed_main_pid_starttime: Option<u64>,
    cgroup_path: String,
    cgroup_absent: bool,
    cgroup_members: Vec<u32>,
    cgroup_populated: Option<bool>,
}

fn fail(message: impl AsRef<str>) -> ! {
    eprintln!("codex-slot-sentinel: {}", message.as_ref());
    exit(1);
}

fn normalized_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn normalized_cgroup(path: &str) -> bool {
    path.starts_with('/')
        && path != "/"
        && Path::new(path.trim_start_matches('/'))
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn bounded(program: &str, arguments: &[String], timeout: &str) -> io::Result<Output> {
    Command::new("/usr/bin/timeout")
        .args(["--signal=TERM", "--kill-after=1s", timeout, program])
        .args(arguments)
        .output()
}

fn fixture_tool(variable: &str, fallback: &str) -> Result<String, String> {
    let Some(raw) = env::var_os(variable) else {
        return Ok(fallback.to_string());
    };
    let raw_root = env::var_os("HERMIT_SENTINEL_TEST_ROOT")
        .ok_or_else(|| format!("{variable} requires HERMIT_SENTINEL_TEST_ROOT"))?;
    let root = fs::canonicalize(raw_root)
        .map_err(|error| format!("canonicalize HERMIT_SENTINEL_TEST_ROOT: {error}"))?;
    if fixture_ancestor(&root).as_deref() != Some(root.as_path()) {
        return Err(format!(
            "{variable} is restricted to a disposable fixture root"
        ));
    }
    let tool =
        fs::canonicalize(raw).map_err(|error| format!("canonicalize {variable}: {error}"))?;
    if !tool.starts_with(&root) || !tool.is_file() {
        return Err(format!("{variable} must name a fixture-local exact file"));
    }
    Ok(tool.display().to_string())
}

fn systemctl(arguments: &[String]) -> Result<Output, String> {
    bounded(
        &fixture_tool("HERMIT_SENTINEL_TEST_SYSTEMCTL", "/usr/bin/systemctl")?,
        arguments,
        QUERY_TIMEOUT,
    )
    .map_err(|error| format!("systemctl unavailable: {error}"))
}

fn query_unit(unit: &str) -> Result<UnitState, String> {
    let mut arguments = vec!["--user".to_string(), "show".to_string(), unit.to_string()];
    for property in [
        "Id",
        "LoadState",
        "ActiveState",
        "SubState",
        "InvocationID",
        "MainPID",
        "ControlGroup",
        "Environment",
        "WorkingDirectory",
        "Transient",
        "Type",
        "Restart",
        "KillMode",
    ] {
        arguments.push(format!("--property={property}"));
    }
    let output = systemctl(&arguments)?;
    if !output.status.success() {
        return Err(format!(
            "systemctl show {} failed: {}",
            unit,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| format!("systemctl show {unit} returned non-UTF8 data"))?;
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("malformed systemctl property for {unit}: {line}"))?;
        if fields.insert(key.to_string(), value.to_string()).is_some() {
            return Err(format!("duplicate systemctl property {key} for {unit}"));
        }
    }
    let required: BTreeSet<&str> = [
        "Id",
        "LoadState",
        "ActiveState",
        "SubState",
        "InvocationID",
        "MainPID",
        "ControlGroup",
        "Environment",
        "WorkingDirectory",
        "Transient",
        "Type",
        "Restart",
        "KillMode",
    ]
    .into_iter()
    .collect();
    if fields.keys().map(String::as_str).collect::<BTreeSet<_>>() != required {
        return Err(format!(
            "systemctl show {unit} did not return the exact property set"
        ));
    }
    let main_pid = fields["MainPID"]
        .parse::<u32>()
        .map_err(|_| format!("systemctl show {unit} has invalid MainPID"))?;
    Ok(UnitState {
        id: fields.remove("Id").unwrap(),
        load_state: fields.remove("LoadState").unwrap(),
        active_state: fields.remove("ActiveState").unwrap(),
        sub_state: fields.remove("SubState").unwrap(),
        invocation_id: fields.remove("InvocationID").unwrap(),
        main_pid,
        cgroup: fields.remove("ControlGroup").unwrap(),
        environment: fields.remove("Environment").unwrap(),
        working_directory: fields.remove("WorkingDirectory").unwrap(),
        transient: fields.remove("Transient").unwrap(),
        service_type: fields.remove("Type").unwrap(),
        restart: fields.remove("Restart").unwrap(),
        kill_mode: fields.remove("KillMode").unwrap(),
    })
}

fn fixture_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors().find_map(|ancestor| {
        ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with("release-worktree-test.")
                    || name.starts_with("allocate-worktree-test.")
                    || name.starts_with("codex-slot-sentinel-test.")
            })
            .then(|| ancestor.to_path_buf())
    })
}

fn evidence_roots(working_directory: &Path) -> Result<EvidenceRoots, String> {
    let Some(raw_test_root) = env::var_os("HERMIT_SENTINEL_TEST_ROOT") else {
        return Ok(EvidenceRoots {
            proc_root: PathBuf::from("/proc"),
            cgroup_root: PathBuf::from("/sys/fs/cgroup"),
        });
    };
    let test_root = fs::canonicalize(raw_test_root)
        .map_err(|error| format!("canonicalize HERMIT_SENTINEL_TEST_ROOT: {error}"))?;
    let fixture = fixture_ancestor(working_directory)
        .ok_or_else(|| "sentinel test override is restricted to disposable fixtures".to_string())?;
    if !test_root.starts_with(&fixture) || !working_directory.starts_with(&test_root) {
        return Err(
            "sentinel test override does not bind the fixture working directory".to_string(),
        );
    }
    Ok(EvidenceRoots {
        proc_root: test_root.join("proc"),
        cgroup_root: test_root.join("cgroup"),
    })
}

fn proc_starttime(proc_root: &Path, pid: u32) -> Result<Option<u64>, String> {
    let path = proc_root.join(pid.to_string()).join("stat");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let close = text
        .rfind(')')
        .ok_or_else(|| format!("malformed proc stat {}", path.display()))?;
    let starttime = text[close + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| format!("proc stat {} has no starttime", path.display()))?
        .parse::<u64>()
        .map_err(|_| format!("proc stat {} has invalid starttime", path.display()))?;
    Ok(Some(starttime))
}

fn proc_cgroup(proc_root: &Path, pid: u32) -> Result<String, String> {
    let path = proc_root.join(pid.to_string()).join("cgroup");
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let groups: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("0::"))
        .collect();
    if groups.len() != 1 || !normalized_cgroup(groups[0]) {
        return Err(format!("{} has no exact unified cgroup", path.display()));
    }
    Ok(groups[0].to_string())
}

fn cgroup_state(roots: &EvidenceRoots, cgroup: &str) -> Result<Option<(Vec<u32>, bool)>, String> {
    let directory = roots.cgroup_root.join(cgroup.trim_start_matches('/'));
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect {}: {error}", directory.display())),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "{} is not an exact cgroup directory",
            directory.display()
        ));
    }
    let members_text = fs::read_to_string(directory.join("cgroup.procs"))
        .map_err(|error| format!("read sentinel cgroup.procs: {error}"))?;
    let members = members_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.trim()
                .parse::<u32>()
                .map_err(|_| format!("sentinel cgroup has invalid pid {line:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let events = fs::read_to_string(directory.join("cgroup.events"))
        .map_err(|error| format!("read sentinel cgroup.events: {error}"))?;
    let populated: Vec<&str> = events
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter_map(|(key, value)| (key == "populated").then_some(value.trim()))
        .collect();
    let populated = match populated.as_slice() {
        ["0"] => false,
        ["1"] => true,
        _ => return Err("sentinel cgroup has no unique populated event".to_string()),
    };
    Ok(Some((members, populated)))
}

fn environment_token(lease: &SentinelLease) -> String {
    plan_environment_token(&lease.plan())
}

fn plan_environment_token(plan: &SentinelPlan) -> String {
    format!("{ENVIRONMENT_KEY}={}:{}", plan.generation, plan.nonce)
}

fn state_static_matches(lease: &SentinelLease, state: &UnitState) -> Result<(), String> {
    if state.id != lease.unit {
        return Err(format!(
            "sentinel unit identity changed: expected {}, got {}",
            lease.unit, state.id
        ));
    }
    if state.invocation_id != lease.invocation_id {
        return Err(format!(
            "sentinel InvocationID changed for {}: expected {}, got {}",
            lease.unit, lease.invocation_id, state.invocation_id
        ));
    }
    if state.main_pid != lease.main_pid {
        return Err(format!(
            "sentinel MainPID changed for {}: expected {}, got {}",
            lease.unit, lease.main_pid, state.main_pid
        ));
    }
    if state.cgroup != lease.cgroup_path {
        return Err(format!(
            "sentinel cgroup changed for {}: expected {}, got {}",
            lease.unit, lease.cgroup_path, state.cgroup
        ));
    }
    if state.working_directory != lease.working_directory {
        return Err(format!(
            "sentinel WorkingDirectory changed for {}",
            lease.unit
        ));
    }
    if !state
        .environment
        .split_whitespace()
        .any(|entry| entry == environment_token(lease))
    {
        return Err(format!(
            "sentinel nonce environment changed for {}",
            lease.unit
        ));
    }
    if state.transient != "yes"
        || state.service_type != "exec"
        || state.restart != "no"
        || state.kill_mode != "control-group"
    {
        return Err(format!(
            "sentinel service policy changed for {}: transient={} type={} restart={} kill_mode={}",
            lease.unit, state.transient, state.service_type, state.restart, state.kill_mode
        ));
    }
    Ok(())
}

fn verify_live(lease: &SentinelLease) -> Result<(), String> {
    lease.validate()?;
    let working = Path::new(&lease.working_directory);
    let roots = evidence_roots(working)?;
    let state = query_unit(&lease.unit)?;
    state_static_matches(lease, &state)?;
    if state.load_state != "loaded"
        || state.active_state != "active"
        || state.sub_state != "running"
    {
        return Err(format!(
            "sentinel {} died unexpectedly: load={} active={} sub={}",
            lease.unit, state.load_state, state.active_state, state.sub_state
        ));
    }
    if proc_starttime(&roots.proc_root, lease.main_pid)? != Some(lease.main_pid_starttime) {
        return Err(format!(
            "sentinel PID/starttime identity is gone or reused for {}",
            lease.unit
        ));
    }
    let cwd = fs::canonicalize(roots.proc_root.join(lease.main_pid.to_string()).join("cwd"))
        .map_err(|error| format!("resolve sentinel cwd for {}: {error}", lease.unit))?;
    let expected = fs::canonicalize(working)
        .map_err(|error| format!("resolve sentinel working directory: {error}"))?;
    if cwd != expected {
        return Err(format!(
            "sentinel process cwd {} does not bind {}",
            cwd.display(),
            expected.display()
        ));
    }
    if proc_cgroup(&roots.proc_root, lease.main_pid)? != lease.cgroup_path {
        return Err(format!(
            "sentinel process cgroup does not bind {}",
            lease.cgroup_path
        ));
    }
    let Some((members, populated)) = cgroup_state(&roots, &lease.cgroup_path)? else {
        return Err(format!("sentinel cgroup {} vanished", lease.cgroup_path));
    };
    if !populated || !members.contains(&lease.main_pid) {
        return Err(format!(
            "sentinel cgroup {} is not populated by MainPID {}",
            lease.cgroup_path, lease.main_pid
        ));
    }
    Ok(())
}

fn prove_revoked(lease: &SentinelLease) -> Result<RevocationProof, String> {
    lease.validate()?;
    let roots = evidence_roots(Path::new(&lease.working_directory))?;
    let state = query_unit(&lease.unit)?;
    if state.active_state == "active" {
        if !state.invocation_id.is_empty() && state.invocation_id != lease.invocation_id {
            return Err(format!(
                "sentinel unit {} restarted with InvocationID {}",
                lease.unit, state.invocation_id
            ));
        }
        return Err(format!("sentinel unit {} remains active", lease.unit));
    }
    if !state.invocation_id.is_empty() && state.invocation_id != lease.invocation_id {
        return Err(format!(
            "sentinel unit {} changed InvocationID during revocation",
            lease.unit
        ));
    }
    let observed_starttime = proc_starttime(&roots.proc_root, lease.main_pid)?;
    if observed_starttime == Some(lease.main_pid_starttime) {
        return Err(format!(
            "sentinel PID {} with recorded starttime remains",
            lease.main_pid
        ));
    }
    let cgroup = cgroup_state(&roots, &lease.cgroup_path)?;
    if let Some((members, populated)) = &cgroup {
        if *populated || !members.is_empty() {
            return Err(format!(
                "sentinel cgroup {} remains populated (members={})",
                lease.cgroup_path,
                members.len()
            ));
        }
    }
    let (cgroup_absent, cgroup_members, cgroup_populated) = match cgroup {
        None => (true, Vec::new(), None),
        Some((members, populated)) => (false, members, Some(populated)),
    };
    let observed_at_unix_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?
        .as_nanos();
    Ok(RevocationProof {
        observed_at_unix_ns,
        unit_id: state.id,
        load_state: state.load_state,
        active_state: state.active_state,
        sub_state: state.sub_state,
        invocation_id: state.invocation_id,
        recorded_main_pid: lease.main_pid,
        recorded_main_pid_starttime: lease.main_pid_starttime,
        observed_main_pid_starttime: observed_starttime,
        cgroup_path: lease.cgroup_path.clone(),
        cgroup_absent,
        cgroup_members,
        cgroup_populated,
    })
}

fn secure_hex(bytes: usize) -> Result<String, String> {
    let mut raw = vec![0u8; bytes];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut raw))
        .map_err(|error| format!("read secure sentinel randomness: {error}"))?;
    Ok(raw.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn stop_unit(unit: &str) -> Result<(), String> {
    let output = bounded(
        &fixture_tool("HERMIT_SENTINEL_TEST_SYSTEMCTL", "/usr/bin/systemctl")?,
        &[
            "--user".to_string(),
            "stop".to_string(),
            "--".to_string(),
            unit.to_string(),
        ],
        MUTATION_TIMEOUT,
    )
    .map_err(|error| format!("systemctl stop unavailable: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "systemctl stop {unit} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn plan(slot: &str, working_directory: &Path) -> Result<SentinelPlan, String> {
    if !worktree_registry::valid_slot_name(slot) {
        return Err("sentinel slot is invalid".to_string());
    }
    let working_directory = fs::canonicalize(working_directory)
        .map_err(|error| format!("canonicalize sentinel working directory: {error}"))?;
    if !normalized_absolute(&working_directory) {
        return Err("sentinel working directory is not normalized absolute".to_string());
    }
    let generation = secure_hex(8)?;
    let nonce = secure_hex(16)?;
    SentinelPlan::current(slot, generation, nonce, &working_directory)
}

fn state_matches_plan(plan: &SentinelPlan, state: &UnitState) -> Result<(), String> {
    if state.id != plan.unit
        || state.working_directory != plan.working_directory
        || !state
            .environment
            .split_whitespace()
            .any(|entry| entry == plan_environment_token(plan))
        || state.transient != "yes"
        || state.service_type != "exec"
        || state.restart != "no"
        || state.kill_mode != "control-group"
    {
        return Err(format!(
            "existing sentinel unit {} does not bind the durable launch intent",
            plan.unit
        ));
    }
    Ok(())
}

fn launch(plan: &SentinelPlan) -> Result<SentinelLease, String> {
    plan.validate()?;
    let working_directory = PathBuf::from(&plan.working_directory);
    let before = query_unit(&plan.unit)?;
    if before.load_state == "not-found" {
        let arguments = vec![
            "--user".to_string(),
            "--quiet".to_string(),
            format!("--unit={}", plan.unit),
            "--property=Type=exec".to_string(),
            "--property=Restart=no".to_string(),
            "--property=KillMode=control-group".to_string(),
            format!("--working-directory={}", plan.working_directory),
            format!("--setenv={}", plan_environment_token(plan)),
            "/usr/bin/sleep".to_string(),
            "infinity".to_string(),
        ];
        let output = bounded(
            &fixture_tool("HERMIT_SENTINEL_TEST_SYSTEMD_RUN", "/usr/bin/systemd-run")?,
            &arguments,
            MUTATION_TIMEOUT,
        )
        .map_err(|error| format!("systemd-run unavailable: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "systemd-run {} failed: {}",
                plan.unit,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    } else {
        // Recovery after a crash between unit creation and final lease storage
        // may adopt only the exact pre-journaled plan, never a name collision.
        state_matches_plan(plan, &before)?;
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    let result = loop {
        let state = query_unit(&plan.unit)?;
        if state.load_state == "loaded"
            && state.active_state == "active"
            && state.sub_state == "running"
            && state.main_pid != 0
        {
            state_matches_plan(plan, &state)?;
            let roots = evidence_roots(&working_directory)?;
            let starttime = proc_starttime(&roots.proc_root, state.main_pid)?.ok_or_else(|| {
                format!("sentinel MainPID {} has no proc identity", state.main_pid)
            })?;
            let lease = SentinelLease::current(
                plan,
                state.invocation_id,
                state.main_pid,
                starttime,
                state.cgroup,
            )?;
            verify_live(&lease)?;
            break Ok(lease);
        }
        if Instant::now() >= deadline {
            break Err(format!(
                "sentinel unit {} did not become exactly live",
                plan.unit
            ));
        }
        thread::sleep(Duration::from_millis(50));
    };
    if result.is_err() {
        if let Ok(state) = query_unit(&plan.unit) {
            if state_matches_plan(plan, &state).is_ok() {
                let _ = stop_unit(&plan.unit);
            }
        }
    }
    result
}

fn revoke(lease: &SentinelLease, recover: bool) -> Result<RevocationProof, String> {
    if let Err(live_error) = verify_live(lease) {
        if !recover {
            return Err(live_error);
        }
        return prove_revoked(lease).map_err(|revoked_error| {
            format!(
                "journaled sentinel is neither exact-live nor proven revoked: live={live_error}; revoked={revoked_error}"
            )
        });
    }
    stop_unit(&lease.unit)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match prove_revoked(lease) {
            Ok(proof) => return Ok(proof),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn parse_lease(raw: &str) -> Result<SentinelLease, String> {
    SentinelLease::from_json(raw)
}

fn parse_plan(raw: &str) -> Result<SentinelPlan, String> {
    SentinelPlan::from_json(raw)
}

fn main() {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments.is_empty() || matches!(arguments[0].as_str(), "-h" | "--help") {
        print!("{USAGE}");
        return;
    }
    let command = arguments[0].as_str();
    let mut slot: Option<String> = None;
    let mut working_directory: Option<PathBuf> = None;
    let mut plan_json: Option<String> = None;
    let mut lease_json: Option<String> = None;
    let mut recover = false;
    let mut index = 1;
    while index < arguments.len() {
        let take = |index: &mut usize, name: &str| -> String {
            *index += 1;
            arguments
                .get(*index)
                .cloned()
                .unwrap_or_else(|| fail(format!("{name} requires a value")))
        };
        match arguments[index].as_str() {
            "--slot" => slot = Some(take(&mut index, "--slot")),
            "--working-directory" => {
                working_directory = Some(PathBuf::from(take(&mut index, "--working-directory")))
            }
            "--plan-json" => plan_json = Some(take(&mut index, "--plan-json")),
            "--lease-json" => lease_json = Some(take(&mut index, "--lease-json")),
            "--recover" => recover = true,
            other => fail(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
        index += 1;
    }

    let result = match command {
        "plan" => {
            if plan_json.is_some() || lease_json.is_some() || recover {
                fail("plan does not accept --plan-json/--lease-json/--recover");
            }
            let slot = slot
                .as_deref()
                .unwrap_or_else(|| fail("plan requires --slot"));
            let working = working_directory
                .as_deref()
                .unwrap_or_else(|| fail("plan requires --working-directory"));
            plan(slot, working).map(|plan| json!({"state": "planned", "plan": plan}))
        }
        "launch" => {
            if slot.is_some() || working_directory.is_some() || lease_json.is_some() || recover {
                fail("launch accepts only --plan-json");
            }
            let plan = parse_plan(
                plan_json
                    .as_deref()
                    .unwrap_or_else(|| fail("launch requires --plan-json")),
            )
            .unwrap_or_else(|error| fail(error));
            launch(&plan).map(|lease| json!({"state": "live", "lease": lease}))
        }
        "verify" | "revoke" | "prove-revoked" => {
            if slot.is_some() || working_directory.is_some() || plan_json.is_some() {
                fail(format!("{command} accepts only --lease-json"));
            }
            let lease = parse_lease(
                lease_json
                    .as_deref()
                    .unwrap_or_else(|| fail(format!("{command} requires --lease-json"))),
            )
            .unwrap_or_else(|error| fail(error));
            match command {
                "verify" if recover => fail("verify does not accept --recover"),
                "verify" => verify_live(&lease).map(|()| json!({"state": "live", "lease": lease})),
                "revoke" => revoke(&lease, recover)
                    .map(|proof| json!({"state": "revoked", "lease": lease, "proof": proof})),
                "prove-revoked" if recover => fail("prove-revoked does not accept --recover"),
                "prove-revoked" => prove_revoked(&lease)
                    .map(|proof| json!({"state": "revoked", "lease": lease, "proof": proof})),
                _ => unreachable!(),
            }
        }
        other => fail(format!("unknown command {other:?}\n\n{USAGE}")),
    };
    match result {
        Ok(value) => println!("{}", serde_json::to_string(&value).unwrap()),
        Err(error) => fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat_starttime_parser_handles_spaces_in_comm() {
        let root = env::temp_dir().join(format!(
            "codex-slot-sentinel-test.{}.starttime",
            std::process::id()
        ));
        let pid_dir = root.join("proc/17");
        fs::create_dir_all(&pid_dir).unwrap();
        let mut fields = vec!["S".to_string()];
        fields.extend((4..=21).map(|number| number.to_string()));
        fields.push("424242".to_string());
        fs::write(
            pid_dir.join("stat"),
            format!("17 (sentinel with spaces) {}\n", fields.join(" ")),
        )
        .unwrap();
        assert_eq!(
            proc_starttime(&root.join("proc"), 17).unwrap(),
            Some(424242)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lease_schema_binds_slot_generation_unit_and_cgroup() {
        let plan = SentinelPlan::current(
            "slot01",
            "0123456789abcdef".to_string(),
            "0123456789abcdef0123456789abcdef".to_string(),
            Path::new("/tmp/worktrees/slots/slot01"),
        )
        .unwrap();
        let lease = SentinelLease::current(
            &plan,
            "abcdefabcdefabcdefabcdefabcdefab".to_string(),
            17,
            99,
            "/user.slice/user-1.slice/user@1.service/app.slice/dev-hermit-slot-slot01-0123456789abcdef.service"
                .to_string(),
        )
        .unwrap();
        let mut wrong = lease.clone();
        wrong.unit = "dev-hermit-slot-other-0123456789abcdef.service".to_string();
        assert!(SentinelLease::from_value(&wrong.to_value().unwrap()).is_err());
    }
}
