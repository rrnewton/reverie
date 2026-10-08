/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `record_libc_identity` must find the C library the process actually runs
//! on, which the loader chooses by `DT_SONAME`, not by file name: a copy of
//! the C library loaded under another name is still the C library, and an
//! unrelated library named `libc.so.6` is not. Each case re-runs this test
//! binary with `LD_PRELOAD` arranging the libraries, records the identity
//! there, and checks that the C library's own signal restorer is accepted.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const CHILD: &str = "REVERIE_TEST_LIBC_IDENTITY_CHILD";
const TEST: &str = "libc_identity_follows_the_soname_not_the_file_name";

extern "C" fn ignore(_signal: libc::c_int) {}

/// In the re-run process: record the identity, install a handler through the
/// C library (which supplies its own restorer), and report both.
fn child_report() {
    let recorded = reverie_inguest::guest::restorer::record_libc_identity();
    let mut action: libc::sigaction = unsafe { core::mem::zeroed() };
    action.sa_sigaction = ignore as *const () as usize;
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGUSR1, &action, core::ptr::null_mut()) },
        0
    );
    let mut installed = reverie_inguest::signal::KernelSigaction::default();
    unsafe { reverie_inguest::signal::raw_sigaction(libc::SIGUSR1, None, Some(&mut installed)) }
        .unwrap();
    let accepted =
        unsafe { reverie_inguest::guest::restorer::glibc_restorer_accepted(installed.restorer) };
    match recorded {
        Ok(()) => println!("identity: recorded accepted={accepted}"),
        Err(error) => println!("identity: refused ({error}) accepted={accepted}"),
    }
}

/// The files of this process's mappings, by path.
fn mapped_files() -> Vec<PathBuf> {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let mut files: Vec<PathBuf> = maps
        .lines()
        .filter_map(|line| line.split_whitespace().nth(5))
        .filter(|path| path.starts_with('/'))
        .map(PathBuf::from)
        .collect();
    files.dedup();
    files
}

fn run_child(preload: &[&Path]) -> String {
    let preload = preload
        .iter()
        .map(|path| path.to_str().unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("LD_PRELOAD", &preload)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "LD_PRELOAD={preload}: {output:?}");
    // The harness prints the report after the test's name, on its line.
    stdout
        .lines()
        .find_map(|line| {
            line.find("identity: ")
                .map(|start| line[start..].to_owned())
        })
        .unwrap_or_else(|| panic!("LD_PRELOAD={preload}: no report in {stdout}"))
}

#[test]
fn libc_identity_follows_the_soname_not_the_file_name() {
    if std::env::var_os(CHILD).is_some() {
        child_report();
        return;
    }
    let files = mapped_files();
    let libc = files
        .iter()
        .find(|path| path.file_name().is_some_and(|name| name == "libc.so.6"))
        .expect("this process maps libc.so.6");
    // Any other shared library this process maps, with a SONAME of its own.
    let other = files
        .iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.contains(".so") && !name.starts_with("libc.so") && !name.starts_with("ld-")
                })
        })
        .expect("this process maps another shared library");
    let directory =
        std::env::temp_dir().join(format!("reverie-libc-identity-{}", std::process::id()));
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = RemoveOnDrop(directory.clone());
    std::fs::create_dir_all(directory.join("helper")).unwrap();
    let renamed = directory.join("libc-custom.so");
    std::fs::copy(libc, &renamed).unwrap();
    let impostor = directory.join("helper/libc.so.6");
    std::fs::copy(other, &impostor).unwrap();

    // The process's own C library, named as usual.
    assert_eq!(run_child(&[]), "identity: recorded accepted=true");
    // The C library loaded from a file with another name.
    assert_eq!(
        run_child(&[&renamed]),
        "identity: recorded accepted=true",
        "copy of {}",
        libc.display()
    );
    // ... and an unrelated library named libc.so.6 beside it.
    assert_eq!(
        run_child(&[&renamed, &impostor]),
        "identity: recorded accepted=true",
        "impostor is a copy of {}",
        other.display()
    );
}
