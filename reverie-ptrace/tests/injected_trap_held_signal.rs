/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A guest signal that a binary rewriter's injected syscall trap holds
//! (`TracerBuilder::injected_syscall_trap`) must reach
//! `Tool::handle_signal_event` before the guest resumes, as it does at a
//! seccomp stop (https://github.com/rrnewton/hermit/issues/703).
//!
//! The guest is a static ELF that blocks SIGSYS, queues it to itself with a
//! positive `si_code`, and calls the trap with a frame whose syscall is
//! `ppoll` with an empty temporary mask. Linux dequeues the now-unblocked
//! synchronous SIGSYS ahead of the injection's step SIGTRAP, and Reverie
//! cannot requeue it past a mask-swapping syscall, so the injection holds it.
//! The Tool suppresses it: if the guest were resumed with it, SIGSYS's
//! default action would kill the guest before it writes its marker.

#![cfg(target_arch = "x86_64")]

use std::fs;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Signal;
use reverie::Subscription;
use reverie::Tool;
use reverie::process::Command;
use reverie::process::Stdio;
use reverie::syscalls::Errno;
use reverie::syscalls::Syscall;
use reverie::syscalls::Sysno;
use reverie_ptrace::TracerBuilder;
use reverie_ptrace::testing::run_tokio_test;

const ENTRY: u64 = 0x401000;
const DATA: u64 = 0x402000;
const MARKER: u64 = 0x5452415054455354;
const AFTER: &[u8] = b"guest-continued";
/// Offsets in the e9tool-compatible frame (`InjectedSyscallFrame`).
const FRAME_R10: usize = 48;
const FRAME_R8: usize = 64;
const FRAME_RDX: usize = 104;
const FRAME_RAX: usize = 120;
const FRAME_RIP: usize = 136;
/// Guest memory for the queued siginfo, the SIGSYS set the guest blocks, the
/// `ppoll` timeout and its (empty) temporary mask, and the marker text.
const SIGINFO: u64 = DATA + 0x200;
const SIGSYS_SET: u64 = DATA + 0x280;
const TIMEOUT: u64 = DATA + 0x2a0;
const EMPTY_SET: u64 = DATA + 0x2c0;
const AFTER_AT: u64 = DATA + 0x300;

#[derive(Default)]
struct Signals(Mutex<Vec<i32>>);

#[reverie::global_tool]
impl GlobalTool for Signals {
    type Request = i32;
    type Response = ();
    type Config = bool;

    async fn receive_rpc(&self, _from: Pid, signal: i32) {
        self.0.lock().unwrap().push(signal);
    }
}

/// Suppresses, and reports, any signal the guest does not block.
///
/// Subscribes to `ppoll`, and so intercepts the trap's syscall
/// and injects it, when configured `true`. Otherwise the trap's syscall is
/// unsubscribed and Reverie runs it without a Tool callback.
#[derive(Clone, Copy, Debug, Default)]
struct SuppressUnblocked;

#[reverie::tool]
impl Tool for SuppressUnblocked {
    type GlobalState = Signals;
    type ThreadState = ();

    fn subscriptions(subscribed: &bool) -> Subscription {
        let mut subscription = Subscription::none();
        if *subscribed {
            subscription.syscall(Sysno::ppoll);
        }
        subscription
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        Ok(guest.inject(syscall).await?)
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        // Linux may report the queued SIGSYS while the guest still blocks
        // it; passed on, it is requeued, as it would be untraced.
        if blocked(guest.tid(), signal) {
            return Ok(Some(signal));
        }
        guest.send_rpc(signal as i32).await;
        Ok(None)
    }
}

/// Whether `tid`'s signal mask blocks `signal`.
fn blocked(tid: Pid, signal: Signal) -> bool {
    let status = fs::read_to_string(format!("/proc/{tid}/status")).unwrap();
    let mask = status
        .lines()
        .find_map(|line| line.strip_prefix("SigBlk:"))
        .unwrap();
    let mask = u64::from_str_radix(mask.trim(), 16).unwrap();
    mask & (1 << (signal as i32 - 1)) != 0
}

struct Fixture {
    path: PathBuf,
    trap_rip: u64,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "reverie-injected-trap-held-signal-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let mut code = Vec::new();
        // rt_sigprocmask(SIG_BLOCK, SIGSYS_SET, NULL, 8)
        code.extend_from_slice(&[0xb8, 14, 0, 0, 0, 0x31, 0xff, 0xbe]);
        code.extend_from_slice(&(SIGSYS_SET as u32).to_le_bytes());
        code.extend_from_slice(&[0x31, 0xd2, 0x41, 0xba, 8, 0, 0, 0, 0x0f, 0x05]);
        // rt_tgsigqueueinfo(getpid(), getpid(), SIGSYS, SIGINFO)
        code.extend_from_slice(&[0xb8, 39, 0, 0, 0, 0x0f, 0x05]);
        code.extend_from_slice(&[0x48, 0x89, 0xc7, 0x48, 0x89, 0xc6, 0xba, 31, 0, 0, 0]);
        code.extend_from_slice(&[0x41, 0xba]);
        code.extend_from_slice(&(SIGINFO as u32).to_le_bytes());
        code.extend_from_slice(&[0xb8, 0x29, 0x01, 0, 0, 0x0f, 0x05]);
        code.extend_from_slice(&[0x48, 0xbf]); // movabs rdi, frame
        code.extend_from_slice(&DATA.to_le_bytes());
        code.extend_from_slice(&[0x48, 0x89, 0xa7, 128, 0, 0, 0]); // mov [rdi+128],rsp
        code.extend_from_slice(&[0x48, 0xb8]); // movabs rax, marker
        code.extend_from_slice(&MARKER.to_le_bytes());
        code.push(0xcc);
        let trap_rip = ENTRY + code.len() as u64;
        code.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xbf, 1, 0, 0, 0, 0xbe]); // write(1,
        code.extend_from_slice(&(AFTER_AT as u32).to_le_bytes());
        code.push(0xba);
        code.extend_from_slice(&(AFTER.len() as u32).to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x05]);
        code.extend_from_slice(&[0xb8, 60, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05]); // exit(0)

        let mut elf = vec![0u8; 0x3000];
        elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());
        elf[20..24].copy_from_slice(&1u32.to_le_bytes());
        elf[24..32].copy_from_slice(&ENTRY.to_le_bytes());
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[52..54].copy_from_slice(&64u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&2u16.to_le_bytes());
        for (header, flags, offset, address, length) in [
            (64, 5u32, 0u64, 0x400000u64, 0x1000 + code.len() as u64),
            (120, 6u32, 0x2000u64, DATA, 0x1000),
        ] {
            elf[header..header + 4].copy_from_slice(&1u32.to_le_bytes());
            elf[header + 4..header + 8].copy_from_slice(&flags.to_le_bytes());
            elf[header + 8..header + 16].copy_from_slice(&offset.to_le_bytes());
            elf[header + 16..header + 24].copy_from_slice(&address.to_le_bytes());
            elf[header + 24..header + 32].copy_from_slice(&address.to_le_bytes());
            elf[header + 32..header + 40].copy_from_slice(&length.to_le_bytes());
            elf[header + 40..header + 48].copy_from_slice(&length.to_le_bytes());
            elf[header + 48..header + 56].copy_from_slice(&0x1000u64.to_le_bytes());
        }
        elf[0x1000..0x1000 + code.len()].copy_from_slice(&code);
        let data = 0x2000;
        let put = |elf: &mut Vec<u8>, at: usize, value: u64| {
            elf[data + at..data + at + 8].copy_from_slice(&value.to_le_bytes());
        };
        // ppoll(NULL, 0, TIMEOUT, EMPTY_SET, 8)
        put(&mut elf, FRAME_RAX, libc::SYS_ppoll as u64);
        put(&mut elf, FRAME_RDX, TIMEOUT);
        put(&mut elf, FRAME_R10, EMPTY_SET);
        put(&mut elf, FRAME_R8, 8);
        put(&mut elf, FRAME_RIP, ENTRY);
        put(
            &mut elf,
            (SIGSYS_SET - DATA) as usize,
            1 << (libc::SIGSYS - 1),
        );
        put(&mut elf, (TIMEOUT - DATA) as usize, 5);
        let siginfo = (SIGINFO - DATA) as usize + data;
        elf[siginfo..siginfo + 4].copy_from_slice(&libc::SIGSYS.to_le_bytes());
        elf[siginfo + 8..siginfo + 12].copy_from_slice(&1i32.to_le_bytes()); // si_code
        let after = (AFTER_AT - DATA) as usize + data;
        elf[after..after + AFTER.len()].copy_from_slice(AFTER);

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(&elf).unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o700))
            .unwrap();
        Self { path, trap_rip }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_file(&self.path).unwrap();
    }
}

fn run(subscribed: bool) {
    let fixture = Fixture::new();
    let mut command = Command::new(&fixture.path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (output, signals) = run_tokio_test(async {
        TracerBuilder::<SuppressUnblocked>::new(command)
            .config(subscribed)
            .injected_syscall_trap(MARKER, fixture.trap_rip)
            .spawn()
            .await
            .unwrap()
            .wait_with_output()
            .await
            .unwrap()
    });
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, AFTER);
    assert_eq!(
        *signals.0.lock().unwrap(),
        vec![libc::SIGSYS],
        "the held SIGSYS is reported to the tool, which suppresses it"
    );
}

#[test]
fn unsubscribed_trap_syscall_reports_its_held_signal_to_the_tool() {
    run(false);
}

#[test]
fn subscribed_trap_syscall_reports_its_held_signal_to_the_tool() {
    run(true);
}
