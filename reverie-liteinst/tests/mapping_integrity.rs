//! Additive real forwarding controls; all original workload sources are separate.

#[cfg(feature = "liteinst")]
use std::ffi::OsStr;
use std::process::Command;

#[cfg(feature = "liteinst")]
use reverie_liteinst::PreloadTool;
#[cfg(feature = "liteinst")]
use reverie_liteinst::configure_command;

// This consumer uses only part of the shared runtime qualification support.
#[allow(dead_code)]
#[path = "support/liteinst_runtime.rs"]
mod liteinst_runtime;
use liteinst_runtime as runtime;
#[path = "support/fulltool_capture.rs"]
mod fulltool_capture;

const C_SOURCE: &[u8] = include_bytes!("fixtures/mapping_integrity_guest.c");
const ASM_SOURCE: &[u8] = include_bytes!("fixtures/mapping_integrity_guest.S");

fn require(value: bool, message: impl Into<String>) -> Result<(), String> {
    if value { Ok(()) } else { Err(message.into()) }
}

#[test]
fn published_owner_guest_mprotect_and_munmap_preserve_real_spare_pages() -> Result<(), String> {
    let leaf = runtime::required_runtime()?;
    let evidence = runtime::evidence_directory("mapping-integrity-minimal")?;
    eprintln!("Mapping integrity literal evidence {}", evidence.display());
    let source_before = runtime::current_source(&evidence, "before")?;
    require(
        source_before == leaf.source,
        "current source differs from qualified runtime",
    )?;
    require(
        leaf.receipt.qualification.allocator_backing.descriptor.mode
            == runtime::artifact::AllocatorBackingMode::Fixed,
        "standalone mapping fixture requires actual Fixed qualification",
    )?;
    let receipt_identity = runtime::artifact::FileIdentity::read(&leaf.receipt_path)?;
    let sources = leaf.source.root.join("reverie-liteinst/tests/fixtures");
    let c_source =
        runtime::artifact::FileIdentity::read(&sources.join("mapping_integrity_guest.c"))?;
    let asm_source =
        runtime::artifact::FileIdentity::read(&sources.join("mapping_integrity_guest.S"))?;
    require(
        c_source.sha256 == runtime::artifact::sha256(C_SOURCE),
        "compiled/current C source differs",
    )?;
    require(
        asm_source.sha256 == runtime::artifact::sha256(ASM_SOURCE),
        "compiled/current asm source differs",
    )?;
    let result = (|| {
        let binary = evidence.join("mapping-integrity-guest");
        let held = runtime::held_environment(b"separate mapping guest compiler".to_vec());
        let mut compiler = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()));
        compiler
            .args([
                "-std=gnu11",
                "-O0",
                "-fno-pie",
                "-no-pie",
                "-fno-builtin",
                "-fno-lto",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-Wl,-z,now",
            ])
            .arg(&c_source.path)
            .arg(&asm_source.path)
            .arg("-ldl")
            .arg("-o")
            .arg(&binary)
            .current_dir(&evidence);
        if !cfg!(feature = "liteinst") {
            compiler.arg("-DREVERIE_MAPPING_FULLTOOL=1");
        }
        let compiled = match runtime::contract::run_without_input(compiler, &held) {
            Ok(run) => run,
            Err(failure) => {
                runtime::retain_failure(&evidence, "compiler", &failure)?;
                return Err(format!(
                    "mapping guest compiler: {failure}; evidence {}",
                    evidence.display()
                ));
            }
        };
        runtime::retain_capture(&evidence, "compiler", &compiled)?;
        require(
            compiled.status.is_some_and(|status| status.success()),
            "mapping guest compiler failed",
        )?;
        let guest_identity = runtime::artifact::FileIdentity::read(&binary)?;
        runtime::artifact::write_new(
            &evidence.join("inputs.json"),
            &serde_json::to_vec_pretty(&serde_json::json!({
                "runtime": leaf.receipt.artifact, "runtime_receipt": receipt_identity,
                "source": source_before, "c_source": c_source, "asm_source": asm_source,
                "guest": guest_identity, "expected_raw_errno": -libc::EPERM,
                "route_evidence_required": true, "original_oracles_changed": false,
                "full_isolation_claimed": false,
            }))
            .map_err(|error| error.to_string())?,
        )?;
        let mut guest_environment = held.environment;
        guest_environment
            .retain(|(name, _)| !name.to_string_lossy().starts_with("REVERIE_LITEINST_"));
        let mut failures = Vec::new();
        #[cfg(feature = "liteinst")]
        for (tool, label) in [
            (PreloadTool::Strace, "strace"),
            (PreloadTool::Compatibility, "compat"),
        ] {
            for operation in ["mprotect", "munmap"] {
                let name = format!("{label}-{operation}");
                let mut command = Command::new(&binary);
                command
                    .arg(&leaf.path)
                    .arg(operation)
                    .current_dir(&evidence)
                    .env_clear()
                    .envs(guest_environment.iter().map(|(key, value)| (key, value)));
                configure_command(&mut command, tool).map_err(|error| error.to_string())?;
                let environment: Vec<_> = command
                    .get_envs()
                    .map(|(key, value)| {
                        value
                            .map(|value| (key.to_os_string(), value.to_os_string()))
                            .ok_or_else(|| {
                                "configured child has a removed environment key".to_owned()
                            })
                    })
                    .collect::<Result<_, _>>()?;
                require(
                    environment.iter().any(|(key, value)| {
                        key == OsStr::new("LD_PRELOAD") && value == leaf.path.as_os_str()
                    }),
                    "configured child preload differs from the qualified runtime",
                )?;
                require(
                    environment.iter().any(|(key, value)| {
                        key == OsStr::new("REVERIE_LITEINST_TOOL") && value == OsStr::new(label)
                    }),
                    "configured child tool selector differs from its trial",
                )?;
                let child_held = runtime::contract::HeldInputs {
                    environment,
                    policy: format!("mapping integrity {label}/{operation}; exact qualified preload; EPERM/maps/sentinel/same-page lease").into_bytes(),
                };
                match runtime::contract::run_without_input(command, &child_held) {
                    Ok(run) => {
                        runtime::retain_capture(&evidence, &name, &run)?;
                        if !run.status.is_some_and(|status| status.success()) {
                            failures.push(format!("{name}: literal child exit {:?}", run.status));
                        }
                        let text = String::from_utf8_lossy(&run.stdout);
                        if text
                            .lines()
                            .filter(|line| line.starts_with("MAPPING_LITERAL "))
                            .count()
                            != 1
                        {
                            failures.push(format!(
                                "{name}: one complete physical observation is required"
                            ));
                        }
                        if run.output_truncated
                            || run.input.is_some()
                            || run.end != runtime::contract::CaptureEnd::Complete
                        {
                            failures.push(format!("{name}: incomplete/truncated process capture"));
                        }
                    }
                    Err(failure) => {
                        runtime::retain_failure(&evidence, &name, &failure)?;
                        failures.push(format!("{name}: {failure}"));
                    }
                }
                c_source.verify()?;
                asm_source.verify()?;
                guest_identity.verify()?;
                leaf.receipt.artifact.verify()?;
                receipt_identity.verify()?;
            }
        }
        #[cfg(not(feature = "liteinst"))]
        failures.extend(full_tool_trials(
            &leaf.path,
            &evidence,
            &binary,
            &guest_environment,
            &["mprotect", "munmap"],
            "MAPPING_LITERAL ",
            &[
                &c_source,
                &asm_source,
                &guest_identity,
                &leaf.receipt.artifact,
                &receipt_identity,
            ],
        )?);
        require(
            failures.is_empty(),
            format!(
                "{}; literal evidence {}",
                failures.join("; "),
                evidence.display()
            ),
        )
    })();
    let after = runtime::current_source(&evidence, "after");
    c_source.verify()?;
    asm_source.verify()?;
    leaf.receipt.artifact.verify()?;
    receipt_identity.verify()?;
    match after {
        Ok(source_after) if source_after == source_before => result,
        Ok(_) => Err(format!(
            "source changed during mapping trials; prior result {result:?}"
        )),
        Err(error) => Err(format!(
            "mapping source postcheck: {error}; prior result {result:?}"
        )),
    }
}

const BOUNDARY_C_SOURCE: &[u8] = include_bytes!("fixtures/mapping_boundaries_guest.c");

#[test]
fn native_mapping_boundaries_preserve_guards_break_abi_and_fork_owner() -> Result<(), String> {
    let leaf = runtime::required_runtime()?;
    let evidence = runtime::evidence_directory("mapping-boundaries")?;
    eprintln!("Mapping boundary literal evidence {}", evidence.display());
    let source_before = runtime::current_source(&evidence, "before")?;
    require(
        source_before == leaf.source,
        "current source differs from qualified runtime",
    )?;
    require(
        leaf.receipt.qualification.allocator_backing.descriptor.mode
            == runtime::artifact::AllocatorBackingMode::Fixed,
        "standalone mapping fixture requires actual Fixed qualification",
    )?;
    let receipt_identity = runtime::artifact::FileIdentity::read(&leaf.receipt_path)?;
    let sources = leaf.source.root.join("reverie-liteinst/tests/fixtures");
    let c_source =
        runtime::artifact::FileIdentity::read(&sources.join("mapping_boundaries_guest.c"))?;
    let asm_source =
        runtime::artifact::FileIdentity::read(&sources.join("mapping_integrity_guest.S"))?;
    require(
        c_source.sha256 == runtime::artifact::sha256(BOUNDARY_C_SOURCE),
        "compiled/current C source differs",
    )?;
    require(
        asm_source.sha256 == runtime::artifact::sha256(ASM_SOURCE),
        "compiled/current asm source differs",
    )?;
    let result = (|| {
        let binary = evidence.join("mapping-boundaries-guest");
        let held = runtime::held_environment(b"separate mapping guest compiler".to_vec());
        let mut compiler = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()));
        compiler
            .args([
                "-std=gnu11",
                "-O0",
                "-fno-pie",
                "-no-pie",
                "-fno-builtin",
                "-fno-lto",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-Wl,-z,now",
            ])
            .arg(&c_source.path)
            .arg(&asm_source.path)
            .arg("-ldl")
            .arg("-o")
            .arg(&binary)
            .current_dir(&evidence);
        if !cfg!(feature = "liteinst") {
            compiler.arg("-DREVERIE_MAPPING_FULLTOOL=1");
        }
        let compiled = match runtime::contract::run_without_input(compiler, &held) {
            Ok(run) => run,
            Err(failure) => {
                runtime::retain_failure(&evidence, "compiler", &failure)?;
                return Err(format!(
                    "mapping guest compiler: {failure}; evidence {}",
                    evidence.display()
                ));
            }
        };
        runtime::retain_capture(&evidence, "compiler", &compiled)?;
        require(
            compiled.status.is_some_and(|status| status.success()),
            "mapping guest compiler failed",
        )?;
        let guest_identity = runtime::artifact::FileIdentity::read(&binary)?;
        runtime::artifact::write_new(
            &evidence.join("inputs.json"),
            &serde_json::to_vec_pretty(&serde_json::json!({
                "runtime": leaf.receipt.artifact, "runtime_receipt": receipt_identity,
                "source": source_before, "c_source": c_source, "asm_source": asm_source,
                "guest": guest_identity, "mapping_errno": -libc::EPERM,
                "brk_contract": "genuine unchanged break; libc ENOMEM for refused growth",
                "mseal_live_support": "reported separately; ENOSYS gives no live mutation credit",
                "route_evidence_required": true, "original_oracles_changed": false,
                "full_isolation_claimed": false,
            }))
            .map_err(|error| error.to_string())?,
        )?;
        let mut guest_environment = held.environment;
        guest_environment
            .retain(|(name, _)| !name.to_string_lossy().starts_with("REVERIE_LITEINST_"));
        let mut failures = Vec::new();
        #[cfg(feature = "liteinst")]
        for (tool, label) in [
            (PreloadTool::Strace, "strace"),
            (PreloadTool::Compatibility, "compat"),
        ] {
            for operation in ["guard", "live", "brk", "mseal", "fork"] {
                let name = format!("{label}-{operation}");
                let mut command = Command::new(&binary);
                command
                    .arg(&leaf.path)
                    .arg(operation)
                    .current_dir(&evidence)
                    .env_clear()
                    .envs(guest_environment.iter().map(|(key, value)| (key, value)));
                configure_command(&mut command, tool).map_err(|error| error.to_string())?;
                let environment: Vec<_> = command
                    .get_envs()
                    .map(|(key, value)| {
                        value
                            .map(|value| (key.to_os_string(), value.to_os_string()))
                            .ok_or_else(|| {
                                "configured child has a removed environment key".to_owned()
                            })
                    })
                    .collect::<Result<_, _>>()?;
                require(
                    environment.iter().any(|(key, value)| {
                        key == OsStr::new("LD_PRELOAD") && value == leaf.path.as_os_str()
                    }),
                    "configured child preload differs from the qualified runtime",
                )?;
                require(
                    environment.iter().any(|(key, value)| {
                        key == OsStr::new("REVERIE_LITEINST_TOOL") && value == OsStr::new(label)
                    }),
                    "configured child tool selector differs from its trial",
                )?;
                let child_held = runtime::contract::HeldInputs {
                    environment,
                    policy: format!("mapping boundaries {label}/{operation}; exact qualified preload; literal ABI/maps/sentinel/fork/lease").into_bytes(),
                };
                match runtime::contract::run_without_input(command, &child_held) {
                    Ok(run) => {
                        runtime::retain_capture(&evidence, &name, &run)?;
                        if !run.status.is_some_and(|status| status.success()) {
                            failures.push(format!("{name}: literal child exit {:?}", run.status));
                        }
                        let text = String::from_utf8_lossy(&run.stdout);
                        if text
                            .lines()
                            .filter(|line| line.starts_with("MAPPING_BOUNDARY "))
                            .count()
                            != 1
                        {
                            failures.push(format!(
                                "{name}: one complete physical observation is required"
                            ));
                        }
                        if run.output_truncated
                            || run.input.is_some()
                            || run.end != runtime::contract::CaptureEnd::Complete
                        {
                            failures.push(format!("{name}: incomplete/truncated process capture"));
                        }
                    }
                    Err(failure) => {
                        runtime::retain_failure(&evidence, &name, &failure)?;
                        failures.push(format!("{name}: {failure}"));
                    }
                }
                c_source.verify()?;
                asm_source.verify()?;
                guest_identity.verify()?;
                leaf.receipt.artifact.verify()?;
                receipt_identity.verify()?;
            }
        }
        #[cfg(not(feature = "liteinst"))]
        failures.extend(full_tool_trials(
            &leaf.path,
            &evidence,
            &binary,
            &guest_environment,
            &["guard", "live", "brk", "mseal", "fork"],
            "MAPPING_BOUNDARY ",
            &[
                &c_source,
                &asm_source,
                &guest_identity,
                &leaf.receipt.artifact,
                &receipt_identity,
            ],
        )?);
        require(
            failures.is_empty(),
            format!(
                "{}; literal evidence {}",
                failures.join("; "),
                evidence.display()
            ),
        )
    })();
    let after = runtime::current_source(&evidence, "after");
    c_source.verify()?;
    asm_source.verify()?;
    leaf.receipt.artifact.verify()?;
    receipt_identity.verify()?;
    match after {
        Ok(source_after) if source_after == source_before => result,
        Ok(_) => Err(format!(
            "source changed during mapping trials; prior result {result:?}"
        )),
        Err(error) => Err(format!(
            "mapping source postcheck: {error}; prior result {result:?}"
        )),
    }
}

/// The plain shared host must guard a raw syscall before typed dispatch can
/// classify it as unknown. Keep this independent of the original legacy legs.
#[test]
#[cfg(feature = "liteinst")]
fn plain_fulltool_raw_mseal_protects_owner_before_unknown_syscall_refusal() -> Result<(), String> {
    let leaf = runtime::required_runtime()?;
    let evidence = runtime::evidence_directory("mapping-plain-fulltool-mseal")?;
    eprintln!("Plain FullTool raw-mseal evidence {}", evidence.display());
    let before = runtime::current_source(&evidence, "before")?;
    require(
        before == leaf.source,
        "current source differs from qualified runtime",
    )?;
    require(
        leaf.receipt.qualification.allocator_backing.descriptor.mode
            == runtime::artifact::AllocatorBackingMode::Fixed,
        "plain FullTool probe requires the actual qualified Fixed leaf",
    )?;
    let receipt = runtime::artifact::FileIdentity::read(&leaf.receipt_path)?;
    let sources = leaf.source.root.join("reverie-liteinst/tests/fixtures");
    let c_source =
        runtime::artifact::FileIdentity::read(&sources.join("mapping_boundaries_guest.c"))?;
    let assembly =
        runtime::artifact::FileIdentity::read(&sources.join("mapping_integrity_guest.S"))?;
    require(
        c_source.sha256 == runtime::artifact::sha256(BOUNDARY_C_SOURCE),
        "compiled/current C source differs",
    )?;
    require(
        assembly.sha256 == runtime::artifact::sha256(ASM_SOURCE),
        "compiled/current assembly source differs",
    )?;
    let result = (|| {
        let binary = evidence.join("plain-fulltool-mseal-guest");
        let held = runtime::held_environment(
            b"independent plain FullTool raw-mseal regression compiler".to_vec(),
        );
        let mut compiler = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()));
        compiler
            .args([
                "-std=gnu11",
                "-O0",
                "-fno-pie",
                "-no-pie",
                "-fno-builtin",
                "-fno-lto",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-Wl,-z,now",
                "-DREVERIE_MAPPING_FULLTOOL=1",
            ])
            .arg(&c_source.path)
            .arg(&assembly.path)
            .arg("-ldl")
            .arg("-o")
            .arg(&binary)
            .current_dir(&evidence);
        let compiled = match runtime::contract::run_without_input(compiler, &held) {
            Ok(run) => {
                runtime::retain_capture(&evidence, "compiler", &run)?;
                run
            }
            Err(error) => {
                runtime::retain_failure(&evidence, "compiler", &error)?;
                return Err(format!("plain FullTool C compiler: {error}"));
            }
        };
        require(
            compiled.status.is_some_and(|status| status.success())
                && compiled.end == runtime::contract::CaptureEnd::Complete
                && !compiled.output_truncated
                && compiled.input.is_none(),
            "plain FullTool C compiler did not complete successfully",
        )?;
        let guest = runtime::artifact::FileIdentity::read(&binary)?;
        runtime::artifact::write_new(&evidence.join("inputs.json"),
            &serde_json::to_vec_pretty(&serde_json::json!({
                "runtime": leaf.receipt.artifact, "receipt": receipt, "source": before,
                "c_source": c_source, "assembly": assembly, "guest": guest,
                "route": "installed FullTool, site patching disabled, plain callback",
                "protected_raw_mseal": -libc::EPERM, "outside_unknown_raw_mseal": -libc::ENOSYS,
                "owner_oracles": "complete VMA, sentinel bytes, owner identity and both lease reuse identities",
                "legacy_positive_legs_changed": false,
            })).map_err(|error| error.to_string())?)?;
        let mut environment = held.environment;
        environment.retain(|(key, _)| {
            !key.to_string_lossy().starts_with("REVERIE_LITEINST_")
                && key != "REVERIE_INGUEST_TOOL"
                && key != "LD_PRELOAD"
        });
        let failures = full_tool_trials(
            &leaf.path,
            &evidence,
            &binary,
            &environment,
            &["mseal"],
            "MAPPING_BOUNDARY ",
            &[
                &c_source,
                &assembly,
                &guest,
                &leaf.receipt.artifact,
                &receipt,
            ],
        )?;
        require(failures.is_empty(), failures.join("; "))
    })();
    let after = (|| {
        c_source.verify()?;
        assembly.verify()?;
        leaf.receipt.artifact.verify()?;
        receipt.verify()?;
        require(
            runtime::current_source(&evidence, "after")? == before,
            "plain FullTool regression source changed",
        )
    })();
    match (result, after) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => {
            Err(format!("{error}; evidence {}", evidence.display()))
        }
        (Err(error), Err(after)) => Err(format!(
            "{error}; final guard: {after}; evidence {}",
            evidence.display()
        )),
    }
}

fn full_tool_trials(
    preload: &std::path::Path,
    evidence: &std::path::Path,
    binary: &std::path::Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
    operations: &[&str],
    literal_prefix: &str,
    identities: &[&runtime::artifact::FileIdentity],
) -> Result<Vec<String>, String> {
    let mut failures = Vec::new();
    for operation in operations {
        let name = format!("full-tool-{operation}");
        let socket = evidence.join(format!("{name}.sock"));
        let mut command = Command::new(binary);
        command
            .arg(preload)
            .arg(operation)
            .current_dir(evidence)
            .env_clear()
            .envs(environment.iter().map(|(key, value)| (key, value)))
            .env("LD_PRELOAD", preload)
            .env(reverie_liteinst::COORDINATOR_ENV, &socket)
            .env(reverie_liteinst::SITE_PATCHING_ENV, "0");
        let environment = command
            .get_envs()
            .map(|(key, value)| {
                value
                    .map(|value| (key.to_os_string(), value.to_os_string()))
                    .ok_or_else(|| "FullTool child has a removed environment key".to_owned())
            })
            .collect::<Result<_, _>>()?;
        let held = runtime::contract::HeldInputs {
            environment,
            policy: format!(
                "mapping installed FullTool/{operation}; exact qualified Fixed leaf; same C ABI/maps/bytes/identity/lease/fork oracles; exact physical fallback and actual callback counts; no sites or hooks"
            )
            .into_bytes(),
        };
        // Keep the original bounded process-group capture, including its
        // 120-second deadline, 16-MiB cap and raw evidence retention. The
        // actual shared RPC server runs concurrently with that same owner.
        let trial = fulltool_capture::run(command, held, socket);
        match trial {
            Ok(run) => {
                runtime::retain_capture(evidence, &name, &run)?;
                if !run.status.is_some_and(|status| status.success()) {
                    failures.push(format!("{name}: literal child exit {:?}", run.status));
                }
                let text = std::str::from_utf8(&run.stdout)
                    .map_err(|error| format!("{name}: incomplete UTF8 observation: {error}"))?;
                if text
                    .lines()
                    .filter(|line| line.starts_with(literal_prefix))
                    .count()
                    != 1
                {
                    failures.push(format!(
                        "{name}: one complete physical observation required"
                    ));
                }
                let warmup = "MAPPING_FULLTOOL_PROOF label=warmup number=39 total=2 syscall=2 callbacks=2 sites=0 hooks=0";
                if text.lines().filter(|line| *line == warmup).count() != 1 {
                    failures.push(format!(
                        "{name}: exact genuine callback/trap warmup missing"
                    ));
                }
                let expected_proofs = if *operation == "fork" { 4 } else { 2 };
                if text
                    .lines()
                    .filter(|line| line.starts_with("MAPPING_FULLTOOL_PROOF "))
                    .count()
                    != expected_proofs
                {
                    failures.push(format!("{name}: missing exact measured syscall proof"));
                }
                if run.output_truncated
                    || run.input.is_some()
                    || run.end != runtime::contract::CaptureEnd::Complete
                {
                    failures.push(format!("{name}: incomplete/truncated process capture"));
                }
            }
            Err(failure) => {
                runtime::retain_failure(evidence, &name, &failure)?;
                failures.push(format!("{name}: {failure}"));
            }
        }
        for identity in identities {
            identity.verify()?;
        }
    }
    Ok(failures)
}
