//! One authenticated setup transaction for the installed runtime endpoints.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use reverie_rpc_transport::guest_log::ordered::Buffer;

use super::*;

const SET_MAGIC: &[u8; 16] = b"REVERIE-LI-SET1\0";
const SET_READY: &[u8; 16] = b"LI-SET-READY-1\0\0";
const SET_REQUEST: usize = 88;
const SET_RESPONSE: usize = SET_REQUEST + 4 * 32;

#[derive(Clone, Copy, Eq, PartialEq)]
struct Backing {
    device: u64,
    inode: u64,
    bytes: i64,
}

impl Backing {
    fn read(fd: RawFd) -> io::Result<Self> {
        let stat = descriptor_stat(fd)?;
        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            bytes: stat.st_size,
        })
    }

    fn encode(self, bytes: &mut [u8]) {
        bytes[..8].copy_from_slice(&self.device.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.inode.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.bytes.to_le_bytes());
    }

    fn decode(bytes: &[u8]) -> Self {
        Self {
            device: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            inode: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            bytes: i64::from_le_bytes(bytes[16..24].try_into().unwrap()),
        }
    }
}

/// Trusted identity for the two fixed captures and per-process RPC endpoints.
/// This describes setup; it does not own processes, workers, or capture completion.
#[derive(Clone)]
pub struct InstalledCoordinator {
    coordinator: MappedCoordinator,
    public: Backing,
    private: Backing,
    statistics: bool,
}

/// Setup owner retaining the two host-only capture transfer descriptors.
///
/// Each accepted process gets fresh main and optional statistics RPC mappings.
/// The public and private captures keep their separate backing and incarnation.
pub struct InstalledSetupListener {
    listener: MappedSetupListener,
    public: OwnedFd,
    private: OwnedFd,
    endpoint: InstalledCoordinator,
    guest_uid: libc::uid_t,
    guest_gid: libc::gid_t,
    captures: [Arc<Buffer>; 2],
    accepted: BTreeMap<u64, i32>,
    incarnations: [BTreeSet<usize>; 2],
    next_process: u64,
}

/// Host endpoints after all transient setup descriptors have closed.
/// Serving and joining these streams are separate from actual process waits.
pub struct InstalledStreams {
    pub pid: reverie::Pid,
    /// Setup identity, not a process-exit or successful-fork certificate.
    pub process_identity: u64,
    pub parent_identity: u64,
    pub main: MappedStream,
    pub statistics: Option<MappedStream>,
    pub public_incarnation: usize,
    pub private_incarnation: usize,
}

pub(in crate::rpc) struct ImportedSet {
    pub process_identity: u64,
    pub main: MappedStream,
    pub statistics: Option<MappedStream>,
    pub public: Arc<Buffer>,
    pub private: Arc<Buffer>,
}

impl InstalledSetupListener {
    /// Bind without replacing an existing path. The supplied descriptors must
    /// identify distinct V4 captures created for this run.
    ///
    /// # Safety
    /// All mapping aliases obey the ordered Buffer and MappedStream contracts
    /// for their full lifetime. Only trusted runtime setup code receives this
    /// identity or the descriptors. The host independently owns child admission,
    /// waits and workers, and maintains its declared descriptor-access policy
    /// before these private resources exist until they are closed. Credentials
    /// detect a different peer; they do not make shared memory a sandbox.
    pub unsafe fn bind(
        path: impl AsRef<Path>,
        public: OwnedFd,
        private: OwnedFd,
        statistics: bool,
        guest_uid: libc::uid_t,
        guest_gid: libc::gid_t,
    ) -> io::Result<Self> {
        let public_identity = Backing::read(public.as_raw_fd())?;
        let private_identity = Backing::read(private.as_raw_fd())?;
        if public_identity.device == private_identity.device
            && public_identity.inode == private_identity.inode
        {
            return Err(invalid("public and private captures share backing"));
        }
        let listener = MappedSetupListener::bind(path)?;
        // Read-only-use views for checking a requested inactive capture's
        // existing shared failure. These aliases obey bind's trusted lifetime.
        let captures = [unsafe { Buffer::import(public.try_clone()?) }?, unsafe {
            Buffer::import(private.try_clone()?)
        }?];
        let endpoint = InstalledCoordinator {
            coordinator: listener.coordinator(),
            public: public_identity,
            private: private_identity,
            statistics,
        };
        Ok(Self {
            listener,
            public,
            private,
            endpoint,
            guest_uid,
            guest_gid,
            captures,
            accepted: BTreeMap::new(),
            incarnations: [BTreeSet::new(), BTreeSet::new()],
            next_process: 1,
        })
    }

    pub fn coordinator(&self) -> InstalledCoordinator {
        self.endpoint.clone()
    }

    /// Accept one complete endpoint set. No retry conceals a partial setup.
    ///
    /// # Safety
    /// In addition to bind's full-lifetime contract, each admitted peer is the
    /// sole guest thread (its physical PID equals TID). Root uses incarnation 1
    /// in both captures; a fork uses the two reservations made before that exact
    /// physical fork. The host continues servicing attachment while a vfork
    /// parent waits and retains every task and descendant through actual cleanup.
    pub unsafe fn accept(
        &mut self,
        capacity: usize,
        deadline: Instant,
    ) -> io::Result<InstalledStreams> {
        let socket = loop {
            wait_fd(self.listener.socket.as_raw_fd(), libc::POLLIN, deadline)?;
            let fd = unsafe {
                libc::accept4(
                    self.listener.socket.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                )
            };
            if fd >= 0 {
                break unsafe { OwnedFd::from_raw_fd(fd) };
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                return Err(error);
            }
        };
        let peer = credentials(socket.as_raw_fd())?;
        let (request, descriptors) =
            receive_inner(socket.as_raw_fd(), SET_REQUEST, Some(deadline))?;
        let indices = [
            usize::try_from(u64::from_le_bytes(request[56..64].try_into().unwrap()))
                .map_err(|_| invalid("public incarnation overflow"))?,
            usize::try_from(u64::from_le_bytes(request[64..72].try_into().unwrap()))
                .map_err(|_| invalid("private incarnation overflow"))?,
        ];
        let inactive = request[73];
        let parent_pid = i32::from_le_bytes(request[76..80].try_into().unwrap());
        let parent_identity = u64::from_le_bytes(request[80..88].try_into().unwrap());
        let root = parent_identity == 0;
        if !descriptors.is_empty()
            || request[..16] != SET_MAGIC[..]
            || request[16..48] != self.endpoint.coordinator.run
            || i32::from_le_bytes(request[48..52].try_into().unwrap()) != peer.pid
            || i32::from_le_bytes(request[52..56].try_into().unwrap()) != peer.pid
            || peer.uid != self.guest_uid
            || peer.gid != self.guest_gid
            || peer.pid <= 0
            || request[72] != u8::from(self.endpoint.statistics)
            || inactive & !3 != 0
            || request[74..76] != [0; 2]
            || (root
                && (parent_pid != 0
                    || indices != [1, 1]
                    || inactive != 0
                    || !self.accepted.is_empty()))
            || (!root
                && (self.accepted.get(&parent_identity) != Some(&parent_pid)
                    || indices.contains(&1)))
        {
            return Err(invalid("installed setup request identity mismatch"));
        }
        for (role, index) in indices.iter().copied().enumerate() {
            let disabled = inactive & (1 << role) != 0;
            if disabled != (index == 0)
                || (disabled && !self.captures[role].guest_failed())
                || (index != 0 && self.incarnations[role].contains(&index))
            {
                return Err(invalid(
                    "installed capture inactive/duplicate incarnation mismatch",
                ));
            }
        }
        let process_identity = self.next_process;
        self.next_process = process_identity
            .checked_add(1)
            .ok_or_else(|| invalid("installed process identity exhausted"))?;
        // Numeric PIDs can be reused after reap. Keep setup generations and
        // role incarnations distinct from current SO_PEERCRED authentication.
        self.accepted.insert(process_identity, peer.pid);
        for (role, index) in indices.iter().copied().enumerate() {
            if index != 0 {
                self.incarnations[role].insert(index);
            }
        }
        // Each successful create has exactly one host stream and one transfer
        // descriptor. No guest client/config callback runs in this operation.
        let (mut main, main_fd) = unsafe { MappedStream::create(capacity) }?;
        let statistics = if self.endpoint.statistics {
            Some(unsafe { MappedStream::create(capacity) }?)
        } else {
            None
        };
        let mut descriptors = vec![main_fd, self.public.try_clone()?, self.private.try_clone()?];
        let statistics = statistics.map(|(stream, fd)| {
            descriptors.push(fd);
            stream
        });
        let mut response = [0; SET_RESPONSE];
        response[..SET_REQUEST].copy_from_slice(&request);
        for (index, descriptor) in descriptors.iter().enumerate() {
            let slot = &mut response[SET_REQUEST + index * 32..SET_REQUEST + (index + 1) * 32];
            slot[..4].copy_from_slice(&((index + 1) as u32).to_le_bytes());
            Backing::read(descriptor.as_raw_fd())?.encode(&mut slot[8..]);
        }
        send_set(socket.as_raw_fd(), &response, &descriptors, deadline)?;
        drop(descriptors);
        drop(socket);
        // Mapping acknowledgement is after actual closure, not a promise sent
        // before close on the setup socket. The permanent host listener/capture
        // source descriptors remain privately owned by this explicit host.
        let mut ready = [0; 24];
        ready[..16].copy_from_slice(SET_READY);
        ready[16..].copy_from_slice(&process_identity.to_le_bytes());
        main.write_all_until(&ready, deadline)?;
        Ok(InstalledStreams {
            pid: reverie::Pid::from_raw(peer.pid),
            process_identity,
            parent_identity,
            main,
            statistics,
            public_incarnation: indices[0],
            private_incarnation: indices[1],
        })
    }
}

impl InstalledCoordinator {
    pub fn statistics_enabled(&self) -> bool {
        self.statistics
    }

    /// Encode identity only; no file descriptor or process ownership is encoded.
    pub fn to_bytes(&self) -> Vec<u8> {
        let coordinator = self.coordinator.to_bytes();
        let mut bytes = vec![0; 20 + coordinator.len() + 56];
        bytes[..16].copy_from_slice(SET_MAGIC);
        bytes[16..20].copy_from_slice(&(coordinator.len() as u32).to_le_bytes());
        bytes[20..20 + coordinator.len()].copy_from_slice(&coordinator);
        let tail = &mut bytes[20 + coordinator.len()..];
        self.public.encode(&mut tail[..24]);
        self.private.encode(&mut tail[24..48]);
        tail[48] = u8::from(self.statistics);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 76 || bytes[..16] != SET_MAGIC[..] {
            return Err(invalid("invalid installed coordinator encoding"));
        }
        let length = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        if bytes.len().checked_sub(76) != Some(length) {
            return Err(invalid("invalid installed coordinator length"));
        }
        let coordinator = MappedCoordinator::from_bytes(&bytes[20..20 + length])?;
        let tail = &bytes[20 + length..];
        if tail[48] > 1 || tail[49..].iter().any(|byte| *byte != 0) {
            return Err(invalid("invalid installed coordinator options"));
        }
        let public = Backing::decode(&tail[..24]);
        let private = Backing::decode(&tail[24..48]);
        if public.bytes <= 0
            || private.bytes <= 0
            || (public.device == private.device && public.inode == private.inode)
        {
            return Err(invalid("invalid installed capture backing"));
        }
        Ok(Self {
            coordinator,
            public,
            private,
            statistics: tail[48] != 0,
        })
    }

    /// # Safety
    /// The installed runtime privately owns all imports and serializes the
    /// actual single-thread fork transition. Its host satisfies accept's trusted
    /// ownership contract. Allocate metadata in the reusable runtime allocator.
    pub(in crate::rpc) unsafe fn connect(
        &self,
        pid: reverie::Pid,
        tid: reverie::Pid,
        incarnations: [usize; 2],
        parent: Option<(u64, reverie::Pid)>,
        deadline: Instant,
    ) -> io::Result<ImportedSet> {
        if pid != tid || pid.as_raw() <= 0 {
            return Err(invalid("installed setup requires the sole guest thread"));
        }
        let socket = socket()?;
        let flags = unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_GETFL) };
        call(flags)?;
        call(unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) })?;
        check_deadline(deadline)?;
        let result = unsafe {
            libc::connect(
                socket.as_raw_fd(),
                (&raw const self.coordinator.address).cast(),
                self.coordinator.address_len,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(error);
            }
            wait_fd(socket.as_raw_fd(), libc::POLLOUT, deadline)?;
            let mut error = 0_i32;
            let mut length = std::mem::size_of_val(&error) as libc::socklen_t;
            call(unsafe {
                libc::getsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    (&raw mut error).cast(),
                    &raw mut length,
                )
            })?;
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
        }
        let peer = credentials(socket.as_raw_fd())?;
        let expected = self.coordinator.host;
        if peer.pid != expected.pid || peer.uid != expected.uid || peer.gid != expected.gid {
            return Err(invalid("installed setup coordinator credentials changed"));
        }
        let mut request = [0; SET_REQUEST];
        request[..16].copy_from_slice(SET_MAGIC);
        request[16..48].copy_from_slice(&self.coordinator.run);
        request[48..52].copy_from_slice(&pid.as_raw().to_le_bytes());
        request[52..56].copy_from_slice(&tid.as_raw().to_le_bytes());
        request[56..64].copy_from_slice(&(incarnations[0] as u64).to_le_bytes());
        request[64..72].copy_from_slice(&(incarnations[1] as u64).to_le_bytes());
        request[72] = u8::from(self.statistics);
        request[73] = u8::from(incarnations[0] == 0) | (u8::from(incarnations[1] == 0) << 1);
        if let Some((identity, parent_pid)) = parent {
            request[76..80].copy_from_slice(&parent_pid.as_raw().to_le_bytes());
            request[80..88].copy_from_slice(&identity.to_le_bytes());
        }
        send_set(socket.as_raw_fd(), &request, &[], deadline)?;
        let (response, descriptors) =
            receive_inner(socket.as_raw_fd(), SET_RESPONSE, Some(deadline))?;
        let count = if self.statistics { 4 } else { 3 };
        if response[..SET_REQUEST] != request || descriptors.len() != count {
            return Err(invalid(
                "installed setup response identity or count mismatch",
            ));
        }
        let mut identities = Vec::with_capacity(count);
        for (index, descriptor) in descriptors.iter().enumerate() {
            let slot = &response[SET_REQUEST + index * 32..SET_REQUEST + (index + 1) * 32];
            let identity = Backing::read(descriptor.as_raw_fd())?;
            if slot[..4] != ((index + 1) as u32).to_le_bytes()
                || slot[4..8] != [0; 4]
                || Backing::decode(&slot[8..]) != identity
                || identities.iter().any(|old: &Backing| {
                    old.device == identity.device && old.inode == identity.inode
                })
                || (index == 1 && identity != self.public)
                || (index == 2 && identity != self.private)
            {
                return Err(invalid("installed setup role or backing identity mismatch"));
            }
            identities.push(identity);
        }
        if response[SET_REQUEST + count * 32..]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(invalid("installed setup unexpected endpoint role"));
        }
        // Every delivered descriptor was owned before any validation. Imports
        // consume/close them, including partial setup failure. These streams are
        // fresh; no inherited endpoint is ever passed through this error path.
        let mut descriptors = descriptors.into_iter();
        let mut main = unsafe { MappedStream::from_owned_fd(descriptors.next().unwrap()) }?;
        let public = unsafe { Buffer::import(descriptors.next().unwrap()) }?;
        let private = unsafe { Buffer::import(descriptors.next().unwrap()) }?;
        let statistics = if self.statistics {
            Some(unsafe { MappedStream::from_owned_fd(descriptors.next().unwrap()) }?)
        } else {
            None
        };
        drop(descriptors);
        drop(socket);
        let mut ready = [0; 24];
        main.read_exact_until(&mut ready, deadline)?;
        let process_identity = u64::from_le_bytes(ready[16..].try_into().unwrap());
        if ready[..16] != *SET_READY || process_identity == 0 {
            return Err(invalid("installed setup closure acknowledgement mismatch"));
        }
        Ok(ImportedSet {
            process_identity,
            main,
            statistics,
            public,
            private,
        })
    }
}

fn send_set(fd: RawFd, bytes: &[u8], descriptors: &[OwnedFd], deadline: Instant) -> io::Result<()> {
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let mut control = Control([0; 256]);
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    let size = descriptors.len() * std::mem::size_of::<RawFd>();
    if !descriptors.is_empty() {
        message.msg_control = control.0.as_mut_ptr().cast();
        message.msg_controllen = unsafe { libc::CMSG_SPACE(size as u32) } as usize;
        let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        unsafe {
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size as u32) as usize;
            let payload = libc::CMSG_DATA(header).cast::<RawFd>();
            for (index, descriptor) in descriptors.iter().enumerate() {
                payload.add(index).write_unaligned(descriptor.as_raw_fd());
            }
        }
    }
    loop {
        wait_fd(fd, libc::POLLOUT, deadline)?;
        let count = unsafe { libc::sendmsg(fd, &message, libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) };
        if count == bytes.len() as isize {
            return check_deadline(deadline);
        }
        let error = io::Error::last_os_error();
        if count < 0
            && (error.raw_os_error() == Some(libc::EINTR)
                || error.kind() == io::ErrorKind::WouldBlock)
        {
            continue;
        }
        return Err(if count < 0 {
            error
        } else {
            invalid("installed setup partial packet write")
        });
    }
}

fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}

pub(super) fn wait_fd(fd: RawFd, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    loop {
        check_deadline(deadline)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut poll = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&raw mut poll, 1, timeout) };
        if result > 0 {
            check_deadline(deadline)?;
            if poll.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
            return Ok(());
        }
        if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return Err(io::Error::last_os_error());
        }
    }
}
