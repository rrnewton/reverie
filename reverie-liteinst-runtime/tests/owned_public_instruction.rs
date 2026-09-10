//! Default-feature regression for the exported public owned-native installer with
//! CPUID/RDTSC subscriptions, plus the host controls that guard its evidence.
//!
//! Two kinds of case live here and they must not be confused:
//!
//! * the **native** case, `public_owned_native_routes_cpuid_and_rdtsc_to_the_shared_tool`,
//!   runs when the host permits its native RPC socket. It checks product selectors
//!   and removes loader-only variables from the child commands.
//!   A guest that exits 127 has failed installation and fails this test; 127 is
//!   never a positive result.
//! * the `hostcontrol_*` cases. Most spawn nothing. A few deliberately re-execute
//!   **this same harness binary** with a test-only case selector, because the
//!   behaviour under test is a process outcome: a destructor running while its
//!   sinks refuse, and the terminal verification chain's exit status. Those child
//!   processes are always the harness, never a product guest or server role and
//!   never the native case. No control touches a product binary.
//!
//! # Child environment
//!
//! `cargo test` injects `LD_LIBRARY_PATH` into this process. The harness leaves the
//! parent environment unchanged and removes loader-only variables from each child
//! command. Product constructor/runtime selectors are still refused. The harness
//! also generates its own `nm`, `objdump`, and `readelf` inputs from the linked
//! guest and retains the exact commands, relevant environment, inputs, and runtime
//! source used by the controls.

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

#[path = "owned_public_instruction/linked_audit.rs"]
mod linked_audit;
#[path = "owned_public_instruction/retention.rs"]
mod retention;
#[path = "../src/bin/owned_public_instruction/selectors.rs"]
mod selectors;
#[path = "../src/bin/owned_public_instruction/verifier.rs"]
mod verifier;

use retention::Retained;

const GUEST: &str = env!("CARGO_BIN_EXE_owned_public_instruction");
const RUNTIME_SOURCE: &str = include_str!("../src/runtime.rs");

struct AuditInputs {
    symbols: String,
    disassembly: String,
    sections: String,
    relocations: String,
}

static AUDIT_INPUTS: OnceLock<AuditInputs> = OnceLock::new();

fn audit_inputs() -> &'static AuditInputs {
    AUDIT_INPUTS.get_or_init(|| {
        let root = retention::root().expect("create retained audit root");
        let directory = root.join("linked-inputs");
        std::fs::create_dir(&directory).expect("create retained linked-input directory");
        let guest = std::path::Path::new(GUEST);
        let path = std::env::var_os("PATH").unwrap_or_default();
        let commands = [
            ("nm", vec!["-nS", "--defined-only"]),
            ("objdump", vec!["-d", "--no-show-raw-insn"]),
            ("readelf-sections", vec!["-SW"]),
            ("readelf-relocations", vec!["-rW"]),
        ];
        let mut invocation = String::new();
        let mut outputs = Vec::new();
        for (label, arguments) in commands {
            let executable = if label.starts_with("readelf") {
                "readelf"
            } else {
                label
            };
            invocation.push_str(executable);
            for argument in &arguments {
                invocation.push(' ');
                invocation.push_str(argument);
            }
            invocation.push(' ');
            invocation.push_str(&guest.display().to_string());
            invocation.push('\n');
            let output = Command::new(executable)
                .env_clear()
                .env("PATH", &path)
                .env("LC_ALL", "C")
                .args(&arguments)
                .arg(guest)
                .output()
                .unwrap_or_else(|error| panic!("run {executable}: {error}"));
            assert!(
                output.status.success(),
                "{label} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            outputs.push(String::from_utf8(output.stdout).expect("tool output must be UTF-8"));
        }
        let environment = format!(
            "env_clear=true\nPATH={}\nLC_ALL=C\n{}={}\nGUEST={}\nRUNTIME_SOURCE=include_str!(../src/runtime.rs)\n",
            path.to_string_lossy(),
            retention::ARTIFACTS_ENV,
            root.display(),
            guest.display(),
        );
        std::fs::write(directory.join("commands.txt"), invocation)
            .expect("retain linked-audit commands");
        std::fs::write(directory.join("environment.txt"), environment)
            .expect("retain linked-audit environment");
        for (name, contents) in [
            ("symbols.txt", &outputs[0]),
            ("disassembly.txt", &outputs[1]),
            ("sections.txt", &outputs[2]),
            ("relocations.txt", &outputs[3]),
        ] {
            std::fs::write(directory.join(name), contents).expect("retain linked-audit input");
        }
        std::fs::write(directory.join("runtime.rs"), RUNTIME_SOURCE)
            .expect("retain linked runtime source");
        AuditInputs {
            symbols: outputs.remove(0),
            disassembly: outputs.remove(0),
            sections: outputs.remove(0),
            relocations: outputs.remove(0),
        }
    })
}

/// Owns the coordinator child and retains what actually happened to it.
///
/// [`Server::shutdown`] performs the original unconditional `kill` then `wait` and
/// records **both** actual results; it never pre-checks liveness in order to skip
/// cleanup. The outcome is cached, so an explicit finalization followed by `Drop`
/// reaps exactly once. A role that was never spawned is reported as such rather
/// than being given an invented status.
///
/// [`Server::finalize`] is the normal-return path and propagates an evidence write
/// failure, so the native case cannot claim success with a missing record. `Drop`
/// is best effort because a destructor must not unwind, and it is the path that
/// survives the panicking cases.
struct Server<'a> {
    child: Option<Child>,
    retained: &'a Retained,
    outcome: Option<String>,
}

impl<'a> Server<'a> {
    fn not_spawned(retained: &'a Retained) -> Self {
        Self {
            child: None,
            retained,
            outcome: None,
        }
    }

    fn shutdown(&mut self) -> String {
        if self.outcome.is_none() {
            self.outcome = Some(match self.child.as_mut() {
                None => "never-spawned\n".to_owned(),
                Some(child) => {
                    let kill = child.kill();
                    let reap = child.wait();
                    format!("kill: {kill:?}\nreap: {reap:?}\n")
                }
            });
        }
        self.outcome.clone().expect("cached above")
    }

    fn finalize(&mut self) -> std::io::Result<()> {
        let text = self.shutdown();
        self.retained.record("server-outcome", &text)
    }
}

impl Drop for Server<'_> {
    fn drop(&mut self) {
        let text = self.shutdown();
        self.retained.record_best_effort("server-outcome", &text);
    }
}

fn loader_names<I, S>(variables: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut names: Vec<String> = variables
        .into_iter()
        .filter_map(|name| {
            let name = name.as_ref();
            (name.starts_with("LD_") || name == "GLIBC_TUNABLES").then(|| name.to_owned())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Refuse a conflicting environment immediately before spawning **this** role, and
/// retain the decision under a role-labelled name so neither role overwrites the
/// other. This refuses; it never unsets or edits a variable.
fn guard_before_spawn(retained: &Retained, role: &str, command: &mut Command) {
    let mut names = selectors::current_names();
    names.extend(
        command
            .get_envs()
            .map(|(name, _)| name.to_string_lossy().into_owned()),
    );
    let refused = selectors::refusals(&names);
    let report = selectors::report(&refused);
    retained
        .record(
            &format!("preflight-selectors-{role}.txt"),
            report.as_deref().unwrap_or("no product selector present\n"),
        )
        .expect("the role guard decision must be retained");
    if let Some(report) = report {
        panic!("{role}: {report}");
    }
    let removed = loader_names(names);
    for name in &removed {
        command.env_remove(name);
    }
    let report = if removed.is_empty() {
        "no loader-only variable removed\n".to_owned()
    } else {
        format!("removed from child command: {}\n", removed.join(", "))
    };
    retained
        .record(&format!("preflight-loader-{role}.txt"), &report)
        .expect("the child loader decision must be retained");
}

fn run(retained: &Retained) -> Output {
    assert_eq!(
        std::fs::symlink_metadata("/etc/ld.so.preload")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    let directory = retained.path().to_path_buf();
    let socket_directory = tempfile::Builder::new()
        .prefix("ropi-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = socket_directory.path().join("s");
    retained
        .record("socket-path", &format!("{}\n", socket.display()))
        .unwrap();

    let mut server = Server::not_spawned(retained);
    let mut server_command = Command::new(GUEST);
    server_command
        .args(["server"])
        .arg(&socket)
        .stdout(Stdio::from(
            std::fs::File::create(directory.join("server.stdout")).unwrap(),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(directory.join("server.stderr")).unwrap(),
        ));
    guard_before_spawn(retained, "server", &mut server_command);
    server.child = Some(server_command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(
            server.child.as_mut().unwrap().try_wait().unwrap().is_none(),
            "RPC server exited; streams and server-outcome retained at {}",
            directory.display()
        );
        assert!(
            Instant::now() < deadline,
            "RPC setup timed out; streams and server-outcome retained at {}",
            directory.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut guest_command = Command::new(GUEST);
    guest_command
        .arg("guest")
        .arg(&socket)
        .stdout(Stdio::from(
            std::fs::File::create(directory.join("guest.stdout")).unwrap(),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(directory.join("guest.stderr")).unwrap(),
        ));
    guard_before_spawn(retained, "guest", &mut guest_command);
    let mut guest = guest_command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = guest.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            guest.kill().unwrap();
            let status = guest.wait().unwrap();
            std::fs::write(directory.join("timeout.status"), status.to_string()).unwrap();
            retained
                .terminal("timeout")
                .expect("timeout must be retained");
            panic!(
                "guest timed out; streams and statuses retained at {}",
                directory.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    std::fs::write(directory.join("status"), status.to_string()).unwrap();
    retained
        .record(
            "child-exit",
            &format!("{status}\nretained at {}\n", directory.display()),
        )
        .expect("the child exit record must be retained");
    let output = Output {
        status,
        stdout: std::fs::read(directory.join("guest.stdout")).unwrap(),
        stderr: std::fs::read(directory.join("guest.stderr")).unwrap(),
    };
    server
        .finalize()
        .expect("the server exit and reap record must be retained");
    output
}

#[test]
fn public_owned_native_routes_cpuid_and_rdtsc_to_the_shared_tool() {
    let retained = Retained::new("native-cpuid-rdtsc").unwrap();
    let output = run(&retained);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("events=6 injections=3"), "{text}");
    assert!(text.contains("kinds=[1, 2, 3, 4, 5, 1]"), "{text}");
    assert!(text.contains("clocks=[0, 4, 8, 12, 16, 20]"), "{text}");
    assert!(
        text.contains(
            "instructions=[4440, 22222222, 33333333, 44444444, 3, 22222222, 33333333, fedcba98, 4, 22222222, aaaa5555, fedcba98]"
        ),
        "{text}"
    );
    retained.finish("ok").unwrap();
}

/// Proves the coordinator handle retains a real exit and a real reap.
///
/// The child is an ordinary `/bin/true`: no product binary, no guest role and no
/// instrumentation. Its exit is observed first, so the expected reap text is
/// formatted from the actual [`std::process::ExitStatus`] rather than a literal.
/// Finalizing twice and then dropping must reuse the cached outcome, so no second
/// reap and no invented status can appear. The program's bytes are compared before
/// and after to show the non-instrumented child was not modified.
#[test]
fn hostcontrol_server_shutdown_retains_actual_exit_and_reap() {
    const PROGRAM: &str = "/bin/true";
    let retained = Retained::new("hostcontrol-server-shutdown").unwrap();
    let root = retained.path().to_path_buf();
    let program_before = std::fs::read(PROGRAM).unwrap();

    let absent = Retained::under(&root, "never-spawned").unwrap();
    let mut idle = Server::not_spawned(&absent);
    idle.finalize().unwrap();
    drop(idle);
    assert_eq!(
        std::fs::read_to_string(root.join("never-spawned/server-outcome")).unwrap(),
        "never-spawned\n"
    );

    let ordinary = Retained::under(&root, "ordinary").unwrap();
    let mut child = Command::new(PROGRAM)
        .stdout(Stdio::from(
            std::fs::File::create(ordinary.join("true.stdout")).unwrap(),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(ordinary.join("true.stderr")).unwrap(),
        ))
        .spawn()
        .unwrap();
    let observed = child.wait().unwrap();
    assert!(observed.success(), "{observed:?}");

    let mut live = Server::not_spawned(&ordinary);
    live.child = Some(child);
    live.finalize().unwrap();
    let recorded = std::fs::read_to_string(root.join("ordinary/server-outcome")).unwrap();
    assert!(recorded.contains("kill: "), "{recorded}");
    assert!(
        recorded.contains(&format!("reap: Ok({observed:?})")),
        "recorded {recorded:?} must carry the actual status {observed:?}"
    );

    live.finalize().unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("ordinary/server-outcome")).unwrap(),
        recorded,
        "a repeated finalization must reuse the cached outcome"
    );
    drop(live);
    assert_eq!(
        std::fs::read_to_string(root.join("ordinary/server-outcome")).unwrap(),
        recorded,
        "drop after an explicit finalization must not reap or relabel again"
    );

    assert!(root.join("ordinary/true.stdout").is_file());
    assert!(root.join("ordinary/true.stderr").is_file());
    assert_eq!(
        std::fs::read(PROGRAM).unwrap(),
        program_before,
        "the non-instrumented program must be unmodified"
    );
    retained
        .record(
            "program.txt",
            &format!("{PROGRAM}\nbytes: {}\n", program_before.len()),
        )
        .unwrap();
    retained.finish("ok").unwrap();
}

fn bound_body() -> (Vec<linked_audit::Symbol>, Vec<linked_audit::Instruction>) {
    let inputs = audit_inputs();
    (
        linked_audit::parse_symbols(&inputs.symbols),
        linked_audit::parse_disassembly(&inputs.disassembly),
    )
}

fn public_first(symbols: &[linked_audit::Symbol]) -> u64 {
    symbols
        .iter()
        .find(|symbol| symbol.name == "public_first")
        .expect("public_first must be defined")
        .address
}

#[test]
fn hostcontrol_linked_audit_accepts_the_bound_public_entry_body() {
    let retained = Retained::new("hostcontrol-linked-audit").unwrap();
    let (symbols, disassembly) = bound_body();
    let report = linked_audit::audit(&symbols, &disassembly)
        .unwrap_or_else(|error| panic!("the built guest body must be accepted: {error}"));
    assert!(report.entry < report.first && report.first < report.end);
    assert_eq!(report.loops, 5);
    retained
        .record("report.txt", &format!("{report:#?}\n"))
        .unwrap();
    retained.finish("ok").unwrap();
}

/// Reproduces the superseded defect: stopping at the nested `public_first` label,
/// which is where the old blank-line extraction stopped. The old excerpt contained
/// no `syscall` at all and still passed; this must reject.
#[test]
fn hostcontrol_linked_audit_rejects_a_body_truncated_at_the_nested_label() {
    let retained = Retained::new("hostcontrol-linked-audit-truncated").unwrap();
    let (symbols, disassembly) = bound_body();
    let boundary = public_first(&symbols);
    let truncated: Vec<_> = disassembly
        .iter()
        .filter(|insn| insn.address < boundary)
        .cloned()
        .collect();
    assert!(
        !truncated.iter().any(|insn| insn.mnemonic == "syscall"),
        "the truncated excerpt must reproduce the superseded no-syscall symptom"
    );
    let error =
        linked_audit::audit(&symbols, &truncated).expect_err("a truncated body must be rejected");
    assert!(error.contains("truncated"), "{error}");
    retained
        .record("rejection.txt", &format!("{error}\n"))
        .unwrap();
    retained.finish("ok").unwrap();
}

#[test]
fn hostcontrol_linked_audit_rejects_a_missing_terminal_endpoint() {
    let retained = Retained::new("hostcontrol-linked-audit-endpoint").unwrap();
    let (symbols, disassembly) = bound_body();
    let without_terminal: Vec<_> = disassembly
        .iter()
        .filter(|insn| insn.mnemonic != "ud2")
        .cloned()
        .collect();
    assert!(disassembly.len() > without_terminal.len(), "no ud2 removed");
    let error = linked_audit::audit(&symbols, &without_terminal)
        .expect_err("a missing terminal endpoint must be rejected");
    assert!(error.contains("terminal ud2"), "{error}");
    retained
        .record("rejection.txt", &format!("{error}\n"))
        .unwrap();
    retained.finish("ok").unwrap();
}

#[test]
fn hostcontrol_linked_audit_rejects_a_call_between_the_enable_and_the_guest() {
    let retained = Retained::new("hostcontrol-linked-audit-intervening").unwrap();
    let (symbols, disassembly) = bound_body();
    let boundary = public_first(&symbols);
    let victim = disassembly
        .iter()
        .filter(|insn| insn.address < boundary)
        .map(|insn| insn.address)
        .max()
        .expect("an instruction must precede public_first");
    let injected: Vec<_> = disassembly
        .iter()
        .cloned()
        .map(|mut insn| {
            if insn.address == victim {
                insn.mnemonic = "call".to_owned();
                insn.operands = "0x0 <injected_intervening_call>".to_owned();
            }
            insn
        })
        .collect();
    let error =
        linked_audit::audit(&symbols, &injected).expect_err("an intervening call must be rejected");
    assert!(error.contains("between the physical enable"), "{error}");
    retained
        .record("rejection.txt", &format!("{error}\n"))
        .unwrap();
    retained.finish("ok").unwrap();
}

/// Binds the default-feature constructor from the ELF itself, not from a comment,
/// and retains the legacy-selector refusal and unselected no-op path it relies on.
#[test]
fn hostcontrol_default_constructor_is_bound_by_the_linked_init_array() {
    let retained = Retained::new("hostcontrol-constructor").unwrap();
    let inputs = audit_inputs();
    let symbols = linked_audit::parse_symbols(&inputs.symbols);
    let sections = linked_audit::parse_sections(&inputs.sections);
    let relocations = linked_audit::parse_relative_relocations(&inputs.relocations);
    let binding = linked_audit::constructor_binding(&symbols, &sections, &relocations)
        .unwrap_or_else(|error| panic!("the default binary must link the constructor: {error}"));
    assert_eq!(binding.installs, binding.initialize);
    retained
        .record("binding.txt", &format!("{binding:#?}\n"))
        .unwrap();

    let start = RUNTIME_SOURCE
        .find("pub(crate) fn initialize_from_environment()")
        .expect("initialize_from_environment must exist in the pinned runtime source");
    let excerpt: String = RUNTIME_SOURCE[start..]
        .lines()
        .take(20)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        excerpt.contains("REVERIE_LITEINST_TOOL"),
        "the removed selector refusal must be visible: {excerpt}"
    );
    assert!(
        excerpt.contains("io::ErrorKind::Unsupported"),
        "the legacy selector must be refused: {excerpt}"
    );
    retained
        .record("unselected-branch.rs", &format!("{excerpt}\n"))
        .unwrap();
    retained.finish("ok").unwrap();
}

/// Drives the real retention helper through success, a real unwind and a silent
/// drop, then contrasts it with the `TempDir` behaviour it replaces. Spawns nothing.
#[test]
fn hostcontrol_retention_keeps_streams_through_success_and_unwind() {
    let outer = Retained::new("hostcontrol-retention").unwrap();
    let root = outer.path().to_path_buf();

    let success = Retained::under(&root, "success").unwrap();
    std::fs::write(success.join("guest.stdout"), b"ok-bytes").unwrap();
    success.finish("ok").unwrap();
    drop(success);
    assert_eq!(
        std::fs::read(root.join("success/guest.stdout")).unwrap(),
        b"ok-bytes"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("success/outcome")).unwrap(),
        "ok"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("success/unwind")).unwrap(),
        "completed"
    );
    assert!(
        std::fs::read_to_string(root.join("success/location"))
            .unwrap()
            .contains("success"),
        "the location must be reported on every outcome"
    );

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let inner = Retained::under(&root, "unwound").unwrap();
        std::fs::write(inner.join("guest.stderr"), b"partial-bytes").unwrap();
        panic!("simulated assertion failure inside the retained scope");
    }));
    std::panic::set_hook(previous);
    assert!(unwound.is_err(), "the control must actually unwind");
    assert_eq!(
        std::fs::read(root.join("unwound/guest.stderr")).unwrap(),
        b"partial-bytes",
        "streams must survive the unwind that claims to retain them"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("unwound/outcome")).unwrap(),
        "panicked"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("unwound/unwind")).unwrap(),
        "panicked"
    );

    drop(Retained::under(&root, "dropped").unwrap());
    assert_eq!(
        std::fs::read_to_string(root.join("dropped/outcome")).unwrap(),
        "dropped-without-outcome"
    );

    assert!(
        Retained::under(&root, "success").is_err(),
        "a rerun must not silently overwrite retained evidence"
    );

    let temporary = tempfile::tempdir().unwrap();
    let deleted = temporary.path().to_path_buf();
    std::fs::write(deleted.join("guest.stdout"), b"x").unwrap();
    drop(temporary);
    assert!(
        !deleted.exists(),
        "TempDir deletes on drop; that is the superseded behaviour being replaced"
    );

    outer.finish("ok").unwrap();
}

/// A selected terminal cause must survive the unwind that follows it, and the
/// unwind must be recorded separately rather than relabelling the cause. This is
/// the timeout path's contract, exercised without a guest.
#[test]
fn hostcontrol_retention_terminal_cause_survives_a_later_panic() {
    let outer = Retained::new("hostcontrol-retention-terminal").unwrap();
    let root = outer.path().to_path_buf();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let inner = Retained::under(&root, "timed-out").unwrap();
        std::fs::write(inner.join("timeout.status"), "signal: 9").unwrap();
        inner.terminal("timeout").unwrap();
        assert!(inner.has_terminal_cause());
        panic!("guest timed out");
    }));
    std::panic::set_hook(previous);
    assert!(unwound.is_err());
    assert_eq!(
        std::fs::read_to_string(root.join("timed-out/outcome")).unwrap(),
        "timeout",
        "the precise terminal cause must not be replaced by a generic unwind label"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("timed-out/unwind")).unwrap(),
        "panicked",
        "the unwind must still be recorded, separately"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("timed-out/timeout.status")).unwrap(),
        "signal: 9"
    );
    outer.finish("ok").unwrap();
}

/// Ordinary evidence writes must propagate failure, and a case must not claim
/// success when its outcome record could not be written.
///
/// The refusal is a deterministic destination collision, not an ambient permission
/// assumption: a directory is placed at each target path, so `std::fs::write` fails
/// with `EISDIR` regardless of the caller's privileges, including root. An earlier
/// revision made the case *directory* read-only, which could not block rewriting an
/// `outcome` file that already existed, so the negative half of this control did not
/// actually fail; that defect was masked by a Clippy error on the restore call.
///
/// Nothing is deleted and no permission is changed: the initial record and the
/// blocking directory are both renamed aside and retained.
#[test]
fn hostcontrol_retention_propagates_record_and_finalization_failure() {
    const EISDIR: i32 = 21;
    let outer = Retained::new("hostcontrol-retention-failure").unwrap();
    let root = outer.path().to_path_buf();
    let blocked = Retained::under(&root, "blocked").unwrap();
    let directory = blocked.path().to_path_buf();

    let outcome = directory.join("outcome");
    std::fs::rename(&outcome, directory.join("outcome.initial")).unwrap();
    assert_eq!(
        std::fs::read_to_string(directory.join("outcome.initial")).unwrap(),
        "started"
    );
    std::fs::create_dir(&outcome).unwrap();
    std::fs::create_dir(directory.join("write-refused")).unwrap();

    let recorded = blocked
        .record("write-refused", "value")
        .expect_err("record must propagate the write failure");
    assert_eq!(
        recorded.raw_os_error(),
        Some(EISDIR),
        "expected EISDIR, got {recorded:?}"
    );
    let finished = blocked
        .finish("ok")
        .expect_err("finish must propagate the write failure");
    assert_eq!(
        finished.raw_os_error(),
        Some(EISDIR),
        "expected EISDIR, got {finished:?}"
    );
    assert!(
        !blocked.has_terminal_cause(),
        "a failed finalization must not claim a terminal outcome"
    );
    outer
        .record(
            "refusals.txt",
            &format!("record: {recorded:?}\nfinish: {finished:?}\n"),
        )
        .unwrap();

    std::fs::rename(&outcome, directory.join("outcome.blocking-directory")).unwrap();
    blocked
        .finish("ok")
        .expect("finish must succeed once the destination is no longer a directory");
    assert!(blocked.has_terminal_cause());
    drop(blocked);
    assert_eq!(std::fs::read_to_string(&outcome).unwrap(), "ok");
    assert!(
        outcome.is_file(),
        "the recovered outcome must be a regular file"
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("outcome.initial")).unwrap(),
        "started",
        "the initial record must stay retained"
    );
    assert!(directory.join("outcome.blocking-directory").is_dir());
    assert!(directory.join("write-refused").is_dir());
    outer.finish("ok").unwrap();
}

/// Selects a child case for
/// [`hostcontrol_destructor_survives_artifact_and_stderr_failure`]. Test-only; no
/// product code reads it.
const DROP_CASE_ENV: &str = "OWNED_PUBLIC_INSTRUCTION_DROP_CASE";

/// Selects a child case for
/// [`hostcontrol_verifier_terminal_chain_exits_with_the_real_outcome`]. Test-only.
const TERMINAL_CASE_ENV: &str = "OWNED_PUBLIC_INSTRUCTION_TERMINAL_CASE";

/// Names the file the child should use as the terminal chain's output sink.
const TERMINAL_OUT_ENV: &str = "OWNED_PUBLIC_INSTRUCTION_TERMINAL_OUT";

/// The terminal chain is a process outcome, so it is exercised as one: the child
/// builds a controlled observation and calls the **same** `verifier::terminate_on`
/// the guest calls, which checks, emits its bounded record through the raw gate and
/// exits. Nothing here copies the terminal logic and nothing mocks a sink.
///
/// Four cases, each an actual process result:
///
/// * a good observation writes the unchanged success record and exits 0;
/// * the exact rev19 failure, `results[7] == 0`, names the rejecting check with its
///   actual and expected values on stderr and exits 70, with no success record, no
///   panic and no cascade;
/// * a good observation whose output sink is the real `/dev/full` must not be
///   called a success: it exits 71 with its own output-failure identity;
/// * both sinks refusing still exits 71 without panicking or hanging, even though
///   no bytes can be retained.
#[test]
fn hostcontrol_verifier_terminal_chain_exits_with_the_real_outcome() {
    if let Some(case) = std::env::var_os(TERMINAL_CASE_ENV) {
        terminal_child(&case.to_string_lossy())
    }
    let retained = Retained::new("hostcontrol-verifier-terminal").unwrap();
    let harness = std::env::current_exe().unwrap();

    for (case, expected_status) in [
        ("good", 0),
        ("bad-rdtsc-rcx", verifier::FAILURE_STATUS),
        ("bad-rdtsc-rcx-unwritable-stderr", verifier::FAILURE_STATUS),
        ("unwritable-output", verifier::OUTPUT_FAILURE_STATUS),
        ("both-sinks-refuse", verifier::OUTPUT_FAILURE_STATUS),
    ] {
        let sink = retained.join(&format!("{case}.sink"));
        let stdout_path = retained.join(&format!("{case}.stdout"));
        let stderr_path = retained.join(&format!("{case}.stderr"));
        let mut child = Command::new(&harness)
            .args([
                "--exact",
                "hostcontrol_verifier_terminal_chain_exits_with_the_real_outcome",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(TERMINAL_CASE_ENV, case)
            .env(TERMINAL_OUT_ENV, &sink)
            .env(retention::ARTIFACTS_ENV, retained.path())
            .stdout(Stdio::from(std::fs::File::create(&stdout_path).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(&stderr_path).unwrap()))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let killed = child.wait().unwrap();
                retained
                    .record(&format!("{case}.timeout"), &format!("{killed}\n"))
                    .unwrap();
                panic!(
                    "{case} child timed out; retained at {}",
                    retained.path().display()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        retained
            .record(&format!("{case}.status"), &format!("{status}\n"))
            .unwrap();
        assert_eq!(
            status.code(),
            Some(expected_status as i32),
            "{case}: unexpected terminal status {status}"
        );
        let stderr = std::fs::read_to_string(&stderr_path)
            .unwrap_or_else(|error| panic!("{case}: retained stderr must be readable: {error}"));
        assert!(
            !stderr.contains("panicked"),
            "{case}: the terminal chain must not panic: {stderr}"
        );
        // Every child creates its sink, including the refusal cases, so a missing or
        // unreadable file is a lost-evidence failure rather than an empty result.
        let sink_text = std::fs::read_to_string(&sink)
            .unwrap_or_else(|error| panic!("{case}: retained sink must be readable: {error}"));
        match case {
            "good" => {
                assert_eq!(sink_text, EXPECTED_SUCCESS_RECORD, "{case}");
                assert!(stderr.is_empty(), "{case}: {stderr}");
            }
            "bad-rdtsc-rcx" => {
                assert!(sink_text.is_empty(), "{case}: no success record may appear");
                assert_eq!(stderr, EXPECTED_BAD_RDTSC_RCX_RECORD, "{case}");
            }
            "bad-rdtsc-rcx-unwritable-stderr" => {
                assert!(sink_text.is_empty(), "{case}: no success record may appear");
                assert!(
                    stderr.is_empty(),
                    "{case}: a refusing diagnostic sink retains nothing, and must not be \
                     reported as if it had: {stderr}"
                );
            }
            "unwritable-output" => {
                assert!(sink_text.is_empty(), "{case}: the sink refused every byte");
                assert!(
                    stderr.contains("check=success-record-output"),
                    "{case}: an unwritten success record must have its own identity: {stderr}"
                );
            }
            _ => {
                assert!(sink_text.is_empty(), "{case}: the sink refused every byte");
                assert!(stderr.is_empty(), "{case}: {stderr}");
            }
        }
    }
    retained.finish("ok").unwrap();
}

const EXPECTED_SUCCESS_RECORD: &str = "owned-public: events=6 injections=3 inventory=0 kinds=[1, 2, 3, 4, 5, 1] clocks=[0, 4, 8, 12, 16, 20] instructions=[4440, 22222222, 33333333, 44444444, 3, 22222222, 33333333, fedcba98, 4, 22222222, aaaa5555, fedcba98]\n";

/// The exact record the terminal chain must emit for the rev19 failure: the typed
/// vector with zero at index 6 against the unchanged expectation of `0x33333333`.
/// Written as an independent literal, not derived from the verifier's constants.
const EXPECTED_BAD_RDTSC_RCX_RECORD: &str = "owned-public FAILED: check=typed-instruction-results actual=[17472, 572662306, 858993459, 1145324612, 3, 572662306, 0, 4275878552, 4, 572662306, 2863289685, 4275878552] expected=[17472, 572662306, 858993459, 1145324612, 3, 572662306, 858993459, 4275878552, 4, 572662306, 2863289685, 4275878552]\n";

fn terminal_child(case: &str) -> ! {
    use std::os::fd::AsRawFd;
    let mut observation = passing_observation();
    if case.starts_with("bad-rdtsc-rcx") {
        observation.results[7] = 0;
    }
    let sink = std::env::var_os(TERMINAL_OUT_ENV).expect("the child needs an output sink");
    let file = std::fs::File::create(Path::new(&sink)).unwrap();
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let (out_fd, err_fd) = match case {
        "unwritable-output" => (full.as_raw_fd() as u64, 2),
        "both-sinks-refuse" => (full.as_raw_fd() as u64, full.as_raw_fd() as u64),
        "bad-rdtsc-rcx-unwritable-stderr" => (file.as_raw_fd() as u64, full.as_raw_fd() as u64),
        _ => (file.as_raw_fd() as u64, 2),
    };
    verifier::terminate_on(&observation, out_fd, err_fd)
}

/// The linked body must seed ECX immediately before `rdtsc`, because `RDTSC` does
/// not write ECX and the `.Lwork_b` loop leaves it zero. Removing the seed
/// reproduces the rev19 native failure, in which `RESULTS[7]` was 0 instead of the
/// sentinel. Counts alone cannot see this, so the audit checks liveness.
#[test]
fn hostcontrol_linked_audit_rejects_the_unseeded_rdtsc_body() {
    let retained = Retained::new("hostcontrol-linked-audit-unseeded").unwrap();
    let (symbols, disassembly) = bound_body();
    let accepted = linked_audit::audit(&symbols, &disassembly).expect("the seeded body");
    let unseeded: Vec<_> = disassembly
        .iter()
        .filter(|insn| insn.address != accepted.rdtsc_seed)
        .cloned()
        .collect();
    assert_eq!(unseeded.len() + 1, disassembly.len(), "no seed removed");
    let error = linked_audit::audit(&symbols, &unseeded)
        .expect_err("the original unseeded body must be rejected");
    assert!(error.contains("RDTSC ECX liveness"), "{error}");
    retained
        .record("rejection.txt", &format!("{error}\n"))
        .unwrap();
    retained.finish("ok").unwrap();
}

/// A write to ECX between the seed and `rdtsc` destroys the sentinel just as the
/// missing seed did, so the audit must reject that too.
#[test]
fn hostcontrol_linked_audit_rejects_a_clobber_between_the_seed_and_rdtsc() {
    let retained = Retained::new("hostcontrol-linked-audit-clobber").unwrap();
    let (symbols, disassembly) = bound_body();
    let accepted = linked_audit::audit(&symbols, &disassembly).expect("the seeded body");
    let mut clobbered = disassembly.clone();
    let position = clobbered
        .iter()
        .position(|insn| insn.address == accepted.rdtsc_seed)
        .expect("the seed must be present");
    clobbered.insert(
        position + 1,
        linked_audit::Instruction {
            address: accepted.rdtsc_seed + 1,
            mnemonic: "xor".to_owned(),
            operands: "%ecx,%ecx".to_owned(),
        },
    );
    let error = linked_audit::audit(&symbols, &clobbered)
        .expect_err("a clobber between the seed and rdtsc must be rejected");
    assert!(error.contains("RDTSC ECX liveness"), "{error}");
    retained
        .record("rejection.txt", &format!("{error}\n"))
        .unwrap();
    retained.finish("ok").unwrap();
}

fn passing_observation() -> verifier::Observation {
    verifier::Observation {
        starts: 1,
        events: verifier::EVENTS_EXPECTED,
        injections: 3,
        kinds: [
            verifier::KIND_GETPID,
            verifier::KIND_CPUID,
            verifier::KIND_RDTSC,
            verifier::KIND_RDTSCP,
            verifier::KIND_READ,
            verifier::KIND_GETPID,
            u64::MAX,
            u64::MAX,
        ],
        clocks: [0, 4, 8, 12, 16, 20, u64::MAX, u64::MAX],
        results: [
            4242,
            verifier::EXPECTED_TYPED[0],
            verifier::EXPECTED_TYPED[1],
            verifier::EXPECTED_TYPED[2],
            verifier::EXPECTED_TYPED[3],
            verifier::EXPECTED_TYPED[4],
            verifier::EXPECTED_TYPED[5],
            verifier::EXPECTED_TYPED[6],
            verifier::EXPECTED_TYPED[7],
            verifier::EXPECTED_TYPED[8],
            verifier::EXPECTED_TYPED[9],
            verifier::EXPECTED_TYPED[10],
            verifier::EXPECTED_TYPED[11],
            verifier::READ_BYTES as u64,
            4242,
        ],
        buffer: [0x5a; 16],
        stats: [0, 0, 0, 0],
        inventory: 0,
        pid: 4242,
    }
}

/// The pure verifier must accept a correct observation and produce the unchanged
/// success record, and must reject the exact register value the rev19 native run
/// actually produced, naming the check with its actual and expected values.
#[test]
fn hostcontrol_verifier_accepts_the_golden_and_rejects_the_retained_rdtsc_rcx() {
    let retained = Retained::new("hostcontrol-verifier").unwrap();
    let good = passing_observation();
    let mut detail = verifier::Detail::default();
    verifier::check(&good, &mut detail).unwrap_or_else(|rejection| {
        panic!(
            "golden observation must pass: {rejection} {}",
            String::from_utf8_lossy(detail.as_bytes())
        )
    });
    let success = verifier::success_line(&good);
    assert!(!success.truncated(), "the success record must fit");
    let text = String::from_utf8(success.as_bytes().to_vec()).unwrap();
    assert_eq!(
        text,
        "owned-public: events=6 injections=3 inventory=0 kinds=[1, 2, 3, 4, 5, 1] clocks=[0, 4, 8, 12, 16, 20] instructions=[4440, 22222222, 33333333, 44444444, 3, 22222222, 33333333, fedcba98, 4, 22222222, aaaa5555, fedcba98]\n"
    );
    retained.record("success-golden.txt", &text).unwrap();

    let mut retained_failure = good;
    retained_failure.results[7] = 0;
    let mut rejected_detail = verifier::Detail::default();
    let rejection = verifier::check(&retained_failure, &mut rejected_detail)
        .expect_err("the rev19 zero RDTSC RCX must be rejected");
    assert_eq!(rejection, "typed-instruction-results");
    let line = String::from_utf8(
        verifier::failure_line(rejection, &rejected_detail)
            .as_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(line.contains("check=typed-instruction-results"), "{line}");
    assert!(line.contains("actual="), "{line}");
    assert!(line.contains("expected="), "{line}");
    assert!(
        line.contains(&format!("{}", verifier::RDTSC_ECX_SEED)),
        "{line}"
    );
    retained.record("rejection.txt", &line).unwrap();
    retained.finish("ok").unwrap();
}

/// Every strict check must reject on its own, element by element where that is
/// cheap, so none of them is decorative. Also proves the report cannot truncate at
/// its widest, and that the bounded sink reports truncation when it does occur.
#[test]
fn hostcontrol_verifier_rejects_each_strict_check_in_order() {
    let retained = Retained::new("hostcontrol-verifier-strict").unwrap();
    let mut rejected_names = Vec::new();

    let mut expect_rejection =
        |label: String, expected_check: &'static str, observation: verifier::Observation| {
            let mut detail = verifier::Detail::default();
            let rejection = verifier::check(&observation, &mut detail)
                .expect_err("the mutated observation must be rejected");
            assert_eq!(rejection, expected_check, "{label}");
            assert!(!detail.truncated(), "{label}: detail must not truncate");
            rejected_names.push(format!("{label} -> {rejection}"));
        };

    for (label, expected_check, mutate) in SCALAR_MUTATIONS {
        let mut observation = passing_observation();
        mutate(&mut observation);
        expect_rejection(label.to_owned(), expected_check, observation);
    }
    for index in 0..6 {
        let mut observation = passing_observation();
        observation.kinds[index] = observation.kinds[index].wrapping_add(1);
        expect_rejection(format!("kinds[{index}]"), "event-kinds", observation);
    }
    for index in 0..6 {
        let mut observation = passing_observation();
        observation.clocks[index] = observation.clocks[index].wrapping_add(1);
        expect_rejection(
            format!("clocks[{index}]"),
            "full-owned-clock-sequence",
            observation,
        );
    }
    for index in 0..12 {
        let mut observation = passing_observation();
        observation.results[1 + index] = observation.results[1 + index].wrapping_add(1);
        expect_rejection(
            format!("typed[{index}]"),
            "typed-instruction-results",
            observation,
        );
    }
    for index in 0..verifier::READ_BYTES {
        let mut observation = passing_observation();
        observation.buffer[index] ^= 0xff;
        expect_rejection(format!("buffer[{index}]"), "read-buffer-bytes", observation);
    }
    for index in 0..4 {
        let mut observation = passing_observation();
        observation.stats[index] = 1;
        expect_rejection(
            format!("stats[{index}]"),
            "patch-and-rewrite-stats",
            observation,
        );
    }
    assert_eq!(
        rejected_names.len(),
        SCALAR_MUTATIONS.len() + 6 + 6 + 12 + verifier::READ_BYTES + 4
    );
    retained
        .record("rejected-checks.txt", &rejected_names.join("\n"))
        .unwrap();

    // The superseded form set every result slot to u64::MAX, which also invalidated
    // the read return length, so `check` rejected at `read-return-length` with a
    // 39-byte detail and never reached the twelve-element comparison this control
    // claims to bound. Keep every previously valid field, especially the read
    // length, and widen only the twelve typed values.
    let mut widest = passing_observation();
    for slot in widest.results[1..13].iter_mut() {
        *slot = u64::MAX;
    }
    let mut widest_detail = verifier::Detail::default();
    let widest_rejection = verifier::check(&widest, &mut widest_detail)
        .expect_err("the maximum-width typed observation must be rejected");
    assert_eq!(
        widest_rejection, "typed-instruction-results",
        "the widest control must reach the typed-result comparison, not a scalar check"
    );
    let widest_detail_text = String::from_utf8(widest_detail.as_bytes().to_vec()).unwrap();
    assert_eq!(widest_detail_text, WIDEST_DETAIL_GOLDEN);
    assert_eq!(widest_detail_text.len(), 397);
    assert!(
        !widest_detail.truncated(),
        "the widest detail must fit its static bound"
    );
    let widest_line = verifier::failure_line(widest_rejection, &widest_detail);
    let widest_record_text = String::from_utf8(widest_line.as_bytes().to_vec()).unwrap();
    assert_eq!(widest_record_text, WIDEST_RECORD_GOLDEN);
    assert_eq!(widest_record_text.len(), 451);
    assert!(
        !widest_line.truncated(),
        "the widest record must fit its static bound"
    );
    retained
        .record("widest-detail.txt", &widest_detail_text)
        .unwrap();
    retained
        .record("widest-record.txt", &widest_record_text)
        .unwrap();
    retained
        .record(
            "widest-metrics.txt",
            &format!(
                "check={widest_rejection}\ndetail_bytes={}\nrecord_bytes={}\ndetail_truncated={}\nrecord_truncated={}\n",
                widest_detail_text.len(),
                widest_record_text.len(),
                widest_detail.truncated(),
                widest_line.truncated()
            ),
        )
        .unwrap();

    let mut overflowing = verifier::Bounded::<8>::default();
    use core::fmt::Write;
    let _ = write!(overflowing, "0123456789abcdef");
    assert!(
        overflowing.truncated(),
        "the bounded sink must report truncation"
    );
    assert_eq!(overflowing.as_bytes().len(), 8);
    retained.finish("ok").unwrap();
}

/// The exact maximum-width detail: twelve `u64::MAX` actual values against the
/// twelve unchanged expected values. An independent literal, not derived from the
/// verifier's own constants.
const WIDEST_DETAIL_GOLDEN: &str = "actual=[18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615] expected=[17472, 572662306, 858993459, 1145324612, 3, 572662306, 858993459, 4275878552, 4, 572662306, 2863289685, 4275878552]";

/// The exact whole record built from that detail.
const WIDEST_RECORD_GOLDEN: &str = "owned-public FAILED: check=typed-instruction-results actual=[18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615, 18446744073709551615] expected=[17472, 572662306, 858993459, 1145324612, 3, 572662306, 858993459, 4275878552, 4, 572662306, 2863289685, 4275878552]\n";

type ScalarMutation = (&'static str, &'static str, fn(&mut verifier::Observation));

const SCALAR_MUTATIONS: [ScalarMutation; 6] = [
    ("starts", "thread-start-count", |observation| {
        observation.starts = 2
    }),
    ("events", "event-count", |observation| {
        observation.events = 5
    }),
    ("injections", "injection-count", |observation| {
        observation.injections = 2
    }),
    ("first-pid", "first-getpid-result", |observation| {
        observation.results[0] = 7
    }),
    ("second-pid", "second-getpid-result", |observation| {
        observation.results[14] = 7
    }),
    ("read-length", "read-return-length", |observation| {
        observation.results[13] = 15
    }),
];

/// Drives the real retention helper with **both** sinks refusing at once: a
/// directory at each record path gives a deterministic `EISDIR`, and the process's
/// stderr is the real `/dev/full`.
///
/// libtest capture would hide the stderr path, so the failing-stderr work runs in a
/// child: this control re-executes the same prebuilt harness with `--exact` on its
/// own name and a test-only case selector. The child is the harness binary, never a
/// product guest or server role and never the native case. The child first proves
/// the sink really refuses, so a pass cannot come from a stderr that silently
/// worked. Dropping during a real primary panic must not raise a second panic and
/// must leave the primary payload intact.
#[test]
fn hostcontrol_destructor_survives_artifact_and_stderr_failure() {
    if let Some(case) = std::env::var_os(DROP_CASE_ENV) {
        return destructor_child(&case.to_string_lossy());
    }
    let retained = Retained::new("hostcontrol-destructor-failure").unwrap();
    let child_root = retained.join("child-root");
    std::fs::create_dir(&child_root).unwrap();
    let harness = std::env::current_exe().unwrap();

    for case in ["ordinary-drop", "panicking-drop"] {
        let stdout_path = retained.join(&format!("{case}.stdout"));
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let mut child = Command::new(&harness)
            .args([
                "--exact",
                "hostcontrol_destructor_survives_artifact_and_stderr_failure",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(DROP_CASE_ENV, case)
            .env(retention::ARTIFACTS_ENV, &child_root)
            .stdout(Stdio::from(std::fs::File::create(&stdout_path).unwrap()))
            .stderr(Stdio::from(full))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let killed = child.wait().unwrap();
                retained
                    .record(&format!("{case}.timeout"), &format!("{killed}\n"))
                    .unwrap();
                panic!(
                    "{case} child timed out; retained at {}",
                    retained.path().display()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let text = std::fs::read_to_string(&stdout_path).unwrap();
        retained
            .record(&format!("{case}.status"), &format!("{status}\n"))
            .unwrap();
        assert_eq!(
            status.code(),
            Some(0),
            "{case}: child must exit 0, not panic or abort; status {status}, stdout {text}"
        );
        assert!(
            text.contains("stderr-write-refused=true"),
            "{case}: stderr must really refuse writes: {text}"
        );
        assert!(
            text.contains("sinks-are-directories=true"),
            "{case}: both record sinks must really be directories: {text}"
        );
        match case {
            "ordinary-drop" => assert!(
                text.contains("survived-ordinary-drop=true"),
                "{case}: {text}"
            ),
            _ => assert!(
                text.contains("primary-payload-preserved=true"),
                "{case}: {text}"
            ),
        }
        assert!(child_root.join(case).join("location").is_file());
        assert!(child_root.join(case).join("unwind").is_dir());
        assert!(child_root.join(case).join("outcome").is_dir());
    }
    retained.finish("ok").unwrap();
}

fn destructor_child(case: &str) {
    use std::io::Write;
    let retained = Retained::new(case).unwrap();
    let directory = retained.path().to_path_buf();
    std::fs::rename(directory.join("outcome"), directory.join("outcome.initial")).unwrap();
    std::fs::create_dir(directory.join("outcome")).unwrap();
    std::fs::create_dir(directory.join("unwind")).unwrap();
    println!(
        "sinks-are-directories={}",
        directory.join("outcome").is_dir() && directory.join("unwind").is_dir()
    );
    let refused = {
        let mut stderr = std::io::stderr().lock();
        writeln!(stderr, "destructor-child stderr probe").is_err()
    };
    println!("stderr-write-refused={refused}");
    println!("destination={}", directory.display());

    if case == "ordinary-drop" {
        drop(retained);
        println!("survived-ordinary-drop=true");
        return;
    }

    const PAYLOAD: &str = "primary failure payload 0x5eed";
    let caught = std::panic::catch_unwind(AssertUnwindSafe(move || {
        let _dropped_during_unwind = retained;
        panic!("{PAYLOAD}");
    }));
    let payload = caught.expect_err("the primary panic must unwind");
    let observed = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("<unrecognised payload>");
    println!("primary-payload-preserved={}", observed == PAYLOAD);
}

/// Exercises the pre-spawn guard as a pure function over synthetic names, and its
/// per-role labelling. It never spawns the instrumented binary and never mutates
/// the parent environment.
#[test]
fn hostcontrol_selector_guard_refuses_constructor_selection_and_cleans_children() {
    let retained = Retained::new("hostcontrol-selectors").unwrap();
    for name in [
        "REVERIE_LITEINST_TOOL",
        "REVERIE_LITEINST_STATS_COORDINATOR",
        "REVERIE_LITEINST_PRELOAD",
        "REVERIE_LITEINST_ALT_STACK",
        "REVERIE_LITEINST_PROCESS_FORK",
        "REVERIE_COMPILED_STEP_ARTIFACTS",
        "LITEINST_ANYTHING",
    ] {
        let refused = selectors::refusals([name]);
        assert_eq!(refused.len(), 1, "{name} must be refused");
        assert!(selectors::report(&refused).unwrap().contains(name));
    }
    for name in [
        "PATH",
        "HOME",
        "CARGO_TARGET_DIR",
        "OWNED_PUBLIC_INSTRUCTION_ARTIFACTS",
        "OWNED_PUBLIC_INSTRUCTION_SYMBOLS",
        "OWNED_PUBLIC_INSTRUCTION_DISASM",
    ] {
        assert!(
            selectors::refusals([name]).is_empty(),
            "{name} must be allowed"
        );
    }
    let ordered = selectors::refusals(["LITEINST_ANYTHING", "REVERIE_LITEINST_TOOL"]);
    assert_eq!(
        ordered
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["LITEINST_ANYTHING", "REVERIE_LITEINST_TOOL"]
    );
    assert_eq!(
        loader_names(["LD_PRELOAD", "PATH", "LD_PRELOAD", "GLIBC_TUNABLES"]),
        ["GLIBC_TUNABLES", "LD_PRELOAD"]
    );
    assert!(selectors::report(&[]).is_none());

    let roles = Retained::under(retained.path(), "roles").unwrap();
    for role in ["server", "guest"] {
        let mut command = Command::new("/bin/true");
        command
            .env("LD_PRELOAD", "/must/not/load.so")
            .env("GLIBC_TUNABLES", "glibc.malloc.check=3");
        guard_before_spawn(&roles, role, &mut command);
        let environment = command
            .get_envs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(environment[std::ffi::OsStr::new("LD_PRELOAD")], None);
        assert_eq!(environment[std::ffi::OsStr::new("GLIBC_TUNABLES")], None);
    }
    for role in ["server", "guest"] {
        let text = std::fs::read_to_string(roles.join(&format!("preflight-selectors-{role}.txt")))
            .unwrap();
        assert!(text.contains("no product selector"), "{role}: {text}");
        let loader =
            std::fs::read_to_string(roles.join(&format!("preflight-loader-{role}.txt"))).unwrap();
        assert!(loader.contains("GLIBC_TUNABLES"), "{role}: {loader}");
        assert!(loader.contains("LD_PRELOAD"), "{role}: {loader}");
    }
    drop(roles);

    let live = selectors::refusals(selectors::current_names());
    retained
        .record(
            "live-environment.txt",
            selectors::report(&live)
                .as_deref()
                .unwrap_or("no refused variable present\n"),
        )
        .unwrap();
    assert!(
        live.is_empty(),
        "no REVERIE_/LITEINST_ product selector may be live: {live:?}"
    );
    retained.finish("ok").unwrap();
}
