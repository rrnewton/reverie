#!/usr/bin/env rust-script
//! Cross-backend **env-block parity** check.
//!
//! A Hermit backend must not hand the guest a different environment than the
//! ptrace reference does. When it does, every env-derived guest observable
//! diverges for a reason that has nothing to do with the backend's semantics:
//! guest stdout differs for any program that reads its own environment, and
//! every `--detlog-stack` hash differs because the env strings live inside the
//! hashed `[stack]` VMA. Such a divergence reads as a backend finding but is an
//! artifact of the measurement setup, so it has to be detected explicitly
//! rather than absorbed into a parity number.
//!
//! This check runs one fixed guest (`fixtures/env_block_probe.c`) under each
//! backend and compares the guest-visible environment against the reference
//! arm, byte-exact and order-sensitive.
//!
//! ## Two authorities, compared separately
//!
//! "The guest's environment" is not one fact. The probe reports both channels
//! a guest can observe, and they are compared independently:
//!
//!   * `environ` -- the libc array that `getenv()` and `environ` walks see.
//!   * `procenv` -- `/proc/self/environ`, the kernel's view of the original
//!     block on the initial stack, compared as an ENTRY LIST.
//!   * `rawblock` -- the same kernel block compared BYTE FOR BYTE.
//!
//! The third is not redundant. A backend can hide an injected variable by
//! zeroing its bytes and shifting it out of `environ` (this is exactly what
//! `reverie-sabre`'s `take_private_env` does). Both entry lists then match the
//! reference perfectly while the block is still longer by the length of the
//! blanked entry -- bytes that sit in the hashed `[stack]` VMA and shift the
//! guest stack. Comparing only entry lists reports parity that is not there.
//!
//! ## Declared residuals
//!
//! A residual is a variable a backend is currently known to add for a reason
//! outside this repository's control. Each one is declared with its owner and
//! why it is not simply removed; anything NOT declared is a failure. Passing
//! `--residuals none` drops the allowlist, which is how the check is shown to
//! be non-inert: with an empty allowlist the declared residuals must fire.
//!
//! ## Both brackets
//!
//! * Positive: arms that genuinely match must report IDENTICAL (a check that
//!   only ever says "differs" proves nothing).
//! * Negative: `--plant NAME=VALUE` injects a deliberate extra variable into
//!   one arm; the check must catch it. `--plant` is deliberately inert with
//!   respect to any authorization -- it only adds a guest env var.
//!
//! Usage:
//!   compat-envelope/check-env-block-parity.rs [OPTIONS]
//!
//! ```cargo
//! [dependencies]
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
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

const USAGE: &str = r#"Usage: compat-envelope/check-env-block-parity.rs [OPTIONS]

Compare the guest-visible environment block across Hermit backends.

Options:
  --hermit PATH       hermit binary (default: hermit/target/debug/hermit)
  --backends LIST     comma-separated arms to compare. An arm may be written
                      `<backend>#<label>` to run the same backend under a
                      second name; `ptrace#control` is a control arm that must
                      come out IDENTICAL, which is what shows the check is not
                      simply stuck reporting DIVERGES.
                      (default: ptrace,ptrace#control,sabre,dbi -- unavailable
                      arms are reported UNAVAILABLE, never silently dropped)
  --reference NAME    reference arm every other arm is compared to
                      (default: ptrace)
  --guest PATH        prebuilt probe binary (default: build the fixture)
  --build-dir DIR     where to build the probe
                      (default: ignored/envparity)
  --residuals MODE    declared (default) | none
                      'none' drops the allowlist so declared residuals fire
  --allow NAME        additionally tolerate this variable (repeatable)
  --plant NAME=VALUE  inject an extra variable into --plant-arm (repeatable)
  --plant-arm NAME    arm to plant into (default: the reference arm)
  --extra-arg ARG     extra argument passed to every `hermit run` (repeatable)
  --json PATH         write the full record as JSON
  -h, --help          this message

Exit codes:
  0  every requested non-reference arm was measured and is at parity
     with the reference (within the allowlist)
  1  at least one arm diverges
  2  a requested arm was unavailable or no comparison was made
     (nothing was proven -- not a pass)
"#;

/// A variable a backend adds for a reason outside this repository's control.
///
/// Anything not listed here is a failure. The attribution is part of the
/// record, so a pass states what it tolerated and why rather than just being
/// a smaller number.
struct Residual {
    name: &'static str,
    arm: &'static str,
    owner: &'static str,
    why: &'static str,
}

const DECLARED_RESIDUALS: &[Residual] = &[
    Residual {
        name: "DYNAMORIO_CONFIGDIR",
        arm: "dbi",
        owner: "third-party DynamoRIO `drrun` (core/unix/injector.c)",
        why: "drrun sets it before exec so DR can find its config; DR also \
              relies on it propagating to children for follow-children \
              injection, so removing it in-guest risks a child escaping \
              instrumentation. Not a drive-by fix.",
    },
    Residual {
        name: "DYNAMORIO_TAKEOVER_IN_INIT",
        arm: "dbi",
        owner: "third-party DynamoRIO `drrun` (core/unix/injector.c:379,571)",
        why: "read by DR at init (core/unix/os.c:777). Consumed before the \
              client runs, so it is scrubbable in principle -- but only \
              together with the follow-children question above.",
    },
    Residual {
        name: "DYNAMORIO_EXE_PATH",
        arm: "dbi",
        owner: "third-party DynamoRIO `drrun`",
        why: "embeds the guest's own path, so this residual's SIZE VARIES PER \
              GUEST. Any byte-count budget derived from one guest does not \
              transfer to another.",
    },
];

/// One arm's measurement, or why it has none.
enum Arm {
    Measured {
        environ: Vec<String>,
        procenv: Vec<String>,
        environ_bytes: usize,
        procenv_bytes: usize,
        /// The kernel env block verbatim, as read from /proc/self/environ.
        rawblock: Vec<u8>,
    },
    Unavailable {
        reason: String,
    },
}

/// Describe how two raw blocks differ, in terms a reader can act on.
///
/// Reports the first differing offset and the run of NUL padding that a
/// blank-in-place scrub leaves behind, because that is the signature of an
/// injected-then-hidden variable rather than a random byte difference.
fn describe_raw_difference(reference: &[u8], arm: &[u8]) -> Vec<String> {
    let mut notes = Vec::new();
    if reference == arm {
        return notes;
    }
    notes.push(format!(
        "block length {} vs reference {} ({:+} bytes)",
        arm.len(),
        reference.len(),
        arm.len() as isize - reference.len() as isize
    ));
    let first = reference
        .iter()
        .zip(arm.iter())
        .position(|(a, b)| a != b)
        .unwrap_or(reference.len().min(arm.len()));
    notes.push(format!("first differing byte at offset {first}"));

    let longest_nul_run = |block: &[u8]| -> (usize, usize) {
        let (mut best_len, mut best_at, mut run, mut start) = (0usize, 0usize, 0usize, 0usize);
        for (index, byte) in block.iter().enumerate() {
            if *byte == 0 {
                if run == 0 {
                    start = index;
                }
                run += 1;
                if run > best_len {
                    best_len = run;
                    best_at = start;
                }
            } else {
                run = 0;
            }
        }
        (best_len, best_at)
    };
    let (reference_run, _) = longest_nul_run(reference);
    let (arm_run, arm_at) = longest_nul_run(arm);
    if arm_run > reference_run {
        notes.push(format!(
            "longest NUL run {arm_run} at offset {arm_at} vs {reference_run} in the reference \
             -- the signature of a variable that was injected before exec and then BLANKED \
             IN PLACE (the entry vanishes from `environ`, the bytes stay in the block)",
        ));
    }
    notes
}

struct Options {
    hermit: PathBuf,
    backends: Vec<String>,
    reference: String,
    guest: Option<PathBuf>,
    build_dir: PathBuf,
    residuals_declared: bool,
    allow: Vec<String>,
    plant: Vec<String>,
    plant_arm: Option<String>,
    extra_args: Vec<String>,
    json: Option<PathBuf>,
}

/// Locate the dev-hermit root by walking up from the cwd until the tracked
/// probe fixture is visible. rust-script compiles to a temp binary, so the
/// script's own path is not available at runtime.
fn repo_root() -> PathBuf {
    let start = std::env::current_dir().expect("cwd");
    let mut candidate = start.as_path();
    loop {
        if candidate
            .join("compat-envelope/fixtures/env_block_probe.c")
            .is_file()
        {
            return candidate.to_path_buf();
        }
        match candidate.parent() {
            Some(parent) => candidate = parent,
            None => fail(
                "cannot locate the dev-hermit root (no \
                 compat-envelope/fixtures/env_block_probe.c above the cwd)",
            ),
        }
    }
}

fn parse_args() -> Options {
    let root = repo_root();
    let mut options = Options {
        hermit: root.join("hermit/target/debug/hermit"),
        backends: vec![
            "ptrace".into(),
            "ptrace#control".into(),
            "sabre".into(),
            "dbi".into(),
        ],
        reference: "ptrace".into(),
        guest: None,
        build_dir: root.join("ignored/envparity"),
        residuals_declared: true,
        allow: Vec::new(),
        plant: Vec::new(),
        plant_arm: None,
        extra_args: Vec::new(),
        json: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| fail(&format!("{arg} needs a value")))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                exit(0);
            }
            "--hermit" => options.hermit = PathBuf::from(value()),
            "--backends" => {
                options.backends = value().split(',').map(|s| s.trim().to_string()).collect()
            }
            "--reference" => options.reference = value(),
            "--guest" => options.guest = Some(PathBuf::from(value())),
            "--build-dir" => options.build_dir = PathBuf::from(value()),
            "--residuals" => {
                options.residuals_declared = match value().as_str() {
                    "declared" => true,
                    "none" => false,
                    other => fail(&format!("--residuals wants declared|none, got {other}")),
                }
            }
            "--allow" => options.allow.push(value()),
            "--plant" => options.plant.push(value()),
            "--plant-arm" => options.plant_arm = Some(value()),
            "--extra-arg" => options.extra_args.push(value()),
            "--json" => options.json = Some(PathBuf::from(value())),
            other => fail(&format!("unknown option {other}\n\n{USAGE}")),
        }
    }
    if !options.backends.contains(&options.reference) {
        options.backends.insert(0, options.reference.clone());
    }
    options
}

fn fail(message: &str) -> ! {
    eprintln!("check-env-block-parity: {message}");
    exit(2);
}

fn decode_hex(bytes: &str) -> Result<Vec<u8>, String> {
    if bytes.len() % 2 != 0 {
        return Err("odd number of hex digits".to_string());
    }
    (0..bytes.len() / 2)
        .map(|index| {
            u8::from_str_radix(&bytes[index * 2..index * 2 + 2], 16)
                .map_err(|_| format!("non-hex byte at offset {}", index * 2))
        })
        .collect()
}

fn verdict_exit_code(diverged: usize, unavailable: usize, identical: usize) -> i32 {
    if diverged > 0 {
        1
    } else if unavailable > 0 || identical == 0 {
        2
    } else {
        0
    }
}

/// Build the probe fixture. Its source is tracked; the binary is not.
fn build_probe(root: &Path, build_dir: &Path) -> PathBuf {
    let source = root.join("compat-envelope/fixtures/env_block_probe.c");
    if !source.is_file() {
        fail(&format!("probe source not found: {}", source.display()));
    }
    std::fs::create_dir_all(build_dir)
        .unwrap_or_else(|e| fail(&format!("cannot create {}: {e}", build_dir.display())));
    let binary = build_dir.join("env_block_probe");
    let status = Command::new("gcc")
        .args(["-O0", "-Wall", "-o"])
        .arg(&binary)
        .arg(&source)
        .status()
        .unwrap_or_else(|e| fail(&format!("cannot run gcc: {e}")));
    if !status.success() {
        fail("failed to build the env-block probe");
    }
    // hermit isolates the guest's /tmp, so the guest must not live there.
    if binary.starts_with("/tmp") {
        fail("the probe must not live under /tmp (hermit isolates the guest's /tmp)");
    }
    binary
}

/// Run the probe under one backend and parse the two env authorities out of it.
fn measure(options: &Options, arm: &str, guest: &Path) -> Arm {
    // `<backend>#<label>` runs `<backend>` but is reported under the full name,
    // so the same backend can appear twice (see the control arm).
    let backend = arm.split('#').next().unwrap_or(arm);
    let mut command = Command::new(&options.hermit);
    command.arg("--backend").arg(backend).arg("run");
    // A pinned base env is the precondition for the comparison meaning
    // anything: with `--base-env host` the two arms would differ by whatever
    // the two shells happened to carry.
    command.args(["--base-env", "minimal", "-e", "LC_ALL=C", "-e", "TZ=UTC"]);
    for extra in &options.extra_args {
        command.arg(extra);
    }
    let plant_arm = options.plant_arm.as_deref().unwrap_or(&options.reference);
    if arm == plant_arm {
        for planted in &options.plant {
            command.arg("-e").arg(planted);
        }
    }
    command.arg("--").arg(guest);

    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            return Arm::Unavailable {
                reason: format!("could not spawn hermit: {error}"),
            }
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(3).collect();
        return Arm::Unavailable {
            reason: format!(
                "hermit --backend {backend} run exited {:?}: {}",
                output.status.code(),
                tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
            ),
        };
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut environ = Vec::new();
    let mut procenv = Vec::new();
    let mut environ_bytes = 0usize;
    let mut procenv_bytes = 0usize;
    let mut rawblock: Vec<u8> = Vec::new();
    let mut saw_sizes = false;
    for line in stdout.lines() {
        if let Some(entry) = line.strip_prefix("ENVIRON\t") {
            environ.push(entry.to_string());
        } else if let Some(entry) = line.strip_prefix("PROCENV\t") {
            procenv.push(entry.to_string());
        } else if let Some(hex) = line.strip_prefix("PROCRAW\t") {
            let bytes = hex.trim();
            rawblock = match decode_hex(bytes) {
                Ok(rawblock) => rawblock,
                Err(reason) => {
                    return Arm::Unavailable {
                        reason: format!("malformed PROCRAW hex under {backend}: {reason}"),
                    }
                }
            };
        } else if let Some(sizes) = line.strip_prefix("SIZES\t") {
            saw_sizes = true;
            for field in sizes.split_whitespace() {
                let Some((key, raw)) = field.split_once('=') else {
                    continue;
                };
                let parsed = raw.parse().unwrap_or(0);
                match key {
                    "environ_bytes" => environ_bytes = parsed,
                    "procenv_bytes" => procenv_bytes = parsed,
                    _ => {}
                }
            }
        } else if line.starts_with("PROCENV_UNAVAILABLE") {
            return Arm::Unavailable {
                reason: format!("the guest could not read /proc/self/environ under {backend}"),
            };
        }
    }
    // A run that produced no probe output is a no-result, not an empty env.
    if !saw_sizes || environ.is_empty() {
        return Arm::Unavailable {
            reason: format!(
                "the probe produced no parseable output under {backend} \
                 ({} stdout bytes) -- treating as UNKNOWN, not as parity",
                output.stdout.len()
            ),
        };
    }
    if rawblock.is_empty() {
        return Arm::Unavailable {
            reason: format!(
                "the probe emitted no PROCRAW block under {backend} -- the byte-exact \
                 comparison is the only channel that catches a blanked-in-place variable, \
                 so without it nothing is proven"
            ),
        };
    }
    Arm::Measured {
        environ,
        procenv,
        environ_bytes,
        procenv_bytes,
        rawblock,
    }
}

/// The variable name of an `NAME=VALUE` entry.
fn key_of(entry: &str) -> &str {
    entry.split_once('=').map(|(k, _)| k).unwrap_or(entry)
}

/// Remove tolerated variables' bytes from a raw block so the remainder is
/// comparable across arms.
///
/// Only NAMED entries can be tolerated. Runs of NUL padding are deliberately
/// preserved: they are what a blank-in-place scrub leaves behind, they belong
/// to no variable, and no allowlist entry can excuse them.
fn strip_tolerated_from_block(block: &[u8], allowed: &[String]) -> Vec<u8> {
    let mut kept: Vec<&[u8]> = Vec::new();
    for chunk in block.split(|byte| *byte == 0) {
        if chunk.is_empty() {
            kept.push(chunk);
            continue;
        }
        let text = String::from_utf8_lossy(chunk);
        if allowed.iter().any(|name| name == key_of(&text)) {
            continue;
        }
        kept.push(chunk);
    }
    kept.join(&0u8)
}

struct ChannelVerdict {
    only_in_arm: Vec<String>,
    only_in_reference: Vec<String>,
    changed: Vec<String>,
    order_differs: bool,
    tolerated: Vec<String>,
}

impl ChannelVerdict {
    fn is_parity(&self) -> bool {
        self.only_in_arm.is_empty()
            && self.only_in_reference.is_empty()
            && self.changed.is_empty()
            && !self.order_differs
    }
}

fn compare_channel(reference: &[String], arm: &[String], allowed: &[String]) -> ChannelVerdict {
    let index = |entries: &[String]| -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|entry| (key_of(entry).to_string(), entry.clone()))
            .collect()
    };
    let reference_index = index(reference);
    let arm_index = index(arm);
    let tolerate = |name: &str| allowed.iter().any(|a| a == name);

    let mut verdict = ChannelVerdict {
        only_in_arm: Vec::new(),
        only_in_reference: Vec::new(),
        changed: Vec::new(),
        order_differs: false,
        tolerated: Vec::new(),
    };
    for (name, entry) in &arm_index {
        match reference_index.get(name) {
            None if tolerate(name) => verdict.tolerated.push(name.clone()),
            None => verdict.only_in_arm.push(entry.clone()),
            Some(reference_entry) if reference_entry != entry && !tolerate(name) => verdict
                .changed
                .push(format!("{reference_entry}  ->  {entry}")),
            Some(_) => {}
        }
    }
    for (name, entry) in &reference_index {
        if !arm_index.contains_key(name) {
            if tolerate(name) {
                verdict.tolerated.push(name.clone());
            } else {
                verdict.only_in_reference.push(entry.clone());
            }
        }
    }
    // Order is part of the block: the same set in a different order is a
    // different block. Only meaningful once the sets match modulo tolerated
    // names, so compare the sequences with tolerated names removed.
    let strip = |entries: &[String]| -> Vec<String> {
        entries
            .iter()
            .filter(|entry| !tolerate(key_of(entry)))
            .cloned()
            .collect()
    };
    verdict.order_differs = strip(reference) != strip(arm);
    verdict.tolerated.sort();
    verdict.tolerated.dedup();
    verdict
}

/// Shorten a value for the human report. The full text always survives in the
/// `--json` record, so truncating here loses nothing an auditor needs.
fn abbreviate(entry: &str) -> String {
    const LIMIT: usize = 120;
    if entry.chars().count() <= LIMIT {
        return entry.to_string();
    }
    let head: String = entry.chars().take(LIMIT).collect();
    format!(
        "{head}... [{} chars total, full value in --json]",
        entry.chars().count()
    )
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn json_list(values: &[String]) -> String {
    let items: Vec<String> = values
        .iter()
        .map(|v| format!("\"{}\"", json_escape(v)))
        .collect();
    format!("[{}]", items.join(","))
}

fn main() {
    let options = parse_args();
    let root = repo_root();

    if !options.hermit.is_file() {
        fail(&format!(
            "hermit binary not found: {} (pass --hermit)",
            options.hermit.display()
        ));
    }
    let guest = match &options.guest {
        Some(path) => path.clone(),
        None => build_probe(&root, &options.build_dir),
    };

    // The allowlist is per-arm: a residual declared for `dbi` must not excuse
    // the same variable appearing under `sabre`.
    let allowed_for = |arm: &str| -> Vec<String> {
        let backend = arm.split('#').next().unwrap_or(arm);
        let mut allowed: Vec<String> = options.allow.clone();
        if options.residuals_declared {
            allowed.extend(
                DECLARED_RESIDUALS
                    .iter()
                    .filter(|residual| residual.arm == backend)
                    .map(|residual| residual.name.to_string()),
            );
        }
        allowed
    };

    let mut arms: BTreeMap<String, Arm> = BTreeMap::new();
    for backend in &options.backends {
        arms.insert(backend.clone(), measure(&options, backend, &guest));
    }

    let plant_arm = options
        .plant_arm
        .clone()
        .unwrap_or_else(|| options.reference.clone());

    println!("== env-block parity ==");
    println!("hermit     : {}", options.hermit.display());
    println!("guest      : {}", guest.display());
    println!("reference  : {}", options.reference);
    println!(
        "residuals  : {}",
        if options.residuals_declared {
            "declared (see the attribution table below)"
        } else {
            "NONE -- declared residuals are expected to fire"
        }
    );
    if !options.plant.is_empty() {
        println!("planted    : {:?} into arm '{plant_arm}'", options.plant);
    }
    println!();

    let Some(Arm::Measured {
        environ: reference_environ,
        procenv: reference_procenv,
        environ_bytes: reference_environ_bytes,
        procenv_bytes: reference_procenv_bytes,
        rawblock: reference_rawblock,
    }) = arms.get(&options.reference)
    else {
        let reason = match arms.get(&options.reference) {
            Some(Arm::Unavailable { reason }) => reason.clone(),
            _ => "not measured".to_string(),
        };
        eprintln!(
            "UNKNOWN: the reference arm '{}' produced no measurement: {reason}",
            options.reference
        );
        eprintln!("Nothing was compared, so nothing is proven. This is not a pass.");
        exit(2);
    };

    println!(
        "{:<15} {:>6} {:>8} {:>6} {:>8} {:>8}  {}",
        "arm", "envN", "envB", "procN", "procB", "rawB", "verdict"
    );
    println!(
        "{:<15} {:>6} {:>8} {:>6} {:>8} {:>8}  {}",
        options.reference,
        reference_environ.len(),
        reference_environ_bytes,
        reference_procenv.len(),
        reference_procenv_bytes,
        reference_rawblock.len(),
        "REFERENCE"
    );

    let mut diverged: Vec<String> = Vec::new();
    let mut unavailable: Vec<String> = Vec::new();
    let mut identical: Vec<String> = Vec::new();
    let mut details = String::new();
    let mut json_arms: Vec<String> = Vec::new();

    for (backend, arm) in &arms {
        if backend == &options.reference {
            continue;
        }
        match arm {
            Arm::Unavailable { reason } => {
                println!(
                    "{:<15} {:>6} {:>8} {:>6} {:>8} {:>8}  UNAVAILABLE",
                    backend, "-", "-", "-", "-", "-"
                );
                let _ = writeln!(details, "\n-- {backend}: UNAVAILABLE --\n  {reason}");
                unavailable.push(backend.clone());
                json_arms.push(format!(
                    "{{\"arm\":\"{}\",\"status\":\"unavailable\",\"reason\":\"{}\"}}",
                    json_escape(backend),
                    json_escape(reason)
                ));
            }
            Arm::Measured {
                environ,
                procenv,
                environ_bytes,
                procenv_bytes,
                rawblock,
            } => {
                let allowed = allowed_for(backend);
                let env_verdict = compare_channel(reference_environ, environ, &allowed);
                let proc_verdict = compare_channel(reference_procenv, procenv, &allowed);
                let raw_notes = describe_raw_difference(
                    &strip_tolerated_from_block(reference_rawblock, &allowed),
                    &strip_tolerated_from_block(rawblock, &allowed),
                );
                let parity =
                    env_verdict.is_parity() && proc_verdict.is_parity() && raw_notes.is_empty();
                println!(
                    "{:<15} {:>6} {:>8} {:>6} {:>8} {:>8}  {}",
                    backend,
                    environ.len(),
                    environ_bytes,
                    procenv.len(),
                    procenv_bytes,
                    rawblock.len(),
                    if parity { "IDENTICAL" } else { "DIVERGES" }
                );
                if parity {
                    identical.push(backend.clone());
                } else {
                    diverged.push(backend.clone());
                }
                for (channel, verdict) in [("environ", &env_verdict), ("procenv", &proc_verdict)] {
                    if verdict.is_parity() && verdict.tolerated.is_empty() {
                        continue;
                    }
                    let _ = writeln!(details, "\n-- {backend} / {channel} --");
                    for entry in &verdict.only_in_arm {
                        let _ =
                            writeln!(details, "  + only under {backend}: {}", abbreviate(entry));
                    }
                    for entry in &verdict.only_in_reference {
                        let _ = writeln!(
                            details,
                            "  - only under {}: {}",
                            options.reference,
                            abbreviate(entry)
                        );
                    }
                    for entry in &verdict.changed {
                        let _ = writeln!(details, "  ~ value differs: {}", abbreviate(entry));
                    }
                    if verdict.order_differs
                        && verdict.only_in_arm.is_empty()
                        && verdict.only_in_reference.is_empty()
                        && verdict.changed.is_empty()
                    {
                        let _ = writeln!(details, "  ! same variables, different ORDER");
                    }
                    if !verdict.tolerated.is_empty() {
                        let _ = writeln!(
                            details,
                            "  = tolerated as declared residuals: {}",
                            verdict.tolerated.join(", ")
                        );
                    }
                }
                if !raw_notes.is_empty() {
                    let _ = writeln!(details, "\n-- {backend} / rawblock (byte-exact) --");
                    for note in &raw_notes {
                        let _ = writeln!(details, "  # {note}");
                    }
                    if env_verdict.is_parity() && proc_verdict.is_parity() {
                        let _ = writeln!(
                            details,
                            "  ! BOTH ENTRY LISTS MATCH but the block does not. Comparing only \
                             `env` output would have called this arm clean."
                        );
                    }
                }
                json_arms.push(format!(
                    "{{\"arm\":\"{}\",\"status\":\"{}\",\"environ_count\":{},\"environ_bytes\":{},\
                     \"procenv_count\":{},\"procenv_bytes\":{},\"rawblock_bytes\":{},\
                     \"rawblock_notes\":{},\
                     \"environ_only_in_arm\":{},\"environ_only_in_reference\":{},\
                     \"environ_changed\":{},\"environ_order_differs\":{},\
                     \"procenv_only_in_arm\":{},\"procenv_only_in_reference\":{},\
                     \"procenv_changed\":{},\"procenv_order_differs\":{},\"tolerated\":{}}}",
                    json_escape(backend),
                    if parity { "identical" } else { "diverges" },
                    environ.len(),
                    environ_bytes,
                    procenv.len(),
                    procenv_bytes,
                    rawblock.len(),
                    json_list(&raw_notes),
                    json_list(&env_verdict.only_in_arm),
                    json_list(&env_verdict.only_in_reference),
                    json_list(&env_verdict.changed),
                    env_verdict.order_differs,
                    json_list(&proc_verdict.only_in_arm),
                    json_list(&proc_verdict.only_in_reference),
                    json_list(&proc_verdict.changed),
                    proc_verdict.order_differs,
                    json_list(&env_verdict.tolerated),
                ));
            }
        }
    }

    if !details.is_empty() {
        println!("{details}");
    }

    if options.residuals_declared {
        println!("\n-- declared residuals (tolerated by this run) --");
        for residual in DECLARED_RESIDUALS {
            println!("  {} [{}]", residual.name, residual.arm);
            println!("    owner: {}", residual.owner);
            println!("    why  : {}", residual.why);
        }
    }

    // A summary that states what it verified, not just a verdict.
    println!(
        "\nsummary: {} identical, {} diverging, {} unavailable (of {} non-reference arms)",
        identical.len(),
        diverged.len(),
        unavailable.len(),
        arms.len() - 1
    );
    if !identical.is_empty() {
        println!("  identical  : {}", identical.join(", "));
    }
    if !diverged.is_empty() {
        println!("  diverging  : {}", diverged.join(", "));
    }
    if !unavailable.is_empty() {
        println!(
            "  unavailable: {} (NOT counted as parity)",
            unavailable.join(", ")
        );
    }

    if let Some(path) = &options.json {
        let record = format!(
            "{{\"hermit\":\"{}\",\"guest\":\"{}\",\"reference\":\"{}\",\
             \"residuals_declared\":{},\"planted\":{},\"plant_arm\":\"{}\",\
             \"identical\":{},\"diverging\":{},\"unavailable\":{},\"arms\":[{}]}}\n",
            json_escape(&options.hermit.display().to_string()),
            json_escape(&guest.display().to_string()),
            json_escape(&options.reference),
            options.residuals_declared,
            json_list(&options.plant),
            json_escape(&plant_arm),
            json_list(&identical),
            json_list(&diverged),
            json_list(&unavailable),
            json_arms.join(",")
        );
        if let Err(error) = std::fs::write(path, record) {
            fail(&format!("cannot write {}: {error}", path.display()));
        }
        println!("json: {}", path.display());
    }

    match verdict_exit_code(diverged.len(), unavailable.len(), identical.len()) {
        0 => exit(0),
        1 => exit(1),
        _ if !unavailable.is_empty() => {
            eprintln!(
                "\nUNKNOWN: {} requested arm(s) were unavailable. Every requested arm must \
                 produce a measurement before this check can pass.",
                unavailable.len()
            );
            exit(2);
        }
        _ => {
            eprintln!(
                "\nUNKNOWN: no arm was successfully compared against the reference. \
                 Nothing is proven; this is not a pass."
            );
            exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_hex_is_decoded_exactly() {
        assert_eq!(decode_hex("00417fff").unwrap(), [0, 0x41, 0x7f, 0xff]);
    }

    #[test]
    fn malformed_hex_is_a_no_result_instead_of_zero_bytes() {
        assert!(decode_hex("0").is_err());
        assert!(decode_hex("zz").is_err());
    }

    #[test]
    fn requested_unavailable_arm_prevents_green_even_with_a_matching_control() {
        assert_eq!(verdict_exit_code(0, 1, 1), 2);
    }

    #[test]
    fn measured_match_is_green_and_real_divergence_is_red() {
        assert_eq!(verdict_exit_code(0, 0, 1), 0);
        assert_eq!(verdict_exit_code(1, 0, 0), 1);
        assert_eq!(verdict_exit_code(1, 1, 1), 1);
    }
}
