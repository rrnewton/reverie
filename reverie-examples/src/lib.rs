/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! LiteInst hosting support for the production Reverie example tools.

#![forbid(unsafe_op_in_unsafe_fn)]
// The reused production tool sources each declare the same test-only KVM helper.
#![allow(clippy::duplicate_mod)]

use std::ffi::CStr;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;

#[allow(dead_code)]
#[path = "../counter1.rs"]
mod counter1;
#[allow(dead_code)]
#[path = "../noop.rs"]
mod noop;
#[allow(dead_code)]
#[path = "../strace/main.rs"]
pub(crate) mod strace;

pub(crate) use strace::config;
pub(crate) use strace::filter;
pub(crate) use strace::global_state;

const TOOL_ENV: &CStr = c"REVERIE_LITEINST_EXAMPLE_TOOL";
const COORDINATOR_ENV: &CStr = c"REVERIE_LITEINST_COORDINATOR";

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-139): Review the example-tool preload constructor and selector boundary.
#[used]
#[unsafe(link_section = ".init_array")]
static LITEINST_EXAMPLE_INIT: unsafe extern "C" fn(
    libc::c_int,
    *mut *mut libc::c_char,
    *mut *mut libc::c_char,
) = initialize;

unsafe fn loaded_as_preload() -> bool {
    let mut info = MaybeUninit::<libc::Dl_info>::uninit();
    let found = unsafe {
        libc::dladdr(
            initialize as *const () as *const libc::c_void,
            info.as_mut_ptr(),
        )
    };
    if found == 0 {
        return false;
    }
    let info = unsafe { info.assume_init() };
    if info.dli_fname.is_null() {
        return false;
    }
    let mapped_name = unsafe { CStr::from_ptr(info.dli_fname) }.to_bytes();
    if mapped_name.is_empty() {
        return false;
    }
    let mapped_path = Path::new(OsStr::from_bytes(mapped_name));
    match (mapped_path.canonicalize(), std::env::current_exe()) {
        (Ok(mapped_path), Ok(executable)) => mapped_path != executable,
        _ => false,
    }
}

unsafe extern "C" fn initialize(
    _argc: libc::c_int,
    _argv: *mut *mut libc::c_char,
    environment: *mut *mut libc::c_char,
) {
    if !unsafe { loaded_as_preload() } {
        return;
    }
    let Some(socket) = (unsafe { take_initial_environment(environment, COORDINATOR_ENV) }) else {
        return;
    };
    let Some(selected) = (unsafe { take_initial_environment(environment, TOOL_ENV) }) else {
        fail("example tool selector is missing");
    };

    let result = match selected.to_str() {
        Some("counter1") => unsafe {
            reverie_liteinst::install_tool::<counter1::CounterLocal>(PathBuf::from(&socket))
        },
        Some("strace") => unsafe {
            reverie_liteinst::install_tool::<strace::Strace>(PathBuf::from(&socket))
        },
        Some("noop") => unsafe {
            reverie_liteinst::install_tool::<noop::NoopTool>(PathBuf::from(&socket))
        },
        Some(other) => fail(&format!("unknown example tool {other:?}")),
        None => fail("example tool selector is not valid UTF-8"),
    };
    if let Err(error) = result {
        fail(&format!("tool initialization failed: {error}"));
    }
}

unsafe fn take_initial_environment(
    environment: *mut *mut libc::c_char,
    name: &CStr,
) -> Option<OsString> {
    let mut slot = environment;
    while !unsafe { (*slot).is_null() } {
        let entry = unsafe { CStr::from_ptr(*slot) };
        let bytes = entry.to_bytes();
        if let Some(value) = bytes
            .strip_prefix(name.to_bytes())
            .and_then(|suffix| suffix.strip_prefix(b"="))
        {
            let value = OsString::from_vec(value.to_vec());
            let length = bytes.len();
            unsafe { ptr::write_bytes(*slot, 0, length) };
            return Some(value);
        }
        slot = unsafe { slot.add(1) };
    }
    None
}

fn fail(message: &str) -> ! {
    eprintln!("reverie-liteinst-examples: {message}");
    unsafe { libc::_exit(127) }
}
