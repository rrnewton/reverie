/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use core::convert::Infallible;
use core::fmt;
use core::ptr;
use core::str::FromStr;
use std::collections::HashMap;
use std::ffi::CString;
use std::ffi::OsStr;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

pub use nix::mount::MsFlags as MountFlags;
use syscalls::Errno;

use super::error::AddContext;
use super::error::Context;
use super::error::Error;
use super::fd::Fd;
use super::fd::FileType;
use super::fd::create_dir_all;
use super::fd::touch_path;
use super::pinned_mount::PinnedMountSource;
use super::pinned_mount::descriptor_path;

/// Additional restrictions on the top mount of a checked bind. Recursive
/// binds retain the existing policy of their descendant mounts.
pub(super) const BIND_RESTRICTIONS: MountFlags = MountFlags::from_bits_retain(
    MountFlags::MS_RDONLY.bits()
        | MountFlags::MS_NOSUID.bits()
        | MountFlags::MS_NODEV.bits()
        | MountFlags::MS_NOEXEC.bits(),
);

/// A mount.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mount {
    source: Option<CString>,
    target: CString,
    fstype: Option<CString>,
    flags: MountFlags,
    data: Option<CString>,
    touch_target: bool,
    allow_readonly_fallback: bool,
    optional: bool,
    pinned_source: Option<PinnedMountSource>,
    /// Where a read-only bind waits between its bind and its remount
    /// ([`Mount::pause_before_remount`]).
    remount_pause: Option<RemountPause>,
    /// A path, fstype or data string that could not be represented as a C
    /// string, recorded at BUILD time and reported at [`Mount::mount`] time.
    ///
    /// ⚠️ WHY A FLAG AND NOT A `Result` FROM THE BUILDER. `mount()` runs AFTER
    /// FORK, where this module documents that it cannot allocate -- so the
    /// failure cannot be discovered or described there. And making the builder
    /// fallible would change `Mount::new(..) -> Self` into
    /// `-> Result<Self, _>` for every caller, including hermit, to report a
    /// condition none of them can do anything about except refuse the mount.
    /// Recording one bool costs nothing after fork and turns a panic into the
    /// `Errno` the caller already handles.
    unrepresentable: bool,
}

/// The files [`Mount::pause_before_remount`] creates and waits for, how long
/// it waits, and how often it looks for the release.
#[derive(Clone, Debug, Eq, PartialEq)]
struct RemountPause {
    arrived: CString,
    release: CString,
    limit: Duration,
    poll: Duration,
}

/// How often a paused mount looks for its release file.
const REMOUNT_PAUSE_POLL: Duration = Duration::from_millis(10);

/// Represents a bind mount. Can be converted into a [`Mount`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bind {
    /// The source path of the bind mount. This path must exist. It can be either
    /// a file or directory.
    pub source: CString,

    /// The target of the bind mount. This does not need to exist and can be
    /// created when performing the bind mount.
    pub target: CString,

    /// Whether the bind is mounted read-only (the `:ro` suffix in the
    /// `source:target:ro` spelling; `:rw` and no suffix are writable).
    pub readonly: bool,
}

/// Creates the arrival marker of [`Mount::pause_before_remount`] at `path`
/// without blocking or following a symbolic link, and refuses an existing
/// file there that is not a regular file. An existing regular file is reused:
/// the marker says that a mount reached the pause since its caller last
/// removed it, so a setup the caller runs again passes the same pause again
/// instead of failing there. Allocates nothing.
fn mark_arrival(path: &CString) -> Result<(), Errno> {
    // SAFETY: `path` is a live NUL-terminated C string; `O_NONBLOCK` keeps an
    // existing FIFO from blocking the open (with no reader it fails `ENXIO`).
    let fd = Errno::result(unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o644,
        )
    })?;
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fd` is the descriptor opened above, and `stat` is a local
    // buffer `fstat` fills on success.
    let stated = Errno::result(unsafe { libc::fstat(fd, stat.as_mut_ptr()) });
    // SAFETY: `fd` is open and owned here.
    unsafe { libc::close(fd) };
    stated?;
    // SAFETY: `fstat` succeeded, so it filled `stat`.
    let mode = unsafe { stat.assume_init() }.st_mode;
    if mode & libc::S_IFMT != libc::S_IFREG {
        return Err(Errno::EEXIST);
    }
    Ok(())
}

/// `CLOCK_MONOTONIC` now, allocating nothing.
fn monotonic_now() -> Result<Duration, Errno> {
    let mut now = MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: `now` is a local buffer `clock_gettime` fills on success.
    Errno::result(unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, now.as_mut_ptr()) })?;
    // SAFETY: `clock_gettime` succeeded, so it filled `now`.
    let now = unsafe { now.assume_init() };
    Ok(Duration::new(now.tv_sec as u64, now.tv_nsec as u32))
}

/// What the wait of [`Mount::pause_before_remount`] reads and asks for: the
/// clock, whether the release file exists, and a sleep. [`SystemPause`] is the
/// system's; the tests drive [`wait_for_release`] with a scripted one.
trait PauseClock {
    /// `CLOCK_MONOTONIC` now.
    fn now(&mut self) -> Result<Duration, Errno>;
    /// Whether the file `release` exists.
    fn released(&mut self, release: &CString) -> bool;
    /// Sleeps for about `duration`: longer when descheduled, shorter when a
    /// signal interrupts it.
    fn sleep(&mut self, duration: Duration);
}

/// The system's clock, file lookup and sleep, none of which allocates.
struct SystemPause;

impl PauseClock for SystemPause {
    fn now(&mut self) -> Result<Duration, Errno> {
        monotonic_now()
    }

    fn released(&mut self, release: &CString) -> bool {
        // SAFETY: `release` is a live NUL-terminated C string; `access`
        // allocates nothing.
        unsafe { libc::access(release.as_ptr(), libc::F_OK) == 0 }
    }

    fn sleep(&mut self, duration: Duration) {
        let duration = libc::timespec {
            tv_sec: duration.as_secs() as libc::time_t,
            tv_nsec: duration.subsec_nanos() as libc::c_long,
        };
        // SAFETY: `duration` is a valid local `timespec`; `EINTR` is fine,
        // since the wait reads the clock again.
        unsafe { libc::nanosleep(&duration, ptr::null_mut()) };
    }
}

/// Waits for `pause`'s release until a deadline `pause.limit` after the first
/// clock reading, decided by the deadline alone: a release counts only when
/// it was seen before a clock reading that is still short of the deadline.
/// Each sleep it asks for is the poll interval capped to the time left at the
/// last reading, and a sleep that ends early just looks again.
fn wait_for_release(pause: &RemountPause, clock: &mut impl PauseClock) -> Result<(), Errno> {
    let deadline = clock.now()?.saturating_add(pause.limit);
    loop {
        let released = clock.released(&pause.release);
        let now = clock.now()?;
        if now >= deadline {
            return Err(Errno::ETIMEDOUT);
        }
        if released {
            return Ok(());
        }
        clock.sleep(pause.poll.min(deadline - now));
    }
}

/// `util::to_cstring` panics on an interior NUL. Every mount path, source,
/// fstype and data string went through it, so a recoverable "this is not a
/// valid C string" was a panic inside the builder. Measured on reverie main
/// 200439dc8de9:
///
/// ```text
/// Mount::new(OsStr::from_bytes(b"/test/work\0dir"))
///   panicked at reverie-process/src/util.rs:17:41:
///   called `Result::unwrap()` on an `Err` value: NulError(10, [...])
/// ```
///
/// This returns the empty string on failure and says so, letting the caller
/// record it and refuse the mount instead of aborting the process.
fn checked_cstring<S: AsRef<OsStr>>(s: S) -> (CString, bool) {
    match CString::new(s.as_ref().as_bytes()) {
        Ok(c) => (c, true),
        Err(_) => (CString::default(), false),
    }
}

impl Mount {
    /// Creates a new mount at the path `target`.
    pub fn new<S: AsRef<OsStr>>(target: S) -> Self {
        let (t, ok) = checked_cstring(target);
        Self {
            unrepresentable: !ok,
            source: None,
            target: t,
            fstype: None,
            flags: MountFlags::empty(),
            data: None,
            touch_target: false,
            allow_readonly_fallback: false,
            optional: false,
            pinned_source: None,
            remount_pause: None,
        }
    }

    /// Creates a bind mount. This effectively creates hardlink of a directory,
    /// making the contents accessible at both places.
    ///
    /// By default, none of the mounts in the `source` directory are visible in
    /// `destination`. To make all mounts recursively visible, combine this with
    /// [`Mount::recursive`]. Can also be used with [`Mount::readonly`] to make
    /// the contents of `destination` read-only.
    pub fn bind<S: AsRef<OsStr>, D: AsRef<OsStr>>(source: S, destination: D) -> Self {
        Self::new(destination)
            .source(source)
            .flags(MountFlags::MS_BIND)
    }

    /// Binds a held source object after checking its child-namespace identity.
    ///
    /// Perform this ordered mount before covering the source path. The object,
    /// symlink bytes and complete mount flags must match the parent pin; missing
    /// or changed sources refuse. Unlike a pathname bind, a final symlink is
    /// installed as the link object. Target parents must not redirect through
    /// symlinks. Only recursive binding and an explicit readonly seal may be
    /// added; propagation isolation is a separate preceding operation.
    ///
    /// Keep the source's parent guard until reaping and retire all local source
    /// handles in the child after setup, before starting any workers. Pinned
    /// mounts refuse shared-memory or shared-descriptor clone configurations.
    pub fn bind_pinned<S: AsRef<OsStr>>(source: &PinnedMountSource, destination: S) -> Self {
        let mut mount = Self::bind(OsStr::from_bytes(source.path().to_bytes()), destination);
        mount.pinned_source = Some(source.clone());
        mount
    }

    pub(super) fn has_pinned_source(&self) -> bool {
        self.pinned_source.is_some()
    }

    /// Move/rename a mount.
    pub fn rename<S: AsRef<OsStr>, D: AsRef<OsStr>>(source: S, destination: D) -> Self {
        Self::new(destination)
            .source(source)
            .flags(MountFlags::MS_MOVE)
    }

    /// Mount a fresh devpts file system. The target is usually `/dev/pts`.
    ///
    /// In order for this devpts to be private and independent of other devpts
    /// (i.e., for containers), use:
    /// ```no_compile
    /// Mount::devpts("/dev/pts").data("newinstance,ptmxmode=0666")
    /// ```
    /// And either make `/dev/ptmx` a symlink pointing to `/dev/pts/ptmx` or
    /// bind-mount it.
    ///
    /// See also: <https://www.kernel.org/doc/Documentation/filesystems/devpts.txt>
    pub fn devpts<S: AsRef<OsStr>>(target: S) -> Self {
        Self::new(target).fstype("devpts")
    }

    /// Mount a fresh proc file system at `/proc`.
    pub fn proc() -> Self {
        Self::new("/proc").fstype("proc")
    }

    /// Mount an overlay file system.
    ///
    /// NOTE: This only works in Linux 5.11 or newer when mounted from a user
    /// namespace. Otherwise, you need real root privileges to mount an
    /// overlayfs.
    ///
    /// An overlay filesystem combines two filesystems - an upper filesystem and
    /// a lower filesystem. When a name exists in both filesystems, the object
    /// in the upper filesystem is visible while the object in the lower
    /// filesystem is either hidden or, in the case of directories, merged with
    /// the upper object.
    ///
    /// In other words, the `lowerdir` and `upperdir` are combined into a
    /// directory `merged` using `workdir` as a temporary work area.
    ///
    /// The lower filesystem can be any filesystem supported by Linux and does
    /// not need to be writable. The lower filesystem can even be another
    /// overlayfs. The upper filesystem should be writable.
    ///
    /// See <https://www.kernel.org/doc/html/latest/filesystems/overlayfs.html> for
    /// more information.
    ///
    /// # Arguments
    ///
    /// * `lowerdir` - The lower directory of the overlay. Can be any filesystem
    ///   and does not need to be writable. This directory is never
    ///   modified by writes to `merged`.
    /// * `upperdir` - The upper directory of the overlay. This is where all
    ///   changes to `merged` are collected. Does not need to be
    ///   empty, but should be when starting a new overlay from
    ///   scratch.
    /// * `workdir`  - The work directory. This should always be empty.
    /// * `merged`   - The combination of `lowerdir` and `upperdir`.
    pub fn overlay(lowerdir: &Path, upperdir: &Path, workdir: &Path, merged: &Path) -> Self {
        // TODO: Since there can actually be multiple lowerdirs, it might be
        // more ergonomic to return an `OverlayBuilder` instead.
        let options = format!(
            "lowerdir={},upperdir={},workdir={}",
            lowerdir.display(),
            upperdir.display(),
            workdir.display()
        );

        Self::new(merged)
            .fstype("overlay")
            .source("overlay")
            .data(options)
    }

    /// Creates a temporary file system at the location specified.
    pub fn tmpfs<S: AsRef<OsStr>>(target: S) -> Self {
        Self::new(target).fstype("tmpfs")
    }

    /// Creates a sys file system at the location specified. The target directory
    /// is usually `/sys`. This is useful when creating a network namespace.
    pub fn sysfs<S: AsRef<OsStr>>(target: S) -> Self {
        Self::new(target).fstype("sysfs")
    }

    /// Sets the mount point target.
    pub fn target<S: AsRef<OsStr>>(mut self, target: S) -> Self {
        let (t, ok) = checked_cstring(target);
        self.target = t;
        self.unrepresentable |= !ok;
        self
    }

    /// Returns the mount point target path.
    pub fn get_target(&self) -> &Path {
        Path::new(OsStr::from_bytes(self.target.to_bytes()))
    }

    /// Sets the source of the mount.
    pub fn source<S: AsRef<OsStr>>(mut self, path: S) -> Self {
        let (v, ok) = checked_cstring(path);
        self.source = Some(v);
        self.unrepresentable |= !ok;
        // A new lexical source cannot silently replace a pinned object.
        self.unrepresentable |= self.pinned_source.is_some();
        self
    }

    /// Returns the mount point source path (if any).
    pub fn get_source(&self) -> Option<&Path> {
        self.source
            .as_ref()
            .map(|s| Path::new(OsStr::from_bytes(s.to_bytes())))
    }

    /// Creates a missing target for a bind mount or a new tmpfs mount.
    ///
    /// New tmpfs targets must be absolute directories with no symlink path
    /// components. Creation happens after preceding mounts, so a nested target
    /// can live in a fresh filesystem. Tmpfs remount, move, recursive and
    /// propagation operations do not create targets. Bind behavior is unchanged.
    pub fn touch_target(mut self) -> Self {
        self.touch_target = true;
        self
    }

    /// Adds mount flags.
    pub fn flags(mut self, flags: MountFlags) -> Self {
        self.flags |= flags;
        self
    }

    /// Make the file system read-only.
    pub fn readonly(mut self) -> Self {
        self.flags |= MountFlags::MS_RDONLY;
        self
    }

    // TODO-HUMAN-REVIEW(PR-615)
    /// Allows a new writable proc mount that fails with `EPERM` to retry read-only.
    ///
    /// This explicitly permits the resulting mount to be less capable than
    /// requested. It has no effect on non-proc or already-read-only mounts,
    /// remounts, bind mounts, moves, or propagation changes.
    pub fn allow_readonly_fallback(mut self) -> Self {
        self.allow_readonly_fallback = true;
        self
    }

    /// Skips this bind mount when its source no longer exists at install
    /// time (`ENOENT`), instead of failing container setup. Only bind
    /// mounts skip: an optional non-bind mount refuses every failure as
    /// usual. A bind whose source still exists is never skipped either:
    /// any other `ENOENT` (for example a missing target) refuses as usual.
    ///
    /// For mounts assembled from a directory snapshot taken before the
    /// clone: an entry that vanishes in between is then absent, as it
    /// would have been had the snapshot run a moment later. A skipped
    /// bind mount DELETES its target where `lstat` proves it is only a
    /// placeholder (an empty regular file, or a directory `rmdir` finds
    /// empty), so no empty placeholder remains where the entry would
    /// have been; any other target is left intact and still refuses
    /// setup. That check-then-delete is sound only where no one else can
    /// swap the target between the two calls: the target's parent
    /// directory has to be private to the caller. Every other failure
    /// still refuses setup. This has no effect
    /// on a pinned bind ([`Mount::bind_pinned`]), whose checked identity
    /// refusal is the point: a vanished pinned source reports
    /// `ENOENT`/`ESTALE` as before.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Pauses a read-only bind between its two steps: after the bind, and
    /// before the remount of its target path that makes it read-only (Linux
    /// ignores `MS_RDONLY` on a new bind). There the mount creates the regular
    /// file `arrived`, without blocking, and then waits until a file `release`
    /// exists, looking every 10 ms, until a `CLOCK_MONOTONIC` deadline `limit`
    /// after its arrival. Past the deadline the remount step fails with
    /// `ETIMEDOUT` ([`Context::BindRemount`]), so a pause nobody releases
    /// fails the mount instead of holding it forever. Each sleep it asks for is
    /// capped to the time left at its last clock reading, so a descheduled
    /// sleep can overshoot the deadline but never admits a release seen after
    /// it. A marker it cannot create, or an existing file there that is not a
    /// regular file (a FIFO, say), fails the remount step at once with the
    /// error. An existing regular file is reused, so a setup run again passes
    /// the same pause again; a caller that waits for a later arrival removes
    /// the marker first. A test can change what the target path names while
    /// the mount is known to be in that window, and then let it go on. Any
    /// other mount, and a pinned bind, which remounts through a descriptor,
    /// never takes the pause; but a pause path that is not a C string still
    /// refuses the mount it is set on, at its first step, as any
    /// unrepresentable string does. Only code that builds the mount sets a
    /// pause: Reverie takes none from an environment variable, an argument or
    /// a file. Like the rest of a mount, the pause allocates nothing.
    pub fn pause_before_remount<A: AsRef<OsStr>, R: AsRef<OsStr>>(
        mut self,
        arrived: A,
        release: R,
        limit: Duration,
    ) -> Self {
        let (arrived, arrived_ok) = checked_cstring(arrived);
        let (release, release_ok) = checked_cstring(release);
        self.unrepresentable |= !(arrived_ok && release_ok);
        self.remount_pause = Some(RemountPause {
            arrived,
            release,
            limit,
            poll: REMOUNT_PAUSE_POLL,
        });
        self
    }

    /// Makes a bind mount recursive.
    pub fn recursive(mut self) -> Self {
        self.flags |= MountFlags::MS_REC;
        self
    }

    /// Makes this mount point private. Mount and unmount events do not propagate
    /// into or out of this mount point.
    pub fn private(mut self) -> Self {
        self.flags |= MountFlags::MS_PRIVATE;
        self
    }

    /// Make this mount point shared. Mount and unmount events immediately under
    /// this mount point will propagate to the other mount points that are
    /// members of this mount's peer group. Propagation here means that the same
    /// mount or unmount will automatically occur under all of the other mount
    /// points in the peer group. Conversely, mount and unmount events that take
    /// place under peer mount points will propagate to this mount point.
    pub fn shared(mut self) -> Self {
        self.flags |= MountFlags::MS_SHARED;
        self
    }

    /// Same as specifying both [`Mount::recursive`] and [`Mount::private`].
    pub fn rprivate(mut self) -> Self {
        self.flags |= MountFlags::MS_REC | MountFlags::MS_PRIVATE;
        self
    }

    /// Same as specifying both [`Mount::recursive`] and [`Mount::shared`].
    pub fn rshared(mut self) -> Self {
        self.flags |= MountFlags::MS_REC | MountFlags::MS_SHARED;
        self
    }

    /// Sets the filesystem type.
    pub fn fstype<S: AsRef<OsStr>>(mut self, fstype: S) -> Self {
        let (v, ok) = checked_cstring(fstype);
        self.fstype = Some(v);
        self.unrepresentable |= !ok;
        self
    }

    /// Sets any additional data required by the mount.
    pub fn data<S: AsRef<OsStr>>(mut self, data: S) -> Self {
        let (v, ok) = checked_cstring(data);
        self.data = Some(v);
        self.unrepresentable |= !ok;
        self
    }

    fn source_ptr(&self) -> *const libc::c_char {
        self.source.as_ref().map_or(ptr::null(), |s| s.as_ptr())
    }

    fn target_ptr(&self) -> *const libc::c_char {
        self.target.as_ptr()
    }

    fn fstype_ptr(&self) -> *const libc::c_char {
        self.fstype.as_ref().map_or(ptr::null(), |s| s.as_ptr())
    }

    fn data_ptr(&self) -> *const libc::c_void {
        self.data
            .as_ref()
            .map_or(ptr::null(), |s| s.as_ptr() as *const libc::c_void)
    }

    /// Performs the mount. For bind-mount operations, the target directory or
    /// file is created if [`touch_target`] was used.
    ///
    /// NOTE: This function *must* not allocate since it is called after `fork`
    /// (or `clone`) and before `execve`. Any allocations could cause deadlocks
    /// (which are hard to track down).
    /// The flags the kernel will not let a read-only bind remount drop, read back
    /// from the mount that now exists at our target.
    ///
    /// ⚠️ WITHOUT THIS, A READ-ONLY BIND OF A `nosuid` OR `nodev` SOURCE FAILS
    /// WITH EPERM AND THE WHOLE CONTAINER NEVER SPAWNS. Inside a user namespace
    /// the kernel locks these flags, and `do_remount` refuses any remount that
    /// would clear one; passing only `MS_RDONLY` asks to clear every other flag
    /// the source had. Re-supplying them asks for exactly what is already there,
    /// which is permitted.
    ///
    /// Measured 2026-08-27: this cost a whole validate arm. Hermit places its
    /// frozen `/etc/group` and empty nscd directory in TMPDIR and binds each
    /// read-only, so a TMPDIR on `/run/user/<uid>` -- `nosuid,nodev` on any
    /// systemd host -- failed every container spawn. 610 of that arm's 612 e2e
    /// rows came from this single mount, each one reading as a test result while
    /// measuring nothing. `nosuid` alone and `nodev` alone were each sufficient.
    ///
    /// ⚠️ `statfs` RATHER THAN `/proc/self/mountinfo` BECAUSE THIS RUNS AFTER
    /// FORK, where the surrounding contract forbids allocation. `statfs` is one
    /// syscall onto a caller-owned buffer and parses nothing.
    ///
    /// ⚠️ AND ONLY THE FLAGS WHOSE `ST_` AND `MS_` VALUES COINCIDE. That is true
    /// for `NOSUID`, `NODEV`, `NOEXEC`, `NOATIME` and `NODIRATIME`, and FALSE for
    /// `RELATIME`: `ST_RELATIME` is 0x1000 while `MS_RELATIME` is 0x200000, so
    /// copying the raw bits across would set `MS_SYNCHRONOUS`-adjacent garbage
    /// rather than the flag intended. Relatime is therefore left out; the kernel
    /// keeps the existing atime policy when a remount names none.
    fn locked_source_flags(&self) -> MountFlags {
        // SAFETY: `statfs` writes only into `buffer`, and `target_ptr` is a
        // NUL-terminated C string owned by `self` for the whole call.
        let mut buffer = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        let flags = unsafe {
            if libc::statvfs(self.target_ptr(), buffer.as_mut_ptr()) != 0 {
                // Cannot read the source, so add nothing: the remount then
                // behaves exactly as it did before this function existed.
                return MountFlags::empty();
            }
            buffer.assume_init().f_flag
        };
        let mut preserved = MountFlags::empty();
        for (probe, flag) in [
            (libc::ST_NOSUID, MountFlags::MS_NOSUID),
            (libc::ST_NODEV, MountFlags::MS_NODEV),
            (libc::ST_NOEXEC, MountFlags::MS_NOEXEC),
            (libc::ST_NOATIME, MountFlags::MS_NOATIME),
            (libc::ST_NODIRATIME, MountFlags::MS_NODIRATIME),
        ] {
            if flags & probe != 0 {
                preserved |= flag;
            }
        }
        preserved
    }

    fn mount_with_flags(&self, flags: MountFlags) -> Result<(), Errno> {
        // SAFETY: Every non-null pointer comes from a live `CString` owned by
        // `self`, and `target` is always present. `mount` only borrows these
        // buffers for the duration of the syscall.
        Errno::result(unsafe {
            libc::mount(
                self.source_ptr(),
                self.target_ptr(),
                self.fstype_ptr(),
                flags.bits(),
                self.data_ptr(),
            )
        })?;

        Ok(())
    }

    fn readonly_proc_fallback(&self, error: Errno) -> Option<MountFlags> {
        let is_proc = self
            .fstype
            .as_ref()
            .is_some_and(|fstype| fstype.as_bytes() == b"proc");
        // These operations ignore the filesystem type, so `proc` does not
        // identify the filesystem being changed. Only a new mount can retry.
        let other_operations = MountFlags::MS_REMOUNT
            | MountFlags::MS_BIND
            | MountFlags::MS_MOVE
            | MountFlags::MS_SHARED
            | MountFlags::MS_PRIVATE
            | MountFlags::MS_SLAVE
            | MountFlags::MS_UNBINDABLE;
        (self.allow_readonly_fallback
            && error == Errno::EPERM
            && is_proc
            && !self.flags.intersects(other_operations)
            && !self.flags.contains(MountFlags::MS_RDONLY))
        .then_some(self.flags | MountFlags::MS_RDONLY)
    }

    fn mount_pinned(&self, source: &PinnedMountSource) -> Result<(), Errno> {
        let allowed = MountFlags::MS_BIND | MountFlags::MS_REC | BIND_RESTRICTIONS;
        if !self.flags.contains(MountFlags::MS_BIND)
            || !(self.flags - allowed).is_empty()
            || self.fstype.is_some()
            || self.data.is_some()
        {
            return Err(Errno::EINVAL);
        }
        // This nonblocking ownership lease remains alive through all mount
        // effects and destination validation. A concurrent retire must refuse.
        let lease = source.lease()?;
        let reopened = lease.reopen()?;
        self.prepare_pinned_target(lease.identity().is_dir())?;
        let mut path = [0; 64];
        let path = descriptor_path(b"/proc/self/fd/", reopened.as_raw_fd(), &mut path)?;
        // SAFETY: both paths are live NUL-terminated buffers, and reopened owns
        // the verified current-namespace object for this entire operation.
        Errno::result(unsafe {
            libc::mount(
                path.as_ptr(),
                self.target_ptr(),
                ptr::null(),
                self.flags.bits(),
                ptr::null(),
            )
        })?;
        let destination = Fd::open_c(
            self.target_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )?;
        lease.validate(destination.as_raw_fd(), MountFlags::empty())?;
        let restrictions = self.flags & BIND_RESTRICTIONS;
        if !restrictions.is_empty() {
            // Use the destination descriptor so a bound symlink's referent is
            // never mistaken for the mount being sealed.
            let mut path = [0; 64];
            let path = descriptor_path(b"/proc/self/fd/", destination.as_raw_fd(), &mut path)?;
            let mut flags = MountFlags::MS_BIND | MountFlags::MS_REMOUNT | restrictions;
            for (probe, flag) in [
                (libc::ST_RDONLY, MountFlags::MS_RDONLY),
                (libc::ST_NOSUID, MountFlags::MS_NOSUID),
                (libc::ST_NODEV, MountFlags::MS_NODEV),
                (libc::ST_NOEXEC, MountFlags::MS_NOEXEC),
                (libc::ST_NOATIME, MountFlags::MS_NOATIME),
                (libc::ST_NODIRATIME, MountFlags::MS_NODIRATIME),
            ] {
                if lease.identity().flags & probe != 0 {
                    flags |= flag;
                }
            }
            // SAFETY: the descriptor's magic path names the actual bound mount.
            Errno::result(unsafe {
                libc::mount(
                    ptr::null(),
                    path.as_ptr(),
                    ptr::null(),
                    flags.bits(),
                    ptr::null(),
                )
            })?;
        }
        lease.validate(destination.as_raw_fd(), restrictions)?;
        // The final bound link was checked through its nofollow descriptor.
        // Parent components must still be directories, including for links.
        self.verify_pinned_target(None, false)?;
        Ok(())
    }

    fn prepare_pinned_target(&self, directory: bool) -> Result<(), Errno> {
        self.verify_pinned_target(Some(directory), self.touch_target)?;
        if self.touch_target {
            if directory {
                create_dir_all(&self.target, 0o777)?;
            } else {
                touch_path(&self.target, 0o666, 0o777)?;
            }
        }
        self.verify_pinned_target(Some(directory), false)
    }

    fn verify_pinned_target(&self, directory: Option<bool>, missing: bool) -> Result<(), Errno> {
        let bytes = self.target.as_bytes_with_nul();
        if bytes.first() != Some(&b'/') {
            return Err(Errno::EINVAL);
        }
        let mut path = [0 as libc::c_char; libc::PATH_MAX as usize];
        if bytes.len() > path.len() {
            return Err(Errno::ENAMETOOLONG);
        }
        for (target, source) in path.iter_mut().zip(bytes) {
            *target = *source as libc::c_char;
        }
        for index in 1..bytes.len() {
            let final_component = index == bytes.len() - 1;
            if !final_component && bytes[index] != b'/' {
                continue;
            }
            if final_component && directory.is_none() {
                continue;
            }
            let saved = path[index];
            path[index] = 0;
            let mut object = MaybeUninit::<libc::stat>::uninit();
            // SAFETY: this prefix is NUL-terminated and the output is local.
            let result = unsafe { libc::lstat(path.as_ptr(), object.as_mut_ptr()) };
            path[index] = saved;
            if result != 0 {
                let error = Errno::last();
                if missing && error == Errno::ENOENT {
                    continue;
                }
                return Err(error);
            }
            // SAFETY: lstat succeeded and initialized object.
            let kind = unsafe { object.assume_init() }.st_mode & libc::S_IFMT;
            if kind == libc::S_IFLNK {
                return Err(Errno::ELOOP);
            }
            if !final_component || directory == Some(true) {
                if kind != libc::S_IFDIR {
                    return Err(Errno::ENOTDIR);
                }
            } else if kind == libc::S_IFDIR {
                return Err(Errno::EISDIR);
            }
        }
        Ok(())
    }

    /// [`Mount::mount_in_steps`], with the errno alone.
    #[cfg(test)]
    pub(super) fn mount(&mut self) -> Result<(), Errno> {
        self.mount_in_steps().map_err(Errno::from)
    }

    /// [`Mount::mount`], naming the step that failed: [`Context::BindRemount`]
    /// for the remount of a read-only bind's target path that makes it
    /// read-only, [`Context::Mount`] for every other step.
    pub(super) fn mount_in_steps(&mut self) -> Result<(), Error> {
        if self.mount_without_remount().context(Context::Mount)? == Installed::Skipped {
            return Ok(());
        }

        // Linux ignores MS_RDONLY on the initial bind mount. Apply per-mount flags with the
        // required bind remount so a read-only bind cannot mutate its source inode. A pinned
        // bind seals itself through its descriptor (`mount_pinned`).
        if self.pinned_source.is_none()
            && self.flags.contains(MountFlags::MS_BIND)
            && self.flags.contains(MountFlags::MS_RDONLY)
        {
            self.pause_before_remount_if_asked()
                .context(Context::BindRemount)?;
            Errno::result(unsafe {
                libc::mount(
                    ptr::null(),
                    self.target_ptr(),
                    ptr::null(),
                    (self.flags | MountFlags::MS_REMOUNT | self.locked_source_flags()).bits(),
                    ptr::null(),
                )
            })
            .context(Context::BindRemount)?;
        }

        Ok(())
    }

    /// The pause [`Mount::pause_before_remount`] asks for, if it does: the
    /// arrival marker, then the wait for the release ([`wait_for_release`]).
    fn pause_before_remount_if_asked(&self) -> Result<(), Errno> {
        let Some(pause) = &self.remount_pause else {
            return Ok(());
        };
        mark_arrival(&pause.arrived)?;
        wait_for_release(pause, &mut SystemPause)
    }

    /// Every step of [`Mount::mount`] but the read-only remount of a bind.
    fn mount_without_remount(&mut self) -> Result<Installed, Errno> {
        // ⚠️ REFUSE, DO NOT PANIC. A path/fstype/data string that is not a valid
        // C string was recorded at build time (see `unrepresentable`). This is
        // the first point that can report it, and it is a plain flag test
        // because this runs after fork where allocation is not available.
        // EINVAL is what the kernel returns for a malformed mount argument, so
        // the caller's existing error path already knows what to do with it.
        if self.unrepresentable {
            return Err(Errno::EINVAL);
        }
        if let Some(source) = &self.pinned_source {
            return self.mount_pinned(source).map(|()| Installed::Mounted);
        }
        let other_operations = MountFlags::MS_BIND
            | MountFlags::MS_REMOUNT
            | MountFlags::MS_MOVE
            | MountFlags::MS_SHARED
            | MountFlags::MS_PRIVATE
            | MountFlags::MS_SLAVE
            | MountFlags::MS_UNBINDABLE
            | MountFlags::MS_REC;
        if self.touch_target
            && self
                .fstype
                .as_ref()
                .is_some_and(|fstype| fstype.as_bytes() == b"tmpfs")
            && !self.flags.intersects(other_operations)
        {
            // The same allocation-free nofollow checks used for pinned targets
            // refuse links and non-directories before and after creation.
            self.prepare_pinned_target(true)?;
        }
        // NOTE: Although we can't allocate here, we can safely *modify* `self`.
        // When this function is called, we have forked virtual memory and any
        // modifications we make are copy-on-write and lost when `execve` is
        // called. Thus, this function takes `self` by mutable reference.
        if self.flags.contains(MountFlags::MS_BIND) && self.touch_target {
            // Bind mounts will fail unless the destination path exists, so it
            // is convenient to create it automatically.
            //
            // One reason for doing this here instead of the parent process is
            // because the target may not yet exist until we mount it. For
            // example, if we want to create a `/tmp` (tmpfs) folder and then
            // bind-mount some files or directories into it, pre-creating the
            // destination directories won't work because they'll get created in
            // a different tmpfs.
            if let Some(src) = &self.source {
                let is_dir = match FileType::new(src.as_ptr()) {
                    Ok(file_type) => file_type.is_dir(),
                    Err(Errno::ENOENT)
                        if self.optional && self.flags.contains(MountFlags::MS_BIND) =>
                    {
                        return Ok(Installed::Skipped);
                    }
                    Err(error) => return Err(error),
                };
                if is_dir {
                    create_dir_all(&self.target, 0o777)?;
                } else {
                    touch_path(&self.target, 0o666, 0o777)?;
                }
            }
        }

        match self.mount_with_flags(self.flags) {
            Err(Errno::ENOENT) if self.optional && self.flags.contains(MountFlags::MS_BIND) => {
                // Only a vanished source may skip: with the source still
                // present, the ENOENT names something else (a missing
                // target or parent), which refuses as usual.
                if let Some(source) = &self.source {
                    // SAFETY: `source` is a live NUL-terminated buffer
                    // owned by `self`, and the stat buffer is caller-owned.
                    let source_missing = unsafe {
                        let mut stat = MaybeUninit::<libc::stat>::uninit();
                        libc::lstat(source.as_ptr(), stat.as_mut_ptr()) != 0
                            && Errno::last() == Errno::ENOENT
                    };
                    if !source_missing {
                        return Err(Errno::ENOENT);
                    }
                }
                // A skipped bind leaves no entry behind: remove its
                // target only where `lstat` proves it is a placeholder
                // (an empty regular file, or a directory `rmdir` finds
                // empty), so the path is absent rather than an empty
                // placeholder. Anything else at the target -- a non-empty
                // or special file, a symbolic link, a directory with
                // contents -- is real data this skip must never delete:
                // it still refuses setup with the original `ENOENT`, the
                // target intact.
                if self.flags.contains(MountFlags::MS_BIND) {
                    // SAFETY: `target_ptr` is a live NUL-terminated buffer
                    // owned by `self`, and both calls write only into the
                    // caller-owned stat buffer.
                    let removed = unsafe {
                        let mut stat = MaybeUninit::<libc::stat>::uninit();
                        if libc::lstat(self.target_ptr(), stat.as_mut_ptr()) != 0 {
                            // Already absent is the skipped state itself.
                            Errno::last() == Errno::ENOENT
                        } else {
                            let stat = stat.assume_init();
                            let kind = stat.st_mode & libc::S_IFMT;
                            if kind == libc::S_IFREG && stat.st_size == 0 {
                                libc::unlink(self.target_ptr()) == 0
                            } else if kind == libc::S_IFDIR {
                                libc::rmdir(self.target_ptr()) == 0
                            } else {
                                false
                            }
                        }
                    };
                    if !removed {
                        return Err(Errno::ENOENT);
                    }
                }
                Ok(Installed::Skipped)
            }
            Err(error) => match self.readonly_proc_fallback(error) {
                Some(flags) => self.mount_with_flags(flags).map(|()| Installed::Mounted),
                None => Err(error),
            },
            Ok(()) => Ok(Installed::Mounted),
        }
    }
}

/// What [`Mount::mount_without_remount`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Installed {
    /// The mount is in place, so a read-only bind goes on to its remount.
    Mounted,
    /// An optional bind whose source had vanished was skipped
    /// ([`Mount::optional`]): there is no bind to pause at or remount.
    Skipped,
}

impl Bind {
    /// Creates a new bind mount. The `target` is optional because it is often
    /// convenient to use an identical `source` and `target` directory. If
    /// `target` is `None`, then it is interpretted as being the same as
    /// `source`.
    pub fn new<S, T>(source: S, target: T) -> Self
    where
        S: AsRef<OsStr>,
        T: AsRef<OsStr>,
    {
        let (src, src_ok) = checked_cstring(source);
        let (tgt, tgt_ok) = checked_cstring(target);
        debug_assert!(
            src_ok && tgt_ok,
            "Bind path not representable as a C string; the Mount it converts \
             into is marked unrepresentable and its mount() will fail EINVAL"
        );
        Self {
            source: src,
            target: tgt,
            readonly: false,
        }
    }

    /// Marks this bind read-only.
    pub fn readonly(mut self) -> Self {
        self.readonly = true;
        self
    }
}

impl From<Bind> for Mount {
    fn from(b: Bind) -> Self {
        let src_empty = b.source.as_bytes().is_empty();
        let tgt_empty = b.target.as_bytes().is_empty();
        Self {
            source: Some(b.source),
            target: b.target,
            fstype: None,
            flags: MountFlags::MS_BIND
                | if b.readonly {
                    MountFlags::MS_RDONLY
                } else {
                    MountFlags::empty()
                },
            data: None,
            touch_target: false,
            allow_readonly_fallback: false,
            optional: false,
            pinned_source: None,
            remount_pause: None,
            // A Bind built from a path that was not representable as a C string
            // holds an EMPTY CString (see `checked_cstring`). Empty source or
            // target is never a valid bind mount, so it carries the refusal
            // forward rather than attempting mount(2) with "".
            unrepresentable: src_empty || tgt_empty,
        }
    }
}

impl From<&str> for Bind {
    fn from(s: &str) -> Self {
        // Docker's `-v` option suffix: `source:target:ro` is read-only,
        // `:rw` is the writable default spelled out. The suffix is taken
        // from the right and only when a source:target split precedes it,
        // so a bare path never loses its tail. Any other suffix stays part
        // of the target, exactly as before this spelling existed.
        let (s, readonly) = match s
            .strip_suffix(":ro")
            .map(|rest| (rest, true))
            .or_else(|| s.strip_suffix(":rw").map(|rest| (rest, false)))
        {
            Some((rest, readonly)) if rest.contains(':') => (rest, readonly),
            _ => (s, false),
        };
        if let Some((source, target)) = s.split_once(':') {
            Self {
                source: checked_cstring(source).0,
                target: checked_cstring(target).0,
                readonly,
            }
        } else {
            // A Rust `&str` may contain an interior NUL, so this path panicked
            // too. An unrepresentable path becomes empty here and the Mount it
            // converts into refuses with EINVAL.
            let source = checked_cstring(s).0;
            let target = source.clone();
            Self {
                source,
                target,
                readonly,
            }
        }
    }
}

impl FromStr for Bind {
    type Err = Infallible;

    /// Parses bind mounts of the following forms:
    ///  1. "path/to/source"
    ///  2. "path/to/source:path/to/dest"
    ///  3. "path/to/source:path/to/dest:ro" (":rw" spells the default)
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from(s))
    }
}

/// An error from parsing a mount.
#[derive(thiserror::Error, Debug, Eq, PartialEq)]
pub enum MountParseError {
    /// The `target` key is missing. This is always required.
    MissingTarget,

    /// An invalid mount option was specified.
    Invalid(String, Option<String>),
}

impl fmt::Display for MountParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::MissingTarget => write!(f, "missing mount target"),
            Self::Invalid(k, v) => match v {
                Some(v) => write!(f, "invalid mount option '{}={}'", k, v),
                None => write!(f, "invalid mount option '{}'", k),
            },
        }
    }
}

impl FromStr for Mount {
    type Err = MountParseError;

    /// Parses a [`Mount`]. This accepts the same syntax as Docker mounts where
    /// each mount consists of a comma-separated key-value list.
    ///
    /// See <https://docs.docker.com/storage/bind-mounts/> for more information.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut map: HashMap<&str, Option<&str>> = HashMap::new();

        for item in s.split(',') {
            let item = item.trim();

            if item.is_empty() {
                continue;
            }

            let (key, value) = match item.split_once('=') {
                Some((key, value)) => (key, Some(value)),
                None => (item, None),
            };

            map.insert(key, value);
        }

        // The mount target is always required.
        let mut mount = match map
            .remove("target")
            .or_else(|| map.remove("destination"))
            .or_else(|| map.remove("dest"))
            .or_else(|| map.remove("dst"))
            .flatten()
        {
            Some(target) => Mount::new(target),
            None => {
                return Err(MountParseError::MissingTarget);
            }
        };

        if let Some(source) = map.remove("source").or_else(|| map.remove("src")).flatten() {
            mount = mount.source(source);
        }

        let is_bind_mount = if let Some(fstype) = map.remove("type").flatten() {
            if fstype == "bind" {
                true
            } else {
                mount = mount.fstype(fstype);
                false
            }
        } else {
            true
        };

        if is_bind_mount {
            mount = mount.flags(MountFlags::MS_BIND);
        } else {
            // A fresh filesystem's target is only a mountpoint: create it
            // (with the same nofollow checks as pinned targets) instead of
            // failing where it does not exist yet. Bind mounts keep
            // requiring their target.
            mount = mount.touch_target();
        }

        if let Some((key, value)) = map.remove_entry("readonly") {
            if let Some(value) = value {
                // No value should have been specified.
                return Err(MountParseError::Invalid(key.into(), Some(value.to_owned())));
            }

            mount = mount.readonly();
        }

        if let Some(propagation) = map.remove("bind-propagation").flatten() {
            if !is_bind_mount {
                return Err(MountParseError::Invalid(
                    "bind-propagation".into(),
                    Some(propagation.into()),
                ));
            }
            let flags = match propagation {
                "shared" => MountFlags::MS_SHARED,
                "slave" => MountFlags::MS_SLAVE,
                "private" => MountFlags::MS_PRIVATE,
                "rshared" => MountFlags::MS_REC | MountFlags::MS_SHARED,
                "rslave" => MountFlags::MS_REC | MountFlags::MS_SLAVE,
                "rprivate" => MountFlags::MS_REC | MountFlags::MS_PRIVATE,
                _ => {
                    return Err(MountParseError::Invalid(
                        "bind-propagation".into(),
                        Some(propagation.into()),
                    ));
                }
            };

            mount = mount.flags(flags);
        } else if is_bind_mount {
            // Bind mounts are private by default. Propagation flags are a
            // separate mount operation and are invalid on a fresh tmpfs mount.
            mount = mount.flags(MountFlags::MS_REC | MountFlags::MS_PRIVATE);
        }

        // Any left over keys are invalid.
        if let Some((k, v)) = map.into_iter().next() {
            return Err(MountParseError::Invalid(k.into(), v.map(ToOwned::to_owned)));
        }

        Ok(mount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getters_and_setters() {
        let m = Mount::bind("/foo", "/bar");
        assert_eq!(m.get_target(), Path::new("/bar"));
        assert_eq!(m.get_source(), Some(Path::new("/foo")));

        let m = m.target("/baz");
        assert_eq!(m.get_target(), Path::new("/baz"));
    }

    #[test]
    fn proc_mount_retries_readonly_only_after_permission_denial() {
        let proc_mount = Mount::proc();
        assert_eq!(proc_mount.readonly_proc_fallback(Errno::EPERM), None);
        assert_eq!(
            proc_mount
                .clone()
                .allow_readonly_fallback()
                .readonly_proc_fallback(Errno::EPERM),
            Some(MountFlags::MS_RDONLY)
        );
        assert_eq!(
            proc_mount
                .clone()
                .allow_readonly_fallback()
                .readonly_proc_fallback(Errno::ENOENT),
            None
        );
        assert_eq!(
            Mount::proc()
                .readonly()
                .allow_readonly_fallback()
                .readonly_proc_fallback(Errno::EPERM),
            None
        );
        assert_eq!(
            Mount::tmpfs("/tmp")
                .allow_readonly_fallback()
                .readonly_proc_fallback(Errno::EPERM),
            None
        );
    }

    #[test]
    fn proc_mount_readonly_fallback_excludes_other_mount_operations() {
        let mount = Mount::proc().allow_readonly_fallback();
        let ordinary_flags = MountFlags::MS_NOSUID | MountFlags::MS_NODEV | MountFlags::MS_NOEXEC;
        assert_eq!(
            mount
                .clone()
                .flags(ordinary_flags)
                .readonly_proc_fallback(Errno::EPERM),
            Some(ordinary_flags | MountFlags::MS_RDONLY)
        );
        for flags in [
            MountFlags::MS_REMOUNT,
            MountFlags::MS_BIND,
            MountFlags::MS_MOVE,
            MountFlags::MS_SHARED,
            MountFlags::MS_PRIVATE,
            MountFlags::MS_SLAVE,
            MountFlags::MS_UNBINDABLE,
            MountFlags::MS_REMOUNT | MountFlags::MS_BIND,
            MountFlags::MS_BIND | MountFlags::MS_REC,
            MountFlags::MS_SHARED | MountFlags::MS_REC,
            MountFlags::MS_PRIVATE | MountFlags::MS_REC,
            MountFlags::MS_SLAVE | MountFlags::MS_REC,
            MountFlags::MS_UNBINDABLE | MountFlags::MS_REC,
        ] {
            assert_eq!(
                mount
                    .clone()
                    .flags(flags)
                    .readonly_proc_fallback(Errno::EPERM),
                None,
                "must not retry a non-creation mount operation: {flags:?}"
            );
        }
    }

    #[test]
    fn parse_mount() {
        assert_eq!(
            Mount::from_str("type=bind,source=/foo,target=/bar,readonly"),
            Ok(Mount::bind("/foo", "/bar").readonly().rprivate())
        );
        assert_eq!(
            Mount::from_str("src=/foo,target=/bar,readonly"),
            Ok(Mount::bind("/foo", "/bar").readonly().rprivate())
        );
        assert_eq!(
            Mount::from_str("src=/foo,target=/bar,bind-propagation=rshared"),
            Ok(Mount::bind("/foo", "/bar").rshared())
        );
        // A parsed fresh filesystem creates its target like Docker and like
        // Mount::tmpfs("/test").touch_target() in container setup: the
        // mountpoint must exist for mount(2), and creating it is always
        // right for a fresh filesystem. Bind mounts keep requiring their
        // target, so a typo'd path still fails instead of mounting empty.
        assert_eq!(
            Mount::from_str("type=tmpfs,target=/tmp"),
            Ok(Mount::tmpfs("/tmp").touch_target())
        );
        assert_eq!(
            Mount::from_str("type=tmpfs,target=/tmp,bind-propagation=rprivate"),
            Err(MountParseError::Invalid(
                "bind-propagation".into(),
                Some("rprivate".into())
            ))
        );
        assert_eq!(
            Mount::from_str("target=foo, ,,,"),
            Ok(Mount::new("foo").flags(MountFlags::MS_BIND).rprivate())
        );

        assert_eq!(Mount::from_str(""), Err(MountParseError::MissingTarget));
        assert_eq!(
            Mount::from_str("type=bind,source=/foo,readonly"),
            Err(MountParseError::MissingTarget)
        );
        assert_eq!(
            Mount::from_str("type=tmpfs,target=/foo,wat"),
            Err(MountParseError::Invalid("wat".into(), None))
        );
        assert_eq!(
            Mount::from_str("type=tmpfs,target=/foo,readonly=wat"),
            Err(MountParseError::Invalid(
                "readonly".into(),
                Some("wat".into())
            ))
        );
    }

    #[test]
    fn parse_bind() {
        assert_eq!(Bind::from("source:target"), Bind::new("source", "target"));
        assert_eq!(Bind::from("source"), Bind::new("source", "source"));

        assert_eq!(
            Mount::from(Bind::from("source:target")),
            Mount::bind("source", "target")
        );
    }

    #[test]
    fn parse_bind_readonly_suffix() {
        assert_eq!(
            Bind::from("source:target:ro"),
            Bind::new("source", "target").readonly(),
            "a trailing :ro is the read-only option, not part of the target"
        );
        assert_eq!(
            Bind::from("source:target:rw"),
            Bind::new("source", "target"),
            "a trailing :rw spells the writable default"
        );
        assert_eq!(
            Bind::from("/path:ro"),
            Bind::new("/path", "ro"),
            "a suffix is an option only after a source:target split"
        );
        assert_eq!(
            Bind::from("source:target:shared"),
            Bind::new("source", "target:shared"),
            "an unrecognized suffix stays part of the target, as before"
        );

        assert_eq!(
            Mount::from(Bind::from("source:target:ro")),
            Mount::bind("source", "target").readonly(),
            "a read-only bind converts to a read-only bind mount"
        );
    }
}

#[cfg(test)]
mod unrepresentable_paths_are_refused_not_panics {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    use syscalls::Errno;

    use super::Bind;
    use super::Mount;

    /// ⚠️ THIS PANICKED BEFORE, AND THE PANIC WAS THE WHOLE DEFECT. Every mount
    /// path went through `util::to_cstring`, which unwraps `CString::new`.
    /// Measured on reverie main 200439dc8de9:
    ///   panicked at reverie-process/src/util.rs:17:41:
    ///   called `Result::unwrap()` on an `Err` value: NulError(10, [...])
    /// A recoverable "this path is not a valid C string" aborted the process.
    #[test]
    fn a_target_with_an_interior_nul_refuses_instead_of_panicking() {
        let bad = OsStr::from_bytes(b"/test/work\0dir");
        let mut m = Mount::new(bad);
        assert_eq!(m.mount(), Err(Errno::EINVAL));
    }

    #[test]
    fn an_unrepresentable_source_fstype_or_data_also_refuses() {
        let bad = OsStr::from_bytes(b"bad\0value");
        for m in [
            Mount::new("/test").source(bad),
            Mount::new("/test").fstype(bad),
            Mount::new("/test").data(bad),
        ] {
            let mut m = m;
            assert_eq!(m.mount(), Err(Errno::EINVAL));
        }
    }

    /// ⚠️ THE CONTROL, WITHOUT WHICH THE THREE ABOVE PROVE NOTHING. A refusal
    /// that fired on every mount would pass them and break every real mount.
    /// A representable path must NOT be marked unrepresentable -- it must get
    /// past the flag test and fail (or succeed) on the real syscall instead.
    #[test]
    fn a_representable_path_is_not_refused_by_this_check() {
        let mut m = Mount::new("/test/workdir").fstype("tmpfs");
        let got = m.mount();
        assert_ne!(
            got,
            Err(Errno::EINVAL),
            "a valid path must reach mount(2); EINVAL here means the refusal \
             over-fired and no mount would ever work"
        );
    }

    /// A pause path that cannot be a C string refuses the whole mount at its
    /// first step, as any other unrepresentable string does.
    #[test]
    fn a_pause_path_with_an_interior_nul_refuses_at_the_mount_step() {
        let mut m = Mount::bind("/test/source", "/test/target")
            .readonly()
            .pause_before_remount(
                OsStr::from_bytes(b"/test/arr\0ived"),
                "/test/release",
                std::time::Duration::from_secs(1),
            );
        assert_eq!(
            m.mount_in_steps(),
            Err(crate::Error::new(Errno::EINVAL, crate::Context::Mount))
        );
    }

    /// The pause is taken only by a read-only bind, but its paths are checked
    /// on every mount it is set on: on a writable bind, which never pauses, a
    /// pause path that cannot be a C string still refuses that mount.
    #[test]
    fn a_pause_path_with_an_interior_nul_refuses_a_bind_that_never_pauses() {
        let mut m = Mount::bind("/test/source", "/test/target").pause_before_remount(
            "/test/arrived",
            OsStr::from_bytes(b"/test/rel\0ease"),
            std::time::Duration::from_secs(1),
        );
        assert_eq!(
            m.mount_in_steps(),
            Err(crate::Error::new(Errno::EINVAL, crate::Context::Mount))
        );
    }

    #[test]
    fn a_bind_from_a_str_with_an_interior_nul_refuses() {
        let b = Bind::from("/test/a\0b");
        let mut m: Mount = b.into();
        assert_eq!(m.mount(), Err(Errno::EINVAL));
    }
}

/// The pause's wait ([`wait_for_release`]) on a scripted clock, and its arrival
/// marker ([`mark_arrival`]): what the wait asks to sleep and when it gives up,
/// without a container or the real clock.
#[cfg(test)]
mod remount_pause {
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::time::Duration;

    use syscalls::Errno;

    use super::PauseClock;
    use super::REMOUNT_PAUSE_POLL;
    use super::RemountPause;
    use super::mark_arrival;
    use super::wait_for_release;

    const MS: Duration = Duration::from_millis(1);
    /// Where the scripted clock starts: any time, as the wait only measures
    /// from its first reading.
    const START: Duration = Duration::from_secs(1000);

    /// A clock that stands still except across a sleep, which moves it on by
    /// what the script says that sleep took; and a release that appears at a
    /// set time.
    struct ScriptedClock {
        now: Duration,
        release_at: Option<Duration>,
        /// How long a sleep that asked for `asked` takes.
        took: fn(asked: Duration, sleep: usize) -> Duration,
        asked: Vec<Duration>,
    }

    impl ScriptedClock {
        fn new(release_at: Option<Duration>, took: fn(Duration, usize) -> Duration) -> Self {
            Self {
                now: START,
                release_at: release_at.map(|at| START + at),
                took,
                asked: Vec::new(),
            }
        }
    }

    impl PauseClock for ScriptedClock {
        fn now(&mut self) -> Result<Duration, Errno> {
            Ok(self.now)
        }

        fn released(&mut self, _release: &CString) -> bool {
            self.release_at.is_some_and(|at| self.now >= at)
        }

        fn sleep(&mut self, duration: Duration) {
            let took = (self.took)(duration, self.asked.len());
            self.asked.push(duration);
            self.now += took;
        }
    }

    fn pause(limit: Duration, poll: Duration) -> RemountPause {
        RemountPause {
            arrived: CString::new("/arrived").unwrap(),
            release: CString::new("/release").unwrap(),
            limit,
            poll,
        }
    }

    fn exact(asked: Duration, _sleep: usize) -> Duration {
        asked
    }

    /// With a poll longer than the limit, the one sleep asked for is the
    /// limit itself, and the wait gives up exactly at its deadline.
    #[test]
    fn a_pause_never_released_sleeps_to_its_deadline_and_no_further() {
        let mut clock = ScriptedClock::new(None, exact);
        let result = wait_for_release(&pause(300 * MS, Duration::from_secs(10)), &mut clock);
        assert_eq!(result, Err(Errno::ETIMEDOUT));
        assert_eq!(clock.asked, [300 * MS], "the sleep is capped to the time left");
        assert_eq!(clock.now, START + 300 * MS, "it gives up at its deadline");
    }

    /// Each sleep is the poll interval, and the last is what is left of the
    /// limit.
    #[test]
    fn each_sleep_is_the_poll_capped_to_the_time_left() {
        let mut clock = ScriptedClock::new(None, exact);
        let result = wait_for_release(&pause(25 * MS, 10 * MS), &mut clock);
        assert_eq!(result, Err(Errno::ETIMEDOUT));
        assert_eq!(clock.asked, [10 * MS, 10 * MS, 5 * MS]);
    }

    /// A sleep descheduled past the deadline overshoots it, and a release
    /// that appeared during that sleep, after the deadline, does not count.
    #[test]
    fn a_release_seen_only_after_an_overshooting_sleep_does_not_count() {
        let mut clock = ScriptedClock::new(Some(310 * MS), |asked, _| asked + 50 * MS);
        let result = wait_for_release(&pause(300 * MS, Duration::from_secs(10)), &mut clock);
        assert_eq!(result, Err(Errno::ETIMEDOUT));
        assert_eq!(clock.asked, [300 * MS]);
    }

    /// A release seen before the deadline ends the wait.
    #[test]
    fn a_release_seen_before_the_deadline_ends_the_wait() {
        let mut clock = ScriptedClock::new(Some(150 * MS), exact);
        let result = wait_for_release(&pause(300 * MS, 100 * MS), &mut clock);
        assert_eq!(result, Ok(()));
        assert_eq!(clock.asked, [100 * MS, 100 * MS]);
    }

    /// The pause the builder sets looks for its release every 10 ms, the
    /// cadence the doc gives, each sleep capped to the time left.
    #[test]
    fn the_builder_pause_looks_every_10_ms() {
        let mount = super::Mount::bind("/source", "/target")
            .readonly()
            .pause_before_remount("/arrived", "/release", 25 * MS);
        let pause = mount.remount_pause.clone().unwrap();
        assert_eq!(pause.poll, 10 * MS);
        let mut clock = ScriptedClock::new(None, exact);
        assert_eq!(wait_for_release(&pause, &mut clock), Err(Errno::ETIMEDOUT));
        assert_eq!(clock.asked, [10 * MS, 10 * MS, 5 * MS]);
    }

    /// A release already there ends the wait before any sleep.
    #[test]
    fn a_release_already_there_needs_no_sleep() {
        let mut clock = ScriptedClock::new(Some(Duration::ZERO), exact);
        let result = wait_for_release(&pause(300 * MS, REMOUNT_PAUSE_POLL), &mut clock);
        assert_eq!(result, Ok(()));
        assert!(clock.asked.is_empty(), "{:?}", clock.asked);
    }

    /// A sleep a signal cuts short (here the first two, at once) only makes
    /// the wait look again: it neither gives up early nor stops capping the
    /// sleeps it asks for to the time left.
    #[test]
    fn an_interrupted_sleep_just_looks_again() {
        let mut clock = ScriptedClock::new(None, |asked, sleep| {
            if sleep < 2 { Duration::ZERO } else { asked }
        });
        let result = wait_for_release(&pause(300 * MS, Duration::from_secs(10)), &mut clock);
        assert_eq!(result, Err(Errno::ETIMEDOUT));
        assert_eq!(clock.asked, [300 * MS, 300 * MS, 300 * MS]);
        assert_eq!(clock.now, START + 300 * MS);
    }

    fn c_path(path: &Path) -> CString {
        CString::new(path.as_os_str().as_bytes()).unwrap()
    }

    /// The marker is a new empty regular file.
    #[test]
    fn an_arrival_marker_is_created_as_a_regular_file() {
        let temp = tempfile::tempdir().unwrap();
        let arrived = temp.path().join("arrived");
        assert_eq!(mark_arrival(&c_path(&arrived)), Ok(()));
        let metadata = fs::symlink_metadata(&arrived).unwrap();
        assert!(metadata.is_file(), "{metadata:?}");
        assert_eq!(metadata.len(), 0);
    }

    /// An existing regular file is reused as it is: a setup run again passes
    /// the same pause again.
    #[test]
    fn an_existing_regular_marker_is_reused_as_it_is() {
        let temp = tempfile::tempdir().unwrap();
        let arrived = temp.path().join("arrived");
        fs::write(&arrived, "an earlier arrival").unwrap();
        assert_eq!(mark_arrival(&c_path(&arrived)), Ok(()));
        assert_eq!(fs::read(&arrived).unwrap(), b"an earlier arrival");
    }

    /// A FIFO with no reader fails at once (`ENXIO`) instead of blocking the
    /// open, so the wait's deadline cannot be skipped by a marker that never
    /// opens.
    #[test]
    fn a_fifo_marker_fails_at_once() {
        let temp = tempfile::tempdir().unwrap();
        let arrived = temp.path().join("arrived");
        let fifo = c_path(&arrived);
        // SAFETY: `fifo` is a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        assert_eq!(mark_arrival(&fifo), Err(Errno::ENXIO));
    }

    /// A symbolic link is not followed, even to a regular file.
    #[test]
    fn a_symlink_marker_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file");
        fs::write(&file, "").unwrap();
        let arrived = temp.path().join("arrived");
        std::os::unix::fs::symlink(&file, &arrived).unwrap();
        assert_eq!(mark_arrival(&c_path(&arrived)), Err(Errno::ELOOP));
    }

    /// A directory is not a marker.
    #[test]
    fn a_directory_marker_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(mark_arrival(&c_path(temp.path())), Err(Errno::EISDIR));
    }
}
