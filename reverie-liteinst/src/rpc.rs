//! Coordinator RPC adapter for in-guest Reverie tools.

use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicI32;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::OnceLock;

use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Pid;
pub(crate) use reverie_preload::sync::SpinMutex;
use reverie_preload::trap::raw_syscall6;
use reverie_ptrace::DisabledRcbEvent;
use reverie_ptrace::InGuestRcbCounter;
use reverie_ptrace::RcbEventDescription;
use reverie_ptrace::RcbPmuProfile;
use reverie_rpc_transport::BlockingRpcClient;

use crate::control::Fd;
use crate::control::Packet;
use crate::control::Reservation;
use crate::control::TrustedStream;
use crate::control::{self};

static FORKED_SINCE_LAST_RPC: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "rcb-qualification")]
static QUALIFICATION_SETUP_FD: AtomicI32 = AtomicI32::new(-1);
#[cfg(feature = "rcb-qualification")]
static QUALIFICATION_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CpuBinding {
    cpu: u32,
    profile: Option<RcbPmuProfile>,
}

fn singleton_affinity() -> io::Result<u32> {
    let mut words = [0_u64; 16];
    let result = unsafe {
        raw_syscall6(
            libc::SYS_sched_getaffinity,
            [
                0,
                core::mem::size_of_val(&words) as u64,
                words.as_mut_ptr() as u64,
                0,
                0,
                0,
            ],
        )
    };
    if result < 0 {
        let errno = i32::try_from(-result).unwrap_or(libc::EIO);
        return Err(if errno == libc::EINVAL {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "kernel CPU affinity mask exceeds the authenticated 1024-bit wire format",
            )
        } else {
            io::Error::from_raw_os_error(errno)
        });
    }
    let mut selected = None;
    for (word_index, word) in words.into_iter().enumerate() {
        if word == 0 {
            continue;
        }
        if word.count_ones() != 1 || selected.is_some() {
            return Err(control::invalid("LiteInst target affinity is not singleton"));
        }
        selected = Some((word_index * 64 + word.trailing_zeros() as usize) as u32);
    }
    selected.ok_or_else(|| control::invalid("LiteInst target affinity is empty"))
}

fn current_cpu() -> io::Result<u32> {
    let mut cpu = u32::MAX;
    let result = unsafe {
        raw_syscall6(
            libc::SYS_getcpu,
            [(&raw mut cpu) as u64, 0, 0, 0, 0, 0],
        )
    };
    if result == 0 && cpu != u32::MAX {
        Ok(cpu)
    } else {
        Err(io::Error::from_raw_os_error(if result < 0 {
            (-result) as i32
        } else {
            libc::EIO
        }))
    }
}

impl CpuBinding {
    fn capture() -> io::Result<Self> {
        let cpu = singleton_affinity()?;
        if current_cpu()? != cpu {
            return Err(control::invalid(
                "LiteInst target is not running on its singleton CPU",
            ));
        }
        let profile = DisabledRcbEvent::native_profile();
        let value = Self { cpu, profile };
        value.verify()?;
        Ok(value)
    }

    fn verify(self) -> io::Result<()> {
        if singleton_affinity()? != self.cpu || current_cpu()? != self.cpu {
            Err(control::invalid(
                "LiteInst target migrated or changed singleton CPU affinity",
            ))
        } else {
            Ok(())
        }
    }
}

/// The first child-side flag store, before any child RPC/configuration callback.
/// Acquisition never uses the inherited parent's connection or stored TID.
pub(crate) fn note_fork_in_child() {
    FORKED_SINCE_LAST_RPC.store(true, Ordering::Release);
}

struct RpcConnection<G: GlobalTool> {
    pid: Pid,
    client: BlockingRpcClient<G, TrustedStream>,
    setup: Fd,
    generation: u64,
    event_id: u64,
    acquired: bool,
}

/// Supervisor disposition after it has matched the locally captured native
/// profile. The root authenticates this once and an admitted fork inherits it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthenticatedRcbProfile {
    Unsupported(CpuBinding),
    Event(CpuBinding),
}

/// Blocking Tool RPC over an independently issued per-process stream.
/// Public Tool/GlobalRPC request and response types remain unchanged. The
/// matched supervisor supplies an authenticated setup bootstrap at real spawn.
/// Thread-style clone still requires a separate per-thread birth/connection.
pub struct CoordinatorRpc<G: GlobalTool> {
    connection: SpinMutex<RpcConnection<G>>,
    config: G::Config,
    fd: AtomicI32,
    prepared_fork: SpinMutex<Option<(Fd, u64)>>,
    cpu_binding: CpuBinding,
    authenticated_rcb_profile: OnceLock<AuthenticatedRcbProfile>,
}

fn connect_endpoint<G: GlobalTool>(
    setup: Fd,
    expected_generation: Option<u64>,
) -> io::Result<RpcConnection<G>> {
    let pid = current_id(libc::SYS_getpid)?;
    let tid = current_id(libc::SYS_gettid)?;
    if pid != tid {
        return Err(control::invalid(
            "nonleader Tool setup needs a thread birth record",
        ));
    }
    let pidfd = control::self_pidfd(raw_syscall6, true)?;
    let mut hello = Packet::new(control::HELLO, 0);
    hello.0[4] = pid.as_raw() as u64;
    hello.0[5] = tid.as_raw() as u64;
    control::send(&setup, hello, &[pidfd.as_raw_fd()], false)?;
    drop(pidfd);
    let (offer, stream) = control::receive(&setup, true, false)?.only_fd()?;
    let generation = offer.0[2];
    offer.require(control::RPC, generation)?;
    if expected_generation.is_some_and(|expected| expected != generation)
        || generation == 0
        || offer.0[5] != pid.as_raw() as u64
        || offer.0[4] == 0
    {
        return Err(control::invalid("RPC stream incarnation mismatch"));
    }
    // Stream I/O alone uses the trusted gate. The FD remains protected during
    // config decode and every subsequent serialization/deserialization callback.
    let client = BlockingRpcClient::from_connected_stream(TrustedStream(stream), tid)
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(RpcConnection {
        pid,
        client,
        setup,
        generation,
        event_id: 0,
        acquired: false,
    })
}

impl<G: GlobalTool> CoordinatorRpc<G> {
    pub(crate) fn raw_fd(&self) -> libc::c_int {
        self.fd.load(Ordering::Acquire)
    }

    /// Connect under the prepared bootstrap dispatcher using the matched supervisor.
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        // This is the one client-side PMU discovery. Installation has not yet
        // enabled instruction faulting. A successful fork inherits this value.
        // A separately launched image constructs a new CoordinatorRpc only
        // after the prior process/event lifetime has ended; this runtime
        // currently refuses an in-place guest exec.
        let cpu_binding = CpuBinding::capture()?;
        let connection = connect_endpoint::<G>(
            control::take_bootstrap(path.as_ref(), raw_syscall6)?,
            Some(1),
        )?;
        let config = connection.client.config().clone();
        let fd = connection.client.as_raw_fd();
        #[cfg(feature = "rcb-qualification")]
        {
            QUALIFICATION_SETUP_FD.store(connection.setup.as_raw_fd(), Ordering::Release);
            QUALIFICATION_GENERATION.store(connection.generation, Ordering::Release);
        }
        Ok(Self {
            connection: SpinMutex::new(connection),
            config,
            fd: AtomicI32::new(fd),
            prepared_fork: SpinMutex::new(None),
            cpu_binding,
            authenticated_rcb_profile: OnceLock::new(),
        })
    }

    pub(crate) fn prepare_fork(&self) -> io::Result<()> {
        self.cpu_binding.verify()?;
        let mut prepared = self.prepared_fork.lock();
        if prepared.is_some() {
            return Err(control::invalid("nested prepared fork reservation"));
        }
        let connection = self.connection.lock();
        if FORKED_SINCE_LAST_RPC.load(Ordering::Acquire) || !connection.acquired {
            return Err(control::invalid(
                "fork requested before current incarnation acquisition",
            ));
        }
        if self.authenticated_rcb_profile.get().is_none() {
            return Err(control::invalid(
                "fork requested before root PMU profile binding",
            ));
        }
        control::send(
            &connection.setup,
            Packet::new(control::PREPARE_FORK, connection.generation),
            &[],
            false,
        )?;
        let (packet, endpoint) = control::receive(&connection.setup, true, false)?.only_fd()?;
        packet.require(control::FORK_CHANNEL, connection.generation)?;
        if packet.0[4] == 0 || packet.0[4] == connection.generation {
            return Err(control::invalid("invalid child reservation generation"));
        }
        #[cfg(feature = "rcb-qualification")]
        crate::runtime::probe_prepared_fork_endpoint_for_test(
            endpoint.as_raw_fd(),
            packet.0[4],
        )?;
        *prepared = Some((endpoint, packet.0[4]));
        Ok(())
    }

    /// Parent success or native fork failure: close only the unused child end.
    /// EOF cancels an unclaimed reservation without rewriting the native result.
    pub(crate) fn discard_prepared_fork(&self) {
        drop(self.prepared_fork.lock().take());
    }

    pub(crate) fn rebind_fork_child(&self) -> io::Result<()> {
        if !FORKED_SINCE_LAST_RPC.load(Ordering::Acquire) {
            return Err(control::invalid("child rebind without physical fork"));
        }
        if self.authenticated_rcb_profile.get().is_none() {
            return Err(control::invalid(
                "fork child inherited no authenticated root PMU profile",
            ));
        }
        // Fork inherits the singleton affinity and captured profile. Reattest
        // them without CPUID before any child setup or Tool callback.
        self.cpu_binding.verify()?;
        let (endpoint, generation) = self
            .prepared_fork
            .lock()
            .take()
            .ok_or_else(|| control::invalid("child has no pre-fork birth endpoint"))?;
        let old_pid = self.connection.lock().pid;
        let current = current_id(libc::SYS_getpid)?;
        if old_pid == current {
            return Err(control::invalid("parent tried to claim its child birth"));
        }
        // Deserialize the child Config with no generic transport mutex held.
        // Its ordinary syscalls remain subject to nested callback admission.
        let replacement = connect_endpoint::<G>(endpoint, Some(generation))?;
        let new_fd = replacement.client.as_raw_fd();
        let retired = {
            let mut old = self.connection.lock();
            if old.pid != old_pid || !FORKED_SINCE_LAST_RPC.load(Ordering::Acquire) {
                return Err(control::invalid("child connection changed during rebind"));
            }
            let old_fd = self.fd.load(Ordering::Acquire);
            crate::runtime::replace_coordinator_fd(old_fd, new_fd)?;
            self.fd.store(new_fd, Ordering::Release);
            std::mem::replace(&mut *old, replacement)
        };
        FORKED_SINCE_LAST_RPC.store(false, Ordering::Release);
        // The old Config destructor also runs outside the transport mutex and
        // outside trusted I/O. Gate-aware owners close only the old references.
        drop(retired);
        Ok(())
    }

    pub(crate) fn acquire_clock(
        &self,
    ) -> io::Result<(Option<InGuestRcbCounter>, Option<Reservation>, u64)> {
        let mut connection = self.connection.lock();
        if FORKED_SINCE_LAST_RPC.load(Ordering::Acquire) || connection.acquired {
            return Err(control::invalid(
                "counter acquisition on stale/already active connection",
            ));
        }
        self.cpu_binding.verify()?;
        let mut acquire = Packet::new(control::ACQUIRE, connection.generation);
        acquire.0[4] = u64::from(self.cpu_binding.cpu);
        if let Some(profile) = self.cpu_binding.profile {
            acquire.0[5] = 1;
            acquire.0[6] = u64::from(profile.event_type());
            acquire.0[7] = profile.config();
        }
        control::send(&connection.setup, acquire, &[], false)?;
        let received = control::receive(&connection.setup, true, false)?;
        if received.packet.0[1] == control::UNSUPPORTED_CPU {
            received
                .packet
                .require(control::UNSUPPORTED_CPU, connection.generation)?;
            received.no_fds()?;
            if self.cpu_binding.profile.is_some() {
                return Err(control::invalid(
                    "supervisor falsely reported unsupported native PMU",
                ));
            }
            self.authenticate_profile(AuthenticatedRcbProfile::Unsupported(self.cpu_binding))?;
            connection.event_id = 0;
            return Ok((None, None, 0));
        }
        let (packet, fd) = received.only_fd()?;
        packet.require(control::EVENT, connection.generation)?;
        let description = RcbEventDescription {
            version: u32::try_from(packet.0[7])
                .map_err(|_| control::invalid("event version overflow"))?,
            event_id: packet.0[4],
            event_type: u32::try_from(packet.0[5])
                .map_err(|_| control::invalid("event type overflow"))?,
            config: packet.0[6],
        };
        if description.version != 1 || description.event_id == 0 {
            return Err(control::invalid("invalid supervisor event description"));
        }
        let Some(native_profile) = self.cpu_binding.profile else {
            return Err(control::invalid(
                "supervisor offered an EVENT on an unsupported native PMU",
            ));
        };
        if !native_profile.matches(&description) {
            return Err(control::invalid(
                "supervisor EVENT differs from the captured native PMU profile",
            ));
        }
        let authenticated = AuthenticatedRcbProfile::Event(self.cpu_binding);
        if self
            .authenticated_rcb_profile
            .get()
            .is_some_and(|root| *root != authenticated)
        {
            return Err(control::invalid(
                "fork-child EVENT differs from root authenticated PMU profile",
            ));
        }
        let (owned, reservation) = fd.into_owned_reserved()?;
        let counter = unsafe {
            InGuestRcbCounter::from_disabled_owned_fd_with_syscall_gate(
                owned,
                &description,
                raw_syscall6,
            )
        }
        .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))?;
        self.authenticate_profile(authenticated)?;
        connection.event_id = description.event_id;
        Ok((Some(counter), Some(reservation), description.event_id))
    }

    /// CPU selected before the one native-profile capture for this exec image.
    pub(crate) fn bound_cpu(&self) -> u32 {
        self.cpu_binding.cpu
    }

    /// Reattest the singleton mask and current CPU while the event is disabled.
    pub(crate) fn verify_cpu_binding(&self) -> io::Result<()> {
        self.cpu_binding.verify()
    }

    #[cfg(feature = "rcb-qualification")]
    pub(crate) fn running_signal_probe_for_test() -> io::Result<(i32, [u64; 8])> {
        let fd = QUALIFICATION_SETUP_FD.load(Ordering::Acquire);
        let generation = QUALIFICATION_GENERATION.load(Ordering::Acquire);
        if fd < 0 || generation == 0 {
            return Err(control::invalid("qualification setup channel is unavailable"));
        }
        Ok((fd, Packet::new(control::RUNNING_SIGNAL_PROBE, generation).0))
    }

    fn authenticate_profile(&self, profile: AuthenticatedRcbProfile) -> io::Result<()> {
        if let Some(root) = self.authenticated_rcb_profile.get() {
            return if *root == profile {
                Ok(())
            } else {
                Err(control::invalid(
                    "fork-child PMU disposition differs from root authentication",
                ))
            };
        }
        self.authenticated_rcb_profile
            .set(profile)
            .map_err(|_| control::invalid("root PMU profile authentication raced"))
    }

    pub(crate) fn acknowledge_clock(&self) -> io::Result<()> {
        let mut connection = self.connection.lock();
        let mut ack = Packet::new(control::ACK, connection.generation);
        ack.0[4] = connection.event_id;
        control::send(&connection.setup, ack, &[], false)?;
        let accepted = control::receive(&connection.setup, true, false)?;
        accepted.no_fds()?;
        accepted
            .packet
            .require(control::ACK, connection.generation)?;
        connection.acquired = true;
        Ok(())
    }
}

#[reverie::tool]
impl<G: GlobalTool> GlobalRPC<G> for CoordinatorRpc<G> {
    async fn send_rpc(&self, message: G::Request) -> G::Response {
        // Child identity/configuration/counter acquisition is eager. Never
        // reconnect or perform network setup from a lazy clock/RPC read path.
        if FORKED_SINCE_LAST_RPC.load(Ordering::Acquire) {
            rpc_fatal(122);
        }
        let connection = self.connection.lock();
        if !connection.acquired {
            rpc_fatal(122);
        }
        match connection.client.try_send_rpc(message) {
            Ok(response) => response,
            Err(_) => rpc_fatal(123),
        }
    }
    fn config(&self) -> &G::Config {
        &self.config
    }
}

fn current_id(number: i64) -> io::Result<Pid> {
    let id = control::result(unsafe { raw_syscall6(number, [0; 6]) })?;
    i32::try_from(id)
        .ok()
        .filter(|id| *id > 0)
        .map(Pid::from_raw)
        .ok_or_else(|| control::invalid("invalid current task identity"))
}
fn rpc_fatal(status: i32) -> ! {
    unsafe {
        raw_syscall6(libc::SYS_exit_group, [status as u64, 0, 0, 0, 0, 0]);
    }
    loop {
        core::hint::spin_loop();
    }
}
