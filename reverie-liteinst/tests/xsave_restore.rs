#![cfg(feature = "test-owned-cpuid")]

use std::process::Command;
use std::sync::OnceLock;

#[path = "../src/bin/rpc_tool_guest/xsave.rs"]
mod xsave;

struct Observation {
    variant: &'static str,
    before: [u8; 2440],
    clobbered: [u8; 2440],
    restore: [u8; 2440],
    after: [u8; 2440],
    raw_equal: bool,
}

fn observations() -> &'static [Observation] {
    static OBSERVATIONS: OnceLock<Vec<Observation>> = OnceLock::new();
    OBSERVATIONS.get_or_init(|| {
        let directory = tempfile::Builder::new()
            .prefix("reverie-xsave-restore-")
            .tempdir()
            .unwrap()
            .keep();
        println!("native XSAVE evidence: {}", directory.display());
        let variants: &[(&str, &[&str])] = &[
            ("zero-no-restore", &[]),
            ("zero-restore", &["RESTORE"]),
            ("zero-clobber-no-restore", &["CLOBBER"]),
            ("zero-clobber-restore", &["CLOBBER", "RESTORE"]),
            ("nonzero-clobber-no-restore", &["NONZERO", "CLOBBER"]),
            (
                "nonzero-clobber-restore",
                &["NONZERO", "CLOBBER", "RESTORE"],
            ),
            (
                "nonzero-corrupt-restore",
                &["NONZERO", "CLOBBER", "RESTORE", "CORRUPT"],
            ),
        ];
        let mut observations = Vec::new();
        for &(variant, defines) in variants {
            let executable = directory.join(variant);
            let mut command = Command::new("timeout");
            command.args([
                "--kill-after=1s",
                "30s",
                "cc",
                "-nostdlib",
                "-static",
                "-Wl,--build-id=none",
            ]);
            for define in defines {
                command.arg(format!("-D{define}"));
            }
            command
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/xsave_restore.S"
                ))
                .arg("-o")
                .arg(&executable);
            std::fs::write(
                directory.join(format!("{variant}.command")),
                format!("{command:?}\n"),
            )
            .unwrap();
            let built = command.output().unwrap();
            std::fs::write(
                directory.join(format!("{variant}.build.stderr")),
                &built.stderr,
            )
            .unwrap();
            std::fs::write(
                directory.join(format!("{variant}.build.status")),
                built.status.to_string(),
            )
            .unwrap();
            assert!(built.status.success(), "{built:?}; {}", directory.display());
            for repeat in 0..3 {
                let output = Command::new("timeout")
                    .args(["--kill-after=1s", "5s"])
                    .arg(&executable)
                    .output()
                    .unwrap();
                let label = format!("{variant}-{repeat}");
                std::fs::write(directory.join(format!("{label}.bin")), &output.stdout).unwrap();
                std::fs::write(directory.join(format!("{label}.stderr")), &output.stderr).unwrap();
                std::fs::write(
                    directory.join(format!("{label}.status")),
                    output.status.to_string(),
                )
                .unwrap();
                assert!(
                    matches!(output.status.code(), Some(0 | 1)),
                    "native XSAVE profile refused: {variant} {:?}; {}",
                    output.status,
                    directory.display()
                );
                assert!(output.stderr.is_empty(), "{output:?}");
                assert_eq!(output.stdout.len(), 9760);
                let observation = Observation {
                    variant,
                    before: output.stdout[..2440].try_into().unwrap(),
                    clobbered: output.stdout[2440..4880].try_into().unwrap(),
                    restore: output.stdout[4880..7320].try_into().unwrap(),
                    after: output.stdout[7320..].try_into().unwrap(),
                    raw_equal: output.status.success(),
                };
                assert_eq!(
                    observation.raw_equal,
                    observation.before == observation.after
                );
                assert_eq!(
                    u64::from_le_bytes(observation.before[512..520].try_into().unwrap()),
                    0x2a7
                );
                assert_eq!(&observation.before[520..528], &[0; 8]);
                let initial = if defines.contains(&"NONZERO") { 255 } else { 0 };
                assert_eq!(&observation.before[1408..2432], &[initial; 1024]);
                if defines.contains(&"CLOBBER") {
                    assert_eq!(&observation.clobbered[1408..2432], &[initial ^ 255; 1024]);
                    assert_ne!(observation.before, observation.clobbered);
                } else {
                    assert_eq!(observation.before, observation.clobbered);
                }
                let mut expected_restore = observation.before;
                if defines.contains(&"CORRUPT") {
                    expected_restore[1408] ^= 1;
                }
                assert_eq!(observation.restore, expected_restore);
                if defines.contains(&"RESTORE") {
                    assert_eq!(
                        &observation.after[1408..2432],
                        &observation.restore[1408..2432]
                    );
                } else {
                    assert_eq!(observation.after, observation.clobbered);
                }
                observations.push(observation);
            }
        }
        observations
    })
}

#[test]
fn actual_restoration_preserves_payload_and_models_only_initialized_hi16_clear() {
    for observation in observations().iter().filter(|observation| {
        matches!(
            observation.variant,
            "zero-no-restore" | "zero-restore" | "zero-clobber-restore" | "nonzero-clobber-restore"
        )
    }) {
        let cleared = xsave::compare_native_xsave(&observation.before, &observation.after).unwrap();
        assert_eq!(cleared, !observation.raw_equal);
        println!(
            "{}: old_full_byte_equality={} initialized_hi16_zmm_bit_cleared={cleared}",
            observation.variant, observation.raw_equal
        );
        if observation.variant == "nonzero-clobber-restore" {
            assert!(observation.raw_equal);
        }
    }
}

#[test]
fn omitted_restore_and_real_corrupted_restore_input_are_rejected() {
    for observation in observations().iter().filter(|observation| {
        matches!(
            observation.variant,
            "zero-clobber-no-restore" | "nonzero-clobber-no-restore" | "nonzero-corrupt-restore"
        )
    }) {
        assert!(!observation.raw_equal);
        assert!(
            xsave::compare_native_xsave(&observation.before, &observation.after).is_err(),
            "{}",
            observation.variant
        );
    }
}

#[test]
fn payload_corruption_and_impermissible_header_changes_are_independently_rejected() {
    let observation = observations()
        .iter()
        .find(|observation| observation.variant == "zero-clobber-restore")
        .unwrap();
    assert!(xsave::compare_native_xsave(&observation.before, &observation.after).is_ok());
    for offset in [0, 1408, 2439, 512, 520, 528] {
        let mut mutated = observation.after;
        mutated[offset] ^= 1;
        assert!(
            xsave::compare_native_xsave(&observation.before, &mutated).is_err(),
            "offset={offset}"
        );
    }
}

#[test]
fn nonzero_restored_payload_rejects_a_falsely_permitted_header_mask() {
    let observation = observations()
        .iter()
        .find(|observation| observation.variant == "nonzero-clobber-restore")
        .unwrap();
    assert_eq!(
        xsave::compare_native_xsave(&observation.before, &observation.after),
        Ok(false)
    );
    let mut mutated = observation.after;
    mutated[512] &= !0x80;
    assert_eq!(observation.before[512] & !0x80, mutated[512]);
    assert_eq!(&observation.before[1408..2432], &mutated[1408..2432]);
    assert!(xsave::compare_native_xsave(&observation.before, &mutated).is_err());
}
