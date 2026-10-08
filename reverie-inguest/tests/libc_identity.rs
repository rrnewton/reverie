/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `record_libc_identity` must find the C library the process actually runs
//! on, which the loader chooses by `DT_SONAME`, not by file name, and must
//! find it without running code the program can substitute. Each case re-runs
//! this test binary with `LD_PRELOAD` arranging the libraries; the re-run
//! process records the identity, checks that the C library's own signal
//! restorer is accepted and that recording left the C library's heap as it
//! was, and reports which file it actually runs the C library from and which
//! of the preloaded files it mapped, so a preload the loader ignored cannot
//! pass for one it used.
//!
//! - the process's own C library;
//! - a copy of it named `libc-custom.so` (the loader uses it as the C library);
//! - that copy and an unrelated library named `helper/libc.so.6`;
//! - a preloaded `dl_iterate_phdr` that allocates and delegates;
//! - a preloaded `dl_iterate_phdr` that reports a fake C library (an object
//!   named `libc.so.6` with `DT_SONAME` `libc.so.6` whose code is its own) and
//!   hides the real one.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const CHILD: &str = "REVERIE_TEST_LIBC_IDENTITY_CHILD";
const TEST: &str = "libc_identity_follows_the_loader_not_file_names_or_interposers";

extern "C" fn ignore(_signal: libc::c_int) {}

/// The file and identity of the mapping containing `address`, from
/// `/proc/self/maps`.
fn mapping_file(address: u64) -> Option<(String, String)> {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    maps.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (start, end) = fields.first()?.split_once('-')?;
        let range = u64::from_str_radix(start, 16).ok()?..u64::from_str_radix(end, 16).ok()?;
        (range.contains(&address) && fields.len() >= 6)
            .then(|| (fields[5].to_owned(), format!("{} {}", fields[3], fields[4])))
    })
}

/// The restorer glibc's `sigaction` installs: an address in the C library the
/// process actually runs.
fn glibc_restorer() -> u64 {
    let mut action: libc::sigaction = unsafe { core::mem::zeroed() };
    action.sa_sigaction = ignore as *const () as usize;
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGUSR1, &action, core::ptr::null_mut()) },
        0
    );
    let mut installed = reverie_inguest::signal::KernelSigaction::default();
    unsafe { reverie_inguest::signal::raw_sigaction(libc::SIGUSR1, None, Some(&mut installed)) }
        .unwrap();
    installed.restorer
}

/// In the re-run process: record the identity and report.
fn child_report() {
    // SAFETY: mallinfo2 only reads the allocator's statistics.
    let before = unsafe { libc::mallinfo2() };
    let recorded = reverie_inguest::guest::restorer::record_libc_identity();
    let after = unsafe { libc::mallinfo2() };
    let heap_unchanged = (before.arena, before.uordblks, before.hblkhd)
        == (after.arena, after.uordblks, after.hblkhd);
    let restorer = glibc_restorer();
    let accepted = unsafe { reverie_inguest::guest::restorer::glibc_restorer_accepted(restorer) };
    let (libc_file, _) = mapping_file(restorer).unwrap_or_default();
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let preloaded = std::env::var("LD_PRELOAD").unwrap_or_default();
    let mapped = preloaded
        .split(' ')
        .filter(|path| !path.is_empty())
        .map(|path| maps.contains(path))
        .collect::<Vec<_>>();
    // How often a preloaded dl_iterate_phdr ran (read after the measurement).
    let counter = unsafe {
        libc::dlsym(
            libc::RTLD_DEFAULT,
            c"reverie_test_interposer_calls".as_ptr(),
        )
    }
    .cast::<u32>();
    let interposer_calls = if counter.is_null() {
        0
    } else {
        unsafe { counter.read() }
    };
    let recorded = match recorded {
        Ok(()) => "recorded".to_owned(),
        Err(error) => format!("refused ({error})"),
    };
    println!(
        "identity: {recorded} accepted={accepted} heap_unchanged={heap_unchanged} \
         interposer_calls={interposer_calls} preloads_mapped={mapped:?} libc_file={libc_file}"
    );
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
        // One malloc arena, so mallinfo2 (which reports the main arena)
        // sees allocations made on the harness's test thread too.
        .env("MALLOC_ARENA_MAX", "1")
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

fn compile_library(source: &str, library: &Path) {
    let source_path = library.with_extension("c");
    std::fs::write(&source_path, source).unwrap();
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let output = Command::new(compiler)
        .args(["-O2", "-Wall", "-Werror", "-shared", "-fPIC", "-o"])
        .arg(library)
        .arg(&source_path)
        .arg("-ldl")
        .output()
        .expect("failed to run the C compiler");
    assert!(output.status.success(), "{output:?}");
}

/// Allocates through the C library's malloc on every call, then delegates.
const ALLOCATING_WRAPPER: &str = r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <link.h>
#include <stdlib.h>
typedef int (*callback_t)(struct dl_phdr_info *, size_t, void *);
/* Exported, so the compiler cannot drop the allocations as unused. */
void *reverie_test_kept[64];
unsigned reverie_test_interposer_calls;
int dl_iterate_phdr(callback_t callback, void *data) {
    reverie_test_kept[reverie_test_interposer_calls++ % 64] = malloc(4096);
    int (*real)(callback_t, void *) = (int (*)(callback_t, void *))dlsym(RTLD_NEXT, "dl_iterate_phdr");
    return real(callback, data);
}
"#;

/// Reports a fake C library first (named libc.so.6, DT_SONAME libc.so.6, its
/// executable segment this library's own code) and hides the real one.
const FAKE_IDENTITY_WRAPPER: &str = r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <elf.h>
#include <link.h>
#include <string.h>
typedef int (*callback_t)(struct dl_phdr_info *, size_t, void *);
static struct {
    ElfW(Dyn) dyn[3];
    char strings[16];
} fake = {{{DT_STRTAB, {0}}, {DT_SONAME, {1}}, {DT_NULL, {0}}}, "\0libc.so.6"};
static ElfW(Phdr) headers[3];
void reverie_test_fake_libc_code(void) {}
unsigned reverie_test_interposer_calls;
struct relay {
    callback_t callback;
    void *data;
};
static int hide_libc(struct dl_phdr_info *info, size_t size, void *data) {
    struct relay *relay = data;
    if (info->dlpi_name != NULL && strstr(info->dlpi_name, "libc") != NULL) {
        return 0;
    }
    return relay->callback(info, size, relay->data);
}
int dl_iterate_phdr(callback_t callback, void *data) {
    reverie_test_interposer_calls++;
    fake.dyn[0].d_un.d_ptr = (ElfW(Addr))fake.strings;
    headers[0] = (ElfW(Phdr)){.p_type = PT_LOAD, .p_flags = PF_R | PF_X,
                              .p_vaddr = (ElfW(Addr))reverie_test_fake_libc_code, .p_memsz = 16};
    headers[1] = (ElfW(Phdr)){.p_type = PT_LOAD, .p_flags = PF_R,
                              .p_vaddr = (ElfW(Addr))&fake, .p_memsz = sizeof fake};
    headers[2] = (ElfW(Phdr)){.p_type = PT_DYNAMIC, .p_flags = PF_R,
                              .p_vaddr = (ElfW(Addr))fake.dyn, .p_memsz = sizeof fake.dyn};
    struct dl_phdr_info info;
    memset(&info, 0, sizeof info);
    info.dlpi_name = "/fake/libc.so.6";
    info.dlpi_phdr = headers;
    info.dlpi_phnum = 3;
    int result = callback(&info, sizeof info, data);
    if (result != 0) {
        return result;
    }
    int (*real)(callback_t, void *) = (int (*)(callback_t, void *))dlsym(RTLD_NEXT, "dl_iterate_phdr");
    struct relay relay = {callback, data};
    return real(hide_libc, &relay);
}
"#;

#[test]
fn libc_identity_follows_the_loader_not_file_names_or_interposers() {
    if std::env::var_os(CHILD).is_some() {
        child_report();
        return;
    }
    // LD_PRELOAD separates entries with spaces and colons, so the copies must
    // live where neither appears; refuse rather than run cases the loader
    // would silently ignore.
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("libc-identity-{}", std::process::id()));
    let spelled = directory.to_str().expect("a UTF-8 scratch path");
    assert!(
        !spelled.contains([' ', ':', '\t', '\n']),
        "LD_PRELOAD cannot name files under {spelled}"
    );
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = RemoveOnDrop(directory.clone());
    std::fs::create_dir_all(directory.join("helper")).unwrap();

    // The C library this process runs: the file whose code holds glibc's
    // restorer, whatever it is named.
    let (libc, _) = mapping_file(glibc_restorer()).expect("the restorer is in a file mapping");
    // Any other shared library this process maps, with a DT_SONAME of its own.
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let other = maps
        .lines()
        .filter_map(|line| line.split_whitespace().nth(5))
        .find(|path| {
            path.starts_with('/')
                && *path != libc
                && path.contains(".so")
                && !Path::new(path)
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("ld-"))
        })
        .expect("this process maps another shared library")
        .to_owned();
    let renamed = directory.join("libc-custom.so");
    std::fs::copy(&libc, &renamed).unwrap();
    let impostor = directory.join("helper/libc.so.6");
    std::fs::copy(&other, &impostor).unwrap();
    let allocating = directory.join("allocating-dl-iterate-phdr.so");
    compile_library(ALLOCATING_WRAPPER, &allocating);
    let fake = directory.join("fake-libc-dl-iterate-phdr.so");
    compile_library(FAKE_IDENTITY_WRAPPER, &fake);

    let expect = |preloads: &str, file: &dyn std::fmt::Display| {
        format!(
            "identity: recorded accepted=true heap_unchanged=true interposer_calls=0 \
             preloads_mapped={preloads} libc_file={file}"
        )
    };
    assert_eq!(run_child(&[]), expect("[]", &libc));
    assert_eq!(
        run_child(&[&renamed]),
        expect("[true]", &renamed.display()),
        "copy of {libc}"
    );
    assert_eq!(
        run_child(&[&renamed, &impostor]),
        expect("[true, true]", &renamed.display()),
        "impostor is a copy of {other}"
    );
    assert_eq!(run_child(&[&allocating]), expect("[true]", &libc));
    assert_eq!(run_child(&[&fake]), expect("[true]", &libc));
}
