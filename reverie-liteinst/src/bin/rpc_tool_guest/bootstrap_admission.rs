/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */
//! Real initial and fork-child callback admission through the shared supervisor.
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tid;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::Sysno;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

static ENABLED: AtomicBool = AtomicBool::new(false);
static PHASES: AtomicU64 = AtomicU64::new(0);
static PROBES: AtomicU64 = AtomicU64::new(0);
static RPC_TOTAL: AtomicU64 = AtomicU64::new(0);
static NATIVE_MESSAGE_CONTROL: AtomicBool = AtomicBool::new(false);

unsafe fn raw(number: i64, args: [u64; 6]) -> i64 {
    let result;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") number => result,
            in("rdi") args[0], in("rsi") args[1], in("rdx") args[2],
            in("r10") args[3], in("r8") args[4], in("r9") args[5],
            lateout("rcx") _, lateout("r11") _, options(nostack),
        );
    }
    result
}

fn probe(phase: u64) {
    if !ENABLED.load(Ordering::Acquire) {
        return; // The supervisor's own Config serialization is outside the guest.
    }
    let sockets: Vec<_> = std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter_map(|entry| {
            let target = std::fs::read_link(entry.path()).ok()?;
            target
                .as_os_str()
                .as_encoded_bytes()
                .starts_with(b"socket:[")
                .then(|| {
                    (
                        entry.file_name().to_str().unwrap().parse::<i32>().unwrap(),
                        target,
                    )
                })
        })
        .collect();
    // Initial setup has its actual setup and RPC sockets. Child Config decode
    // also retains the old parent references until the replacement is prepared.
    assert!(sockets.len() >= 2, "actual setup/RPC sockets: {sockets:?}");
    let mut byte = [0x5a_u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    for (fd, identity) in &sockets {
        let mut packet: libc::mmsghdr = unsafe { std::mem::zeroed() };
        packet.msg_hdr.msg_iov = &raw mut iov;
        packet.msg_hdr.msg_iovlen = 1;
        for (number, address, argument2, argument3) in [
            (
                libc::SYS_sendmsg,
                (&raw mut message) as u64,
                (libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) as u64,
                0,
            ),
            (
                libc::SYS_recvmsg,
                (&raw mut message) as u64,
                libc::MSG_DONTWAIT as u64,
                0,
            ),
            (
                libc::SYS_sendmmsg,
                (&raw mut packet) as u64,
                1,
                (libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) as u64,
            ),
            (
                libc::SYS_recvmmsg,
                (&raw mut packet) as u64,
                1,
                libc::MSG_DONTWAIT as u64,
            ),
        ] {
            assert_eq!(
                unsafe {
                    raw(
                        number,
                        [*fd as u64, address, argument2, argument3, 0, 0],
                    )
                },
                -i64::from(libc::ENOTSUP),
                "phase={phase} fd={fd} identity={identity:?} syscall={number}"
            );
        }
        assert_eq!(
            unsafe {
                raw(
                    libc::SYS_shutdown,
                    [*fd as u64, libc::SHUT_RDWR as u64, 0, 0, 0, 0],
                )
            },
            -i64::from(libc::EBADF),
            "phase={phase} fd={fd} identity={identity:?}"
        );
        let mut value = 0_i32;
        let mut size = std::mem::size_of::<i32>() as u32;
        assert_eq!(
            unsafe {
                raw(
                    libc::SYS_getsockopt,
                    [
                        *fd as u64,
                        libc::SOL_SOCKET as u64,
                        libc::SO_TYPE as u64,
                        (&raw mut value) as u64,
                        (&raw mut size) as u64,
                        0,
                    ],
                )
            },
            -i64::from(libc::EBADF)
        );
        for number in [libc::SYS_getsockname, libc::SYS_getpeername] {
            let mut address: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
            size = std::mem::size_of_val(&address) as u32;
            assert_eq!(
                unsafe {
                    raw(
                        number,
                        [
                            *fd as u64,
                            (&raw mut address) as u64,
                            (&raw mut size) as u64,
                            0,
                            0,
                            0,
                        ],
                    )
                },
                -i64::from(libc::EBADF)
            );
        }
        // Preserve the existing virtual close convention and prove below that
        // actual handshake/first callback RPC still works on these references.
        assert_eq!(
            unsafe { raw(libc::SYS_close_range, [*fd as u64, *fd as u64, 0, 0, 0, 0]) },
            0
        );
        println!(
            "admission private phase={phase} fd={fd} identity={}",
            identity.display()
        );
    }
    let (sender, receiver) = UnixStream::pair().unwrap();
    let mut send_packet: libc::mmsghdr = unsafe { std::mem::zeroed() };
    send_packet.msg_hdr.msg_iov = &raw mut iov;
    send_packet.msg_hdr.msg_iovlen = 1;
    for (number, address, argument2, argument3) in [
        (
            libc::SYS_sendmsg,
            (&raw mut message) as u64,
            libc::MSG_NOSIGNAL as u64,
            0,
        ),
        (
            libc::SYS_sendmmsg,
            (&raw mut send_packet) as u64,
            1,
            libc::MSG_NOSIGNAL as u64,
        ),
    ] {
        assert_eq!(
            unsafe {
                raw(
                    number,
                    [
                        sender.as_raw_fd() as u64,
                        address,
                        argument2,
                        argument3,
                        0,
                        0,
                    ],
                )
            },
            -i64::from(libc::ENOTSUP),
            "ordinary message send is categorical after private activation"
        );
    }
    let mut received = [0_u8];
    let mut receive_iov = libc::iovec {
        iov_base: received.as_mut_ptr().cast(),
        iov_len: received.len(),
    };
    let mut receive_message: libc::msghdr = unsafe { std::mem::zeroed() };
    receive_message.msg_iov = &raw mut receive_iov;
    receive_message.msg_iovlen = 1;
    let mut receive_packet: libc::mmsghdr = unsafe { std::mem::zeroed() };
    receive_packet.msg_hdr.msg_iov = &raw mut receive_iov;
    receive_packet.msg_hdr.msg_iovlen = 1;
    for (number, address, argument2, argument3) in [
        (
            libc::SYS_recvmsg,
            (&raw mut receive_message) as u64,
            libc::MSG_DONTWAIT as u64,
            0,
        ),
        (
            libc::SYS_recvmmsg,
            (&raw mut receive_packet) as u64,
            1,
            libc::MSG_DONTWAIT as u64,
        ),
    ] {
        assert_eq!(
            unsafe {
                raw(
                    number,
                    [
                        receiver.as_raw_fd() as u64,
                        address,
                        argument2,
                        argument3,
                        0,
                        0,
                    ],
                )
            },
            -i64::from(libc::ENOTSUP),
            "ordinary message receive is categorical after private activation"
        );
    }
    assert_eq!(received, [0], "refused receive changed the destination");
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_shutdown,
                [sender.as_raw_fd() as u64, libc::SHUT_WR as u64, 0, 0, 0, 0],
            )
        },
        0
    );
    PHASES.fetch_or(phase, Ordering::AcqRel);
    PROBES.fetch_add(1, Ordering::AcqRel);
}

fn preactivation_native_message_control() {
    let (sender, receiver) = UnixStream::pair().unwrap();
    let byte = [0x5a_u8];
    let mut send_iov = libc::iovec {
        iov_base: byte.as_ptr().cast_mut().cast(),
        iov_len: byte.len(),
    };
    let mut send_message: libc::msghdr = unsafe { std::mem::zeroed() };
    send_message.msg_iov = &raw mut send_iov;
    send_message.msg_iovlen = 1;
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_sendmsg,
                [
                    sender.as_raw_fd() as u64,
                    (&raw mut send_message) as u64,
                    libc::MSG_NOSIGNAL as u64,
                    0,
                    0,
                    0,
                ],
            )
        },
        1,
        "preactivation ordinary sendmsg"
    );
    let mut received = [0_u8];
    let mut receive_iov = libc::iovec {
        iov_base: received.as_mut_ptr().cast(),
        iov_len: received.len(),
    };
    let mut receive_message: libc::msghdr = unsafe { std::mem::zeroed() };
    receive_message.msg_iov = &raw mut receive_iov;
    receive_message.msg_iovlen = 1;
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_recvmsg,
                [
                    receiver.as_raw_fd() as u64,
                    (&raw mut receive_message) as u64,
                    libc::MSG_DONTWAIT as u64,
                    0,
                    0,
                    0,
                ],
            )
        },
        1,
        "preactivation ordinary recvmsg"
    );
    assert_eq!(received, byte);
    NATIVE_MESSAGE_CONTROL.store(true, Ordering::Release);
}

#[derive(Debug, Default, Serialize)]
pub(super) struct Config;
impl Clone for Config {
    fn clone(&self) -> Self {
        probe(2);
        Self
    }
}
impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        <()>::deserialize(deserializer)?;
        probe(1);
        Ok(Self)
    }
}

#[derive(Default)]
pub(super) struct Global {
    calls: AtomicU64,
}
#[reverie::global_tool]
impl GlobalTool for Global {
    type Request = i32;
    type Response = u64;
    type Config = Config;
    async fn receive_rpc(&self, from: Tid, pid: i32) -> u64 {
        assert!(pid > 0);
        assert_eq!(from.as_raw(), pid);
        self.calls.fetch_add(1, Ordering::AcqRel) + 1
    }
}

#[derive(Default)]
struct AdmissionTool;
#[reverie::tool]
impl Tool for AdmissionTool {
    type GlobalState = Global;
    type ThreadState = ();
    fn new(_pid: Pid, _config: &Config) -> Self {
        probe(8);
        Self
    }
    fn subscriptions(_config: &Config) -> Subscription {
        probe(4);
        [Sysno::getpid].into_iter().collect()
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        _syscall: Syscall,
    ) -> Result<i64, Error> {
        let pid = guest.pid().as_raw();
        let total = guest.send_rpc(pid).await;
        RPC_TOTAL.store(total, Ordering::Release);
        Ok(i64::from(pid))
    }
}

pub(super) fn run(path: &Path) {
    preactivation_native_message_control();
    ENABLED.store(true, Ordering::Release);
    let original_pid = unsafe { raw(libc::SYS_getpid, [0; 6]) };
    let mut original_mask = 0_u64;
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_rt_sigprocmask,
                [0, 0, (&raw mut original_mask) as u64, 8, 0, 0],
            )
        },
        0
    );
    let seeded_mask =
        original_mask | (1_u64 << (libc::SIGUSR1 - 1)) | (1_u64 << (libc::SIGSYS - 1));
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_SETMASK as u64,
                    (&raw const seeded_mask) as u64,
                    0,
                    8,
                    0,
                    0,
                ],
            )
        },
        0
    );
    unsafe { reverie_liteinst::with_tool_root!({
        unsafe { reverie_liteinst::install_tool::<AdmissionTool>(path) }.unwrap();
    }); }
    assert_eq!(PHASES.load(Ordering::Acquire), 15);
    let mut actual_mask = 0_u64;
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_rt_sigprocmask,
                [0, 0, (&raw mut actual_mask) as u64, 8, 0, 0],
            )
        },
        0
    );
    assert_eq!(actual_mask, seeded_mask & !(1_u64 << (libc::SIGSYS - 1)));
    assert_eq!(unsafe { raw(libc::SYS_getpid, [0; 6]) }, original_pid);
    assert_eq!(RPC_TOTAL.load(Ordering::Acquire), 1);
    // Both processes inherit zero expectations. Fresh child config/new work
    // must populate them; inherited parent success cannot satisfy this control.
    PHASES.store(0, Ordering::Release);
    PROBES.store(0, Ordering::Release);
    let child = unsafe { raw(libc::SYS_fork, [0; 6]) };
    assert!(child >= 0, "raw fork={child}");
    if child == 0 {
        assert_eq!(
            PHASES.load(Ordering::Acquire),
            1 | 8,
            "fresh child Deserialize and T::new"
        );
        assert_eq!(
            PROBES.load(Ordering::Acquire),
            2,
            "exact fresh child callback probes"
        );
        let actual = unsafe { raw(libc::SYS_getpid, [0; 6]) };
        assert!(actual > 0 && actual != original_pid);
        assert_eq!(RPC_TOTAL.load(Ordering::Acquire), 2);
        println!(
            "admission child pid={actual} phases=9 probes={} rpc=2",
            PROBES.load(Ordering::Acquire)
        );
        #[cfg(feature = "rcb-qualification")]
        super::emit_hardware_counter_result("bootstrap-admission-child");
        unsafe {
            raw(libc::SYS_exit_group, [0; 6]);
        }
        unreachable!();
    }
    assert_eq!(
        PHASES.load(Ordering::Acquire),
        0,
        "child callback phases must not mutate parent state"
    );
    assert_eq!(PROBES.load(Ordering::Acquire), 0);
    let mut status = -1_i32;
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_wait4,
                [child as u64, (&raw mut status) as u64, 0, 0, 0, 0],
            )
        },
        child
    );
    assert_eq!(status, 0, "actual child wait status");
    assert_eq!(unsafe { raw(libc::SYS_getpid, [0; 6]) }, original_pid);
    assert_eq!(RPC_TOTAL.load(Ordering::Acquire), 3);
    assert!(NATIVE_MESSAGE_CONTROL.load(Ordering::Acquire));
    assert!(reverie_liteinst::reverie_liteinst_bootstrap_sigsys_count() > 0);
    println!(
        "admission parent pid={original_pid} child={child} phases=15 rpc=3 mask=preserved native-messages=ok active-messages=ENOTSUP trusted-setup-rpc=ok bootstrap-signals={}",
        reverie_liteinst::reverie_liteinst_bootstrap_sigsys_count()
    );
}
