// Private SCM tokens are never guest file-table entries. A pinned object key
// grants access to a shared virtual description; a reserved name only rejects
// unknown tokens. No supervisor standard descriptor enters the host message.
const CAPTURE_TOKEN_PREFIX: &[u8] = b"reverie-kvm.capture-transfer.v1";

fn capture_token() -> std::io::Result<std::fs::File> {
    // Root setup runs before executor guards exist.
    capture_token_in(&crate::elf::FileRetirement::default())
}

fn capture_token_in(retirement: &crate::elf::FileRetirement) -> std::io::Result<std::fs::File> {
    let name = CString::new(CAPTURE_TOKEN_PREFIX).expect("static token name");
    let raw =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let file = retirement.stage(unsafe { std::fs::File::from_raw_fd(raw) });
    let file = if raw < 3 {
        let private = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 3) };
        if private < 0 {
            return Err(std::io::Error::last_os_error());
        }
        retirement.stage(unsafe { std::fs::File::from_raw_fd(private) })
    } else {
        file
    };
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    #[cfg(test)]
    let seals = if CAPTURE_TOKEN_FAIL_SEAL.get() {
        seals | i32::MIN
    } else {
        seals
    };
    if unsafe { libc::fcntl(file.as_file().as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file.into_file())
}

fn capture_token_candidate(file: &std::fs::File) -> Result<bool, i64> {
    // Only inspect the fixed reserved prefix. Long ordinary proc-fd targets
    // remain ordinary, and O_PATH tokens are classified without reading data.
    const PREFIX: &[u8] = b"/memfd:reverie-kvm.capture-transfer.v1";
    let path = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
    let mut prefix = [0_u8; PREFIX.len()];
    let length = unsafe { libc::readlink(path.as_ptr(), prefix.as_mut_ptr().cast(), prefix.len()) };
    if length < 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    Ok(length as usize == PREFIX.len() && prefix.as_slice() == PREFIX)
}

#[derive(Debug)]
pub(crate) struct CaptureTransfer {
    description: Arc<CaptureDescription>,
    in_flight: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutgoingTransfer {
    Proc((libc::dev_t, libc::ino_t)),
    Capture((libc::dev_t, libc::ino_t)),
}

fn virtual_transfers_in_flight(table: &crate::elf::GuestFileIdentityTable) -> usize {
    table
        .proc_transfers
        .values()
        .map(|entry| entry.in_flight)
        .sum::<usize>()
        + table
            .capture_transfers
            .values()
            .map(|entry| entry.in_flight)
            .sum::<usize>()
}

fn register_capture_transfer(
    state: &LoadedStaticElf,
    guest_fd: libc::c_int,
) -> Result<((libc::dev_t, libc::ino_t), RawFd), i64> {
    let description = state
        .capture_descriptions
        .get(&guest_fd)
        .ok_or_else(|| negative_errno(libc::ENOSYS))?;
    if output_alias(state, guest_fd) != Some(description.alias)
        || !state
            .capture_owner
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, &description.identities))
        || description.status.load(Ordering::SeqCst) & (libc::O_PATH | libc::O_ACCMODE)
            != libc::O_WRONLY
    {
        return Err(negative_errno(libc::ENOSYS));
    }
    let token_fd = description.token.as_raw_fd();
    let key = host_file_key(token_fd)?;
    let mut table = state
        .file_identity_table
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if virtual_transfers_in_flight(&table) >= PROC_TRANSFER_LIMIT {
        return Err(negative_errno(libc::ETOOMANYREFS));
    }
    match table.capture_transfers.entry(key) {
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            if !Arc::ptr_eq(&entry.get().description, description) {
                return Err(negative_errno(libc::ENOSYS));
            }
            entry.get_mut().in_flight += 1;
        }
        std::collections::btree_map::Entry::Vacant(entry) => {
            // The description owns this unique sealed token, its private I/O
            // description, stream identity and sink. This is also the inode pin.
            entry.insert(CaptureTransfer {
                description: description.clone(),
                in_flight: 1,
            });
        }
    }
    Ok((key, token_fd))
}

fn received_capture_transfer(
    state: &LoadedStaticElf,
    file: &std::fs::File,
) -> Result<Option<Arc<CaptureDescription>>, i64> {
    let key = host_file_key(file.as_raw_fd())?;
    let table = state
        .file_identity_table
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(entry) = table.capture_transfers.get(&key) {
        if !state
            .capture_owner
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, &entry.description.identities))
        {
            return Err(negative_errno(libc::ENOSYS));
        }
        return Ok(Some(entry.description.clone()));
    }
    drop(table);
    if capture_token_candidate(file)? {
        return Err(negative_errno(libc::EBADMSG));
    }
    Ok(None)
}

fn take_capture_transfer(
    table: &mut crate::elf::GuestFileIdentityTable,
    key: (libc::dev_t, libc::ino_t),
) -> Option<Arc<CaptureDescription>> {
    if let std::collections::btree_map::Entry::Occupied(mut entry) =
        table.capture_transfers.entry(key)
    {
        entry.get_mut().in_flight -= 1;
        if entry.get().in_flight == 0 {
            return Some(entry.remove().description);
        }
    }
    None
}

fn release_outgoing_transfers(state: &LoadedStaticElf, transfers: &[OutgoingTransfer]) {
    let mut table = state
        .file_identity_table
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let mut captures = Vec::new();
    for transfer in transfers {
        match transfer {
            OutgoingTransfer::Proc(key) => {
                take_proc_transfer(&mut table, *key);
            }
            OutgoingTransfer::Capture(key) => {
                captures.extend(take_capture_transfer(&mut table, *key));
            }
        }
    }
    drop(table);
    state.file_retirement.retire_capture(captures);
}

fn release_received_virtual_transfers(
    state: &LoadedStaticElf,
    keys: &[(libc::dev_t, libc::ino_t)],
) {
    let mut table = state
        .file_identity_table
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let mut captures = Vec::new();
    for key in keys {
        take_proc_transfer(&mut table, *key);
        captures.extend(take_capture_transfer(&mut table, *key));
    }
    drop(table);
    state.file_retirement.retire_capture(captures);
}

// Linux UAPI F_LINUX_SPECIFIC_BASE + 3. Pinned libc need not expose it.
const HOST_F_DUPFD_QUERY: libc::c_int = 1027;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PhysicalStdioOfdQuery {
    Fcntl,
    Kcmp,
}

impl PhysicalStdioOfdQuery {
    fn same(self, left: RawFd, right: RawFd) -> Result<bool, i64> {
        let result = match self {
            Self::Fcntl => {
                // SAFETY: this command compares live descriptor references;
                // it neither allocates a descriptor nor changes the OFD.
                unsafe { libc::fcntl(left, HOST_F_DUPFD_QUERY, right) as libc::c_long }
            }
            Self::Kcmp => {
                // Compare the calling thread's file table. getpid() can name
                // another table after thread-local unsharing; never cache TID.
                let tid = unsafe { libc::syscall(libc::SYS_gettid) };
                if tid < 0 {
                    return Err(io_error(std::io::Error::last_os_error()));
                }
                // SAFETY: KCMP_FILE=0 takes two task IDs and unsigned-long fd
                // indices. The kernel validates descriptors and permissions.
                unsafe {
                    libc::syscall(
                        libc::SYS_kcmp,
                        tid as libc::pid_t,
                        tid as libc::pid_t,
                        0 as libc::c_int,
                        left as libc::c_ulong,
                        right as libc::c_ulong,
                    )
                }
            }
        };
        if result == -1 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        self.decode_result(result)
    }

    fn decode_result(self, result: libc::c_long) -> Result<bool, i64> {
        match (self, result) {
            (Self::Fcntl, 1) | (Self::Kcmp, 0) => Ok(true),
            (Self::Fcntl, 0) | (Self::Kcmp, 1..=3) => Ok(false),
            _ => Err(negative_errno(libc::EIO)),
        }
    }
}

fn select_physical_stdio_query(
    stdout: RawFd,
    stderr: RawFd,
    mut same: impl FnMut(PhysicalStdioOfdQuery, RawFd, RawFd) -> Result<bool, i64>,
) -> Result<PhysicalStdioOfdQuery, [i64; 2]> {
    let mut qualify = |method: PhysicalStdioOfdQuery| {
        if same(method, stdout, stdout)? && !same(method, stdout, stderr)? {
            Ok(method)
        } else {
            Err(negative_errno(libc::EIO))
        }
    };
    match qualify(PhysicalStdioOfdQuery::Fcntl) {
        Ok(method) => Ok(method),
        Err(fcntl_error) => {
            qualify(PhysicalStdioOfdQuery::Kcmp).map_err(|kcmp_error| [fcntl_error, kcmp_error])
        }
    }
}

fn qualify_physical_stdio_query(
    owner: &CapturedPipeIdentities,
) -> Result<PhysicalStdioOfdQuery, [i64; 2]> {
    // These two independently-created pipes are pinned by owner. Capability
    // selection must precede any inspection of physical stdout/stderr, even
    // when both are closed. Preserve both qualification errors for diagnosis.
    let stdout = owner.writer(OutputAlias::Stdout);
    let stderr = owner.writer(OutputAlias::Stderr);
    select_physical_stdio_query(
        stdout.as_raw_fd(),
        stderr.as_raw_fd(),
        PhysicalStdioOfdQuery::same,
    )
}

#[cfg(test)]
fn physical_stdio_is_open(fd: RawFd) -> Result<bool, i64> {
    // SAFETY: F_GETFD only checks the descriptor. External concurrent stdio
    // replacement remains outside the inherited-stdio lifetime contract.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EBADF) {
        Ok(false)
    } else {
        Err(io_error(error))
    }
}

// Fixed proc snapshots are bearer capabilities, not ordinary host files. A
// proc_files inode or a reserved-looking name alone cannot authorize bypassing
// the physical-stdio guard: authenticate this exact pinned read-only carrier.
fn authenticated_fixed_proc_donation(
    state: &LoadedStaticElf,
    guest_fd: libc::c_int,
    budget: &mut crate::proc_carrier::CarrierAuthBudget,
    cache: &mut crate::proc_carrier::CarrierAuthCache,
) -> Result<bool, i64> {
    let Some(owner) = &state.capture_owner else {
        return Ok(false);
    };
    let Some(expected_path) = state
        .proc_files
        .get(&guest_fd)
        .and_then(|&inode| synthetic_proc_path_for_inode(inode))
        .filter(|path| fixed_synthetic_proc_path(path))
    else {
        return Ok(false);
    };
    let file = state
        .files
        .get(&guest_fd)
        .ok_or_else(|| negative_errno(libc::EBADMSG))?;
    if state
        .proc_carrier_authority
        .candidate_kind(file)
        .map_err(negative_errno)?
        != crate::proc_carrier::ProcCarrierCandidate::Reserved
    {
        return Err(negative_errno(libc::EBADMSG));
    }
    ensure_proc_carrier_readonly_or_path(file)?;
    let inspection = state
        .proc_carrier_authority
        .open_inspection_alias(file)
        .map(|file| state.file_retirement.stage(file))
        .map_err(negative_errno)?;
    let authenticated = state
        .proc_carrier_authority
        .authenticate_reserved(file, inspection.as_file(), budget, cache)
        .map_err(negative_errno)?;
    if authenticated.canonical_path.as_slice() != expected_path
        || authenticated.virtual_nofollow != state.synthetic_proc_nofollow_fds.contains(&guest_fd)
    {
        return Err(negative_errno(libc::EBADMSG));
    }
    // This is refusal only, after full authentication. A private sealed
    // carrier used as capture-entry physical stdio must not leak that OFD.
    // Compare against pinned entry identities, including recorded absence;
    // never inspect live fd 1/2 or mistake an inspection alias for stdio.
    let carrier_key = host_file_key(file.as_raw_fd())?;
    for physical in owner.physical_stdio() {
        if host_file_key(physical)? == carrier_key {
            return Err(negative_errno(libc::ENOSYS));
        }
    }
    Ok(true)
}

fn unlabelled_capture_carrier(state: &LoadedStaticElf, fd: RawFd) -> Result<bool, i64> {
    let Some(owner) = &state.capture_owner else {
        return Ok(false);
    };
    // No capability means uniform refusal of otherwise-ordinary rights under
    // capture, independent of the donated inode or physical stdio topology.
    // Authenticated capture/proc rights take their existing earlier paths.
    let query = qualify_physical_stdio_query(owner)
        .map_err(|_qualification_errors| negative_errno(libc::ENOSYS))?;
    let key = host_file_key(fd)?;
    for alias in [OutputAlias::Stdout, OutputAlias::Stderr] {
        // Private pipe identity is intentionally object-wide, including an
        // independently reopened endpoint. Do not narrow this check to OFDs.
        if host_file_key(owner.writer(alias).as_raw_fd())? == key {
            return Ok(true);
        }
    }
    // Compare only OFDs present at capture entry. The pins survive later
    // closes/reuse of their standard numbers; independent opens of the same
    // inode stay ordinary. Uniform query qualification above still precedes
    // an empty pin set, and later query errors remain errors, never permission.
    for standard in owner.physical_stdio() {
        if query.same(fd, standard)? {
            return Ok(true);
        }
    }
    Ok(false)
}

// This gate is shared by all fork/thread/exec members of one identity namespace.
// It is separate from the allocator lock: receive staging calls allocator and
// transfer lookup helpers, and guest copyout must run with both locks released.
fn capture_receive_gate(state: &LoadedStaticElf) -> Option<Arc<Mutex<()>>> {
    state.capture_owner.as_ref()?;
    Some(
        state
            .file_identity_table
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .capture_receive_gate
            .clone(),
    )
}

#[cfg(test)]
thread_local! {
    static CAPTURE_TOKEN_FAIL_SEAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static CAPTURE_RECEIVE_AFTER_HOST: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static CAPTURE_RECEIVE_AFTER_RELEASE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn capture_receive_after_host() {
    let hook = CAPTURE_RECEIVE_AFTER_HOST.with_borrow_mut(Option::take);
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(test)]
fn capture_receive_after_release() {
    let hook = CAPTURE_RECEIVE_AFTER_RELEASE.with_borrow_mut(Option::take);
    if let Some(hook) = hook {
        hook();
    }
}
