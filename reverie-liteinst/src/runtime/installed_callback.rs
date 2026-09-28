/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * Licensed under the BSD-style license in the repository root LICENSE.
 */

//! The installer accepts only handles constructed here, so an omitted raw
//! callback cannot become an unmeasured installation refusal. Every constructor
//! below also emits its wrapper entry and the inspector's actual table row.

use core::arch::global_asm;

use liteinst2::trampoline::HookCallback;
use liteinst2::trampoline::HookContext;

unsafe extern "C" {
    fn reverie_preload_rcb_callback(
        context: *mut libc::c_void,
        body: unsafe extern "C" fn(*mut libc::c_void),
        mode: u32,
    );
}

/// Only this module can construct a callback accepted by the site installer.
#[derive(Clone, Copy)]
pub(super) struct InstalledCallback(HookCallback);

impl InstalledCallback {
    pub(super) fn entry(self) -> HookCallback {
        self.0
    }
}

#[derive(Clone, Copy)]
#[repr(u32)]
enum CallbackMode {
    InProcess = 0,
    // The ptrace host owns its own counter. Only when the private event is
    // UNAVAILABLE may its callback avoid private clock/TLS/owner operations.
    // An active or incomplete private event still uses the strict boundary.
    Host = 1,
}

#[repr(C)]
#[cfg(feature = "rcb-qualification")]
struct Callback {
    entry: HookCallback,
    body: HookCallback,
    entry_bytes: usize,
    mode: usize,
}

#[cfg(feature = "rcb-qualification")]
const _: () = {
    assert!(core::mem::size_of::<Callback>() == 32);
    assert!(core::mem::offset_of!(Callback, entry) == 0);
    assert!(core::mem::offset_of!(Callback, body) == 8);
    assert!(core::mem::offset_of!(Callback, entry_bytes) == 16);
    assert!(core::mem::offset_of!(Callback, mode) == 24);
};

macro_rules! callbacks {
    ($($handle:ident => $entry:ident, $body:path, $mode:ident;)+) => {
        $(
            unsafe extern "C" {
                #[link_name = concat!("reverie_liteinst_", stringify!($entry))]
                fn $entry(context: *mut HookContext);
            }
            pub(super) const $handle: InstalledCallback = InstalledCallback($entry);
            global_asm!(concat!(
                ".text\n.p2align 4\n.global reverie_liteinst_", stringify!($entry),
                "\n.hidden reverie_liteinst_", stringify!($entry),
                "\n.type reverie_liteinst_", stringify!($entry), ",@function\n",
                "reverie_liteinst_", stringify!($entry), ":\n",
                "endbr64\nlea rsi,[rip+{body}]\nmov edx,{mode}\njmp {wrapper}\n",
                ".size reverie_liteinst_", stringify!($entry), ",.-reverie_liteinst_", stringify!($entry), "\n"
            ), body = sym $body, mode = const CallbackMode::$mode as u32,
                wrapper = sym reverie_preload_rcb_callback);
        )+
        #[cfg(feature = "rcb-qualification")]
        #[unsafe(export_name = "reverie_liteinst_installed_callbacks")]
        static INSTALLED_CALLBACKS: [Callback; [$(stringify!($handle)),+].len()] = [
            $(Callback { entry: $entry, body: $body, entry_bytes: 21,
                mode: CallbackMode::$mode as usize }),+
        ];
    };
}

callbacks! {
    CPUID => installed_cpuid_hook, super::installed_cpuid_hook_body, InProcess;
    RDTSC => installed_rdtsc_hook, super::installed_rdtsc_hook_body, InProcess;
    RDTSCP => installed_rdtscp_hook, super::installed_rdtscp_hook_body, InProcess;
    SYSCALL => installed_syscall_hook, super::installed_syscall_hook_body, InProcess;
    HOST_SYSCALL => installed_host_syscall_hook, super::host_syscall_hook_body, Host;
    VDSO_TIME => installed_vdso_time_hook, super::installed_vdso_time_hook_body, InProcess;
    VDSO_CLOCK_GETTIME => installed_vdso_clock_gettime_hook, super::installed_vdso_clock_gettime_hook_body, InProcess;
    VDSO_GETCPU => installed_vdso_getcpu_hook, super::installed_vdso_getcpu_hook_body, InProcess;
    VDSO_GETTIMEOFDAY => installed_vdso_gettimeofday_hook, super::installed_vdso_gettimeofday_hook_body, InProcess;
    VDSO_CLOCK_GETRES => installed_vdso_clock_getres_hook, super::installed_vdso_clock_getres_hook_body, InProcess;
}

#[cfg(feature = "rcb-qualification")]
pub(super) fn table() -> (u64, Vec<[u64; 4]>) {
    (
        INSTALLED_CALLBACKS.as_ptr() as usize as u64,
        INSTALLED_CALLBACKS
            .iter()
            .map(|entry| {
                [
                    entry.entry as usize as u64,
                    entry.body as usize as u64,
                    entry.entry_bytes as u64,
                    entry.mode as u64,
                ]
            })
            .collect(),
    )
}
