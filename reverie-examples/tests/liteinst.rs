use std::process::Command;

#[test]
fn launcher_exposes_liteinst_tool_selection() {
    let output = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-examples"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    for tool in [
        "chaos",
        "chrome-trace",
        "chunky-print",
        "counter1",
        "counter2",
        "debug",
        "noop",
        "strace",
        "strace-minimal",
    ] {
        assert!(help.contains(tool), "missing {tool:?} from help:\n{help}");
    }
}

#[test]
fn launcher_reports_the_backend_refusal_without_starting_the_guest() {
    let output = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-examples"))
        .args(["--tool", "noop", "--preload", "/bin/true", "--"])
        .arg("/definitely-not-a-program")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("LiteInst Backend execution requires a caller-owned PreparedCommand"),
        "{stderr}"
    );
}
