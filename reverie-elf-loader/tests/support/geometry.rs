/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::ffi::OsStr;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::fs::Permissions;
use std::io;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;
use std::process::Output;

use reverie_elf_loader::Error;
use reverie_elf_loader::Invocation;
use reverie_elf_loader::prepare_start_with_limits;

use super::Artifacts;
use super::Case;
use super::Fixture;
use super::METADATA_FD;
use super::Mutation;
use super::Naming;
use super::TARGET_FD;
use super::TestResult;
use super::compare;
use super::error;
use super::io as check_io;
use super::output_root;
use super::run_pair;

const INTERPRETER_HINT: u64 = 0x800000;
const SECOND_HEADER_PAGE: u64 = 0x900000;

pub fn interpreter_address_hint(test: &str, fixture: Fixture) -> TestResult {
    let mut case = Case::new(test, fixture, Naming::Absolute, None)?;
    let mut interpreter = check_io(fs::read(env!("ELF_LOADER_OBSERVER")))?;
    let entry = word(&interpreter, 24) + INTERPRETER_HINT;
    put_word(&mut interpreter, 24, entry);
    for offset in header_offsets(&interpreter) {
        if kind(&interpreter, offset) == 1 {
            let address = word(&interpreter, offset + 16) + INTERPRETER_HINT;
            put_word(&mut interpreter, offset + 16, address);
            put_word(&mut interpreter, offset + 24, address);
        }
    }
    let path = case.root.join("hint-interpreter");
    write_fixture(
        &path,
        &interpreter,
        "observer PT_LOADs and entry shifted by 0x800000",
    )?;
    let mut main = check_io(fs::read(fixture.path()))?;
    set_interpreter(&mut main, &path);
    install_main(&mut case, &main)?;
    let pair = run_pair(&case, Mutation::None)?;
    let base = aux(&pair.native.entry.aux, 7)?;
    if matches!(fixture, Fixture::EntryNonPie) {
        if pair.artifacts.prepared.layout.load_bias != 0
            || base != 0
            || pair.native.entry.registers.rip != entry
        {
            return Err(format!(
                "ET_EXEC main did not honor free interpreter hint {INTERPRETER_HINT:#x}: AT_BASE={base:#x}, RIP={:#x}",
                pair.native.entry.registers.rip
            ));
        }
        compare::entries_with_zero_interpreter_bias(&case, &pair)
            .map_err(|failure| failure.to_string())?;
    } else {
        if pair.artifacts.prepared.layout.load_bias == 0 || base == 0 {
            return Err("PIE control did not discard the interpreter hint".into());
        }
        compare::entries(&case, &pair).map_err(|failure| failure.to_string())?;
    }
    if pair.loaded.entry.registers.rip != entry + base {
        return Err("loader interpreter entry did not match the native hint/bias".into());
    }
    compare::observers(&pair).map_err(|failure| failure.to_string())?;
    println!(
        "{test}: first PT_LOAD hint={INTERPRETER_HINT:#x}, main bias={:#x}, native/loader AT_BASE={base:#x}, RIP={:#x}",
        pair.artifacts.prepared.layout.load_bias, pair.native.entry.registers.rip
    );
    Ok(())
}

pub fn duplicate_header_mapping() -> TestResult {
    for fixture in [Fixture::EntryPie, Fixture::EntryNonPie] {
        let mut case = Case::new("last-header-mapping", fixture, Naming::Absolute, None)?;
        let mut main = check_io(fs::read(fixture.path()))?;
        let phoff = word(&main, 32);
        // The observer never runs the main's dynamic linker. Its unused
        // RELRO header can independently map the first file page a second
        // time, after every ordinary PT_LOAD, without duplicating GNU_STACK.
        let offset = header_offsets(&main)
            .into_iter()
            .find(|offset| kind(&main, *offset) == 0x6474e552)
            .expect("entry fixture has a replaceable GNU_RELRO");
        main[offset..offset + 56].fill(0);
        main[offset..offset + 4].copy_from_slice(&1u32.to_le_bytes());
        main[offset + 4..offset + 8].copy_from_slice(&4u32.to_le_bytes());
        put_word(&mut main, offset + 16, SECOND_HEADER_PAGE);
        put_word(&mut main, offset + 24, SECOND_HEADER_PAGE);
        put_word(&mut main, offset + 32, 4096);
        put_word(&mut main, offset + 40, 4096);
        put_word(&mut main, offset + 48, 4096);
        install_main(&mut case, &main)?;
        let pair = run_pair(&case, Mutation::None)?;
        let expected = pair.artifacts.prepared.layout.load_bias + SECOND_HEADER_PAGE + phoff;
        if aux(&pair.native.entry.aux, 3)? != expected
            || pair.artifacts.prepared.layout.phdr != expected
        {
            return Err(format!(
                "last header mapping did not supply AT_PHDR={expected:#x}"
            ));
        }
        compare::entries(&case, &pair).map_err(|failure| failure.to_string())?;
        compare::observers(&pair).map_err(|failure| failure.to_string())?;
        println!(
            "duplicate header page {}: native/loader AT_PHDR={expected:#x}",
            fixture.label()
        );
    }
    Ok(())
}

pub fn interpreter_load_span() -> TestResult {
    for (elf_type, label) in [(2u16, "et-exec"), (3, "et-dyn")] {
        let mut case = Case::new(
            &format!("interpreter-load-span-{label}"),
            Fixture::EntryNonPie,
            Naming::Absolute,
            None,
        )?;
        let mut main = check_io(fs::read(Fixture::EntryNonPie.path()))?;
        let entry = word(&main, 24);
        let text = header_offsets(&main)
            .into_iter()
            .find(|offset| {
                kind(&main, *offset) == 1
                    && word(&main, *offset + 16) <= entry
                    && entry < word(&main, *offset + 16) + word(&main, *offset + 32)
            })
            .expect("entry fixture maps its entry");
        let code = (word(&main, text + 8) + entry - word(&main, text + 16)) as usize;
        // Make reaching the already mapped main entry a successful exit.
        // The zero-span interpreter below must never transfer to this code.
        main[code..code + 11].copy_from_slice(&[
            0xb8, 0xe7, 0, 0, 0, // mov $SYS_exit_group, %eax
            0x31, 0xff, // xor %edi, %edi
            0x0f, 0x05, // syscall
            0x0f, 0x0b, // ud2
        ]);
        let interpreter_path = case.root.join("span-interpreter");
        set_interpreter(&mut main, &interpreter_path);
        install_main(&mut case, &main)?;
        let mut interpreter = check_io(fs::read(env!("ELF_LOADER_OBSERVER")))?;
        interpreter.truncate(64);
        interpreter.resize(120, 0);
        interpreter[16..18].copy_from_slice(&elf_type.to_le_bytes());
        interpreter[56..58].copy_from_slice(&1u16.to_le_bytes());
        interpreter[58..64].fill(0); // no section table
        put_word(&mut interpreter, 24, entry);
        put_word(&mut interpreter, 32, 64);
        put_word(&mut interpreter, 40, 0);
        interpreter[64..68].copy_from_slice(&1u32.to_le_bytes());
        interpreter[68..72].copy_from_slice(&5u32.to_le_bytes());
        put_word(&mut interpreter, 80, INTERPRETER_HINT);
        put_word(&mut interpreter, 88, INTERPRETER_HINT);
        put_word(&mut interpreter, 104, 1); // positive span: one byte of BSS
        put_word(&mut interpreter, 112, 4096);
        write_fixture(
            &interpreter_path,
            &interpreter,
            "one-byte interpreter load span; entry in main",
        )?;
        let pair = run_pair(&case, Mutation::None)?;
        compare::entries_with_zero_interpreter_bias(&case, &pair)
            .map_err(|failure| failure.to_string())?;
        if !pair.native.stdout.is_empty() || !pair.loaded.stdout.is_empty() {
            return Err("positive-span control unexpectedly wrote output".into());
        }

        // Deliberately invalidate the pinned path after a qualifying prepare
        // to test the loader's own guard as well as the host refusal. The main,
        // prepared image, metadata and entry address are unchanged.
        put_word(&mut interpreter, 104, 0);
        write_fixture(
            &interpreter_path,
            &interpreter,
            "zero interpreter load span; entry still in main",
        )?;
        match prepare_start_with_limits(
            &case.target,
            &case.invocation,
            Path::new("./h"),
            case.limits,
        ) {
            Err(Error::ZeroInterpreterLoadSpan) => {}
            result => {
                return Err(format!(
                    "zero-span {label} preparation gave wrong refusal: {result:?}"
                ));
            }
        }
        let native = untraced_start(&case, None, "zero-span-native")?;
        if native.status.signal() != Some(libc::SIGSEGV)
            || !native.stdout.is_empty()
            || !native.stderr.is_empty()
        {
            return Err(format!(
                "zero-span {label} native control did not die silently with SIGSEGV: {native:?}"
            ));
        }
        let loaded = untraced_start(&case, Some(&pair.artifacts), "zero-span-loader")?;
        if loaded.status.code() != Some(127)
            || !loaded.stdout.is_empty()
            || loaded.stderr != b"reverie-elf-loader: interpreter has zero load span -22\n"
        {
            return Err(format!(
                "zero-span {label} escaped loader defensive refusal: {loaded:?}"
            ));
        }
        println!(
            "{label} interpreter: span1 native/loader parity PASS; span0 native SIGSEGV, host ZeroInterpreterLoadSpan, loader exit127 with exact diagnostic"
        );
    }
    Ok(())
}

fn install_main(case: &mut Case, image: &[u8]) -> TestResult {
    let path = case.root.join("target");
    write_fixture(&path, image, "entry fixture ELF header/code mutation")?;
    case.target = check_io(File::open(&path))?;
    case.invocation = error(Invocation::execve(path.as_os_str()))?;
    Ok(())
}

fn set_interpreter(image: &mut Vec<u8>, path: &Path) {
    let offset = header_offsets(image)
        .into_iter()
        .find(|offset| kind(image, *offset) == 3)
        .expect("entry fixture has PT_INTERP");
    let start = image.len() as u64;
    let name = path.as_os_str().as_bytes();
    image.extend_from_slice(name);
    image.push(0);
    put_word(image, offset + 8, start);
    put_word(image, offset + 32, name.len() as u64 + 1);
    put_word(image, offset + 40, name.len() as u64 + 1);
}

fn write_fixture(path: &Path, image: &[u8], operation: &str) -> TestResult {
    check_io(fs::write(path, image))?;
    check_io(fs::set_permissions(path, Permissions::from_mode(0o755)))?;
    let hash = check_io(Command::new("sha256sum").arg(path).output())?;
    if !hash.status.success() {
        return Err("sha256sum failed for ELF geometry fixture".into());
    }
    let mut evidence = check_io(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(output_root()?.join("geometry-images.provenance")),
    )?;
    check_io(writeln!(
        evidence,
        "operation: {operation}\nsha256: {}",
        String::from_utf8_lossy(&hash.stdout).trim_end()
    ))
}

fn untraced_start(case: &Case, artifacts: Option<&Artifacts>, label: &str) -> TestResult<Output> {
    let path = artifacts.map_or(case.invocation.path(), |value| {
        value.prepared.padded_path.as_c_str()
    });
    let mut command = Command::new(OsStr::from_bytes(path.to_bytes()));
    command.arg0(OsStr::from_bytes(case.argv[0].as_bytes()));
    for arg in &case.argv[1..] {
        command.arg(OsStr::from_bytes(arg.as_bytes()));
    }
    command.current_dir(&case.root).env_clear();
    for variable in &case.env {
        let bytes = variable.as_bytes();
        let separator = bytes
            .iter()
            .position(|byte| *byte == b'=')
            .expect("fixture environment has '='");
        command.env(
            OsStr::from_bytes(&bytes[..separator]),
            OsStr::from_bytes(&bytes[separator + 1..]),
        );
    }
    let target_fd = case.target.as_raw_fd();
    let metadata_fd = artifacts.map(|value| value.metadata.as_raw_fd());
    let data_limit = case.limits.data;
    // SAFETY: prepared child values are copied integers. After fork this
    // callback allocates nothing, takes no locks and issues only syscalls.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(target_fd, TARGET_FD) < 0
                || libc::fcntl(
                    TARGET_FD,
                    libc::F_SETFD,
                    if metadata_fd.is_some() {
                        0
                    } else {
                        libc::FD_CLOEXEC
                    },
                ) < 0
            {
                return Err(io::Error::last_os_error());
            }
            if let Some(fd) = metadata_fd
                && (libc::dup2(fd, METADATA_FD) < 0
                    || libc::fcntl(METADATA_FD, libc::F_SETFD, 0) < 0)
            {
                return Err(io::Error::last_os_error());
            }
            let current = libc::personality(!0 as libc::c_ulong);
            if current < 0
                || libc::personality(
                    current as libc::c_ulong | libc::ADDR_NO_RANDOMIZE as libc::c_ulong,
                ) < 0
                || libc::personality(!0 as libc::c_ulong) & libc::ADDR_NO_RANDOMIZE == 0
            {
                return Err(io::Error::last_os_error());
            }
            let core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            let data = libc::rlimit {
                rlim_cur: data_limit,
                rlim_max: data_limit,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &core) < 0
                || libc::setrlimit(libc::RLIMIT_DATA, &data) < 0
            {
                return Err(io::Error::last_os_error());
            }
            libc::alarm(30);
            Ok(())
        });
    }
    let output = check_io(command.output())?;
    check_io(fs::write(
        case.root.join(format!("{label}.stdout")),
        &output.stdout,
    ))?;
    check_io(fs::write(
        case.root.join(format!("{label}.stderr")),
        &output.stderr,
    ))?;
    Ok(output)
}

fn aux(entries: &[(u64, u64)], requested: u64) -> TestResult<u64> {
    entries
        .iter()
        .find_map(|(kind, value)| (*kind == requested).then_some(*value))
        .ok_or_else(|| format!("auxv type {requested} missing"))
}

fn header_offsets(image: &[u8]) -> Vec<usize> {
    assert_eq!(u16::from_le_bytes(image[54..56].try_into().unwrap()), 56);
    let count = u16::from_le_bytes(image[56..58].try_into().unwrap()) as usize;
    let start = word(image, 32) as usize;
    (0..count).map(|index| start + index * 56).collect()
}

fn kind(image: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(image[offset..offset + 4].try_into().unwrap())
}

fn word(image: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(image[offset..offset + 8].try_into().unwrap())
}

fn put_word(image: &mut [u8], offset: usize, value: u64) {
    image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
