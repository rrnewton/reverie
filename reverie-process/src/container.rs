/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use nix::sched::CpuSet;
use nix::sched::sched_setaffinity;
use serde::Serialize;
use serde::de::DeserializeOwned;
use syscalls::Errno;

use super::clone::clone_with_stack;
use super::env::Env;
use super::error::AddContext;
use super::error::Context;
use super::error::Error;
use super::exit_status::ExitStatus;
use super::fd::Fd;
use super::fd::pipe;
use super::fd::write_bytes;
use super::id_map::make_id_map;
use super::mount::Mount;
use super::namespace::Namespace;
use super::net::IfName;
use super::pid::Pid;
use super::pty::PtyChild;
use super::seccomp;
use super::stdio::Stdio;
use super::util::reset_signal_handling;
use super::util::to_cstring;

/// A `Container` is a configuration of how a process shall be spawned. It can,
/// but doesn't have to, include Linux namespace configuration.
///
/// NOTE: Configuring resource limits via cgroups is not yet supported.
pub struct Container {
    pub(super) env: Env,
    current_dir: Option<CString>,
    chroot: Option<CString>,
    pub(super) namespace: Namespace,
    pub(super) stdin: Stdio,
    pub(super) stdout: Stdio,
    pub(super) stderr: Stdio,
    pub(super) uid_map: Vec<(libc::uid_t, libc::uid_t, u32)>,
    pub(super) gid_map: Vec<(libc::uid_t, libc::uid_t, u32)>,
    mounts: Vec<Mount>,
    local_networking_only: bool,
    hostname: Option<OsString>,
    domainname: Option<OsString>,
    pub(super) seccomp: Option<seccomp::Filter>,
    pub(super) seccomp_notify: bool,
    pub(super) pty: Option<PtyChild>,
    /// The core number to which the new process, and descendents, will be
    /// pinned.
    affinity: Option<usize>,
}

impl Default for Container {
    fn default() -> Self {
        Self {
            env: Default::default(),
            current_dir: None,
            chroot: None,
            namespace: Default::default(),
            stdin: Stdio::inherit(),
            stdout: Stdio::inherit(),
            stderr: Stdio::inherit(),
            uid_map: Vec::new(),
            gid_map: Vec::new(),
            mounts: Vec::new(),
            local_networking_only: false,
            hostname: None,
            domainname: None,
            seccomp: None,
            seccomp_notify: false,
            pty: None,
            affinity: None,
        }
    }
}

impl Container {
    /// Returns the configured features that cannot be represented by
    /// `std::process::Command`.
    pub(super) fn std_conversion_blockers(&self) -> Vec<&'static str> {
        // Keep this exhaustive: adding Container state must fail to compile
        // until the standard-command conversion explicitly classifies it.
        let Self {
            env: _,
            current_dir: _,
            chroot,
            namespace,
            stdin: _,
            stdout: _,
            stderr: _,
            uid_map,
            gid_map,
            mounts,
            local_networking_only,
            hostname,
            domainname,
            seccomp,
            seccomp_notify,
            pty,
            affinity,
        } = self;

        let mut blockers = Vec::new();

        if chroot.is_some() {
            blockers.push("chroot");
        }
        if !namespace.is_empty() {
            blockers.push("Linux namespaces");
        }
        if !uid_map.is_empty() {
            blockers.push("user ID mappings");
        }
        if !gid_map.is_empty() {
            blockers.push("group ID mappings");
        }
        if !mounts.is_empty() {
            blockers.push("mounts");
        }
        if *local_networking_only {
            blockers.push("local-only networking");
        }
        if hostname.is_some() {
            blockers.push("hostname");
        }
        if domainname.is_some() {
            blockers.push("domain name");
        }
        if seccomp.is_some() {
            blockers.push("seccomp filter");
        }
        if *seccomp_notify {
            blockers.push("seccomp notification");
        }
        if pty.is_some() {
            blockers.push("pseudoterminal");
        }
        if affinity.is_some() {
            blockers.push("CPU affinity");
        }

        blockers
    }

    /// Creates a new `Container` that inherits everything from the parent
    /// process.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or updates an environment variable mapping.
    ///
    /// Note that environment variable names are case-insensitive (but
    /// case-preserving) on Windows, and case-sensitive on all other platforms.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use reverie_process::Container;
    ///
    /// let container = Container::new().env("PATH", "/bin");
    /// ```
    pub fn env<K, V>(&mut self, key: K, val: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.env.set(key.as_ref(), val.as_ref());
        self
    }

    /// Adds or updates multiple environment variable mappings.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::collections::HashMap;
    /// use std::env;
    ///
    /// use reverie_process::Container;
    /// use reverie_process::Stdio;
    ///
    /// let filtered_env: HashMap<String, String> = env::vars()
    ///     .filter(|&(ref k, _)| k == "TERM" || k == "TZ" || k == "LANG" || k == "PATH")
    ///     .collect();
    ///
    /// let container = Container::new()
    ///     .stdin(Stdio::null())
    ///     .stdout(Stdio::inherit())
    ///     .env_clear()
    ///     .envs(&filtered_env);
    /// ```
    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (k, v) in vars.into_iter() {
            self.env(k, v);
        }
        self
    }

    /// Removes an environment variable mapping.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use reverie_process::Container;
    ///
    /// let container = Container::new().env_remove("PATH");
    /// ```
    pub fn env_remove<K: AsRef<OsStr>>(&mut self, key: K) -> &mut Self {
        self.env.remove(key.as_ref());
        self
    }

    /// Clears the entire environment map for the child process.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use reverie_process::Container;
    ///
    /// let container = Container::new().env_clear();
    /// ```
    pub fn env_clear(&mut self) -> &mut Self {
        self.env.clear();
        self
    }

    /// Sets the working directory for the child process.
    ///
    /// # Interaction with `chroot`
    ///
    /// The working directory is set *after* the chroot is performed (if a chroot
    /// directory is specified). Thus, the path given is relative to the chroot
    /// directory. Otherwise, if no chroot directory is specified, the working
    /// directory is relative to the current working directory of the parent
    /// process at the time the child process is spawned.
    ///
    /// # Platform-specific behavior
    ///
    /// If the program path is relative (e.g., `"./script.sh"`), it's ambiguous
    /// whether it should be interpreted relative to the parent's working
    /// directory or relative to `current_dir`. The behavior in this case is
    /// platform specific and unstable, and it's recommended to use
    /// [`canonicalize`] to get an absolute program path instead.
    ///
    /// [`canonicalize`]: std::fs::canonicalize()
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use reverie_process::Container;
    ///
    /// let container = Container::new().current_dir("/bin");
    /// ```
    pub fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Self {
        self.current_dir = Some(to_cstring(dir.as_ref()));
        self
    }

    /// Sets configuration for the child process's standard input (stdin) handle.
    ///
    /// Defaults to [`Stdio::inherit`] when used with `spawn` or `status`, and
    /// defaults to [`Stdio::piped`] when used with `output`.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use reverie_process::Container;
    /// use reverie_process::Stdio;
    ///
    /// let container = Container::new().stdin(Stdio::null());
    /// ```
    pub fn stdin<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.stdin = cfg.into();
        self
    }

    /// Sets configuration for the child process's standard output (stdout)
    /// handle.
    ///
    /// Defaults to [`Stdio::inherit`] when used with `spawn` or `status`, and
    /// defaults to [`Stdio::piped`] when used with `output`.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use reverie_process::Container;
    /// use reverie_process::Stdio;
    ///
    /// let container = Container::new().stdout(Stdio::null());
    /// ```
    pub fn stdout<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.stdout = cfg.into();
        self
    }

    /// Sets configuration for the child process's standard error (stderr)
    /// handle.
    ///
    /// Defaults to [`Stdio::inherit`] when used with `spawn` or `status`, and
    /// defaults to [`Stdio::piped`] when used with `output`.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use reverie_process::Container;
    /// use reverie_process::Stdio;
    ///
    /// let container = Container::new().stderr(Stdio::null());
    /// ```
    pub fn stderr<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.stderr = cfg.into();
        self
    }

    /// Changes the root directory of the calling process to the specified path.
    /// This directory will be inherited by all child processes of the calling
    /// process.
    ///
    /// Note that changing the root directory may cause the program to not be
    /// found. As such, the program path should be relative to this directory.
    pub fn chroot<P: AsRef<Path>>(&mut self, chroot: P) -> &mut Self {
        self.chroot = Some(to_cstring(chroot.as_ref()));
        self
    }

    /// Unshares parts of the process execution context that are normally shared
    /// with the parent process. This is useful for executing the child process
    /// in a new namespace.
    pub fn unshare(&mut self, namespace: Namespace) -> &mut Self {
        self.namespace |= namespace;
        self
    }

    /// Returns the working directory for the child process.
    ///
    /// This returns None if the working directory will not be changed.
    pub fn get_current_dir(&self) -> Option<&Path> {
        if let Some(dir) = &self.current_dir {
            Some(Path::new(OsStr::from_bytes(dir.to_bytes())))
        } else {
            None
        }
    }

    /// Returns an iterator of the environment variables that will be set when
    /// the process is spawned. Note that this does not include any environment
    /// variables inherited from the parent process.
    pub fn get_envs(&self) -> impl Iterator<Item = (&OsStr, Option<&OsStr>)> {
        self.env.iter()
    }

    /// Returns a mapping of all environment variables that the new child process
    /// will inherit.
    pub fn get_captured_envs(&self) -> BTreeMap<OsString, OsString> {
        self.env.capture()
    }

    /// Gets an environment variable. If the child process is to inherit this
    /// environment variable from the current process, then this returns the
    /// current process's environment variable unless it is to be overridden.
    pub fn get_env<K: AsRef<OsStr>>(&self, env: K) -> Option<Cow<'_, OsStr>> {
        self.env.get_captured(env)
    }

    /// Maps one user ID to another.
    ///
    /// Implies `Namespace::USER`.
    ///
    /// # Example
    ///
    /// This is can be used to gain `CAP_SYS_ADMIN` privileges in the user
    /// namespace by mapping the root user inside the container to the current
    /// user outside of the container.
    ///
    /// ```no_run
    /// use reverie_process::Container;
    ///
    /// let container = Container::new().map_uid(1, unsafe { libc::getuid() });
    /// ```
    ///
    /// # Implementation
    ///
    /// This modifies `/proc/{pid}/uid_map` where `{pid}` is the PID of the child
    /// process. See [`user_namespaces(7)`] for more details.
    ///
    /// [`user_namespaces(7)`]: https://man7.org/linux/man-pages/man7/user_namespaces.7.html
    pub fn map_uid(&mut self, inside_uid: libc::uid_t, outside_uid: libc::uid_t) -> &mut Self {
        self.map_uid_range(inside_uid, outside_uid, 1)
    }

    /// Maps potentially many user IDs inside the new user namespace to user IDs
    /// outside of the user namespace.
    ///
    /// Implies `Namespace::USER`.
    ///
    /// # Implementation
    ///
    /// This modifies `/proc/{pid}/uid_map` where `{pid}` is the PID of the child
    /// process. See [`user_namespaces(7)`] for more details.
    ///
    /// [`user_namespaces(7)`]: https://man7.org/linux/man-pages/man7/user_namespaces.7.html
    pub fn map_uid_range(
        &mut self,
        starting_inside_uid: libc::uid_t,
        starting_outside_uid: libc::uid_t,
        count: u32,
    ) -> &mut Self {
        self.uid_map
            .push((starting_inside_uid, starting_outside_uid, count));
        self.namespace |= Namespace::USER;
        self
    }

    /// Convience function for mapping root (inside the container) to the current
    /// user ID (outside the container). This is useful for gaining new
    /// capabilities inside the container, such as being able to mount file
    /// systems.
    ///
    /// Implies `Namespace::USER`.
    ///
    /// This is the same as:
    /// ```no_run
    /// use reverie_process::Container;
    ///
    /// let container = Container::new()
    ///     .map_uid(0, unsafe { libc::geteuid() })
    ///     .map_gid(0, unsafe { libc::getegid() });
    /// ```
    pub fn map_root(&mut self) -> &mut Self {
        self.map_uid(0, unsafe { libc::geteuid() });
        self.map_gid(0, unsafe { libc::getegid() })
    }

    /// Maps one group ID to another.
    ///
    /// Implies `Namespace::USER`.
    ///
    /// # Implementation
    ///
    /// This modifies `/proc/{pid}/gid_map` where `{pid}` is the PID of the child
    /// process. See [`user_namespaces(7)`] for more details.
    ///
    /// [`user_namespaces(7)`]: https://man7.org/linux/man-pages/man7/user_namespaces.7.html
    pub fn map_gid(&mut self, inside_gid: libc::gid_t, outside_gid: libc::gid_t) -> &mut Self {
        self.map_gid_range(inside_gid, outside_gid, 1)
    }

    /// Maps potentially many group IDs inside the new user namespace to group
    /// IDs outside of the user namespace.
    ///
    /// Implies `Namespace::USER`.
    ///
    /// # Implementation
    ///
    /// This modifies `/proc/{pid}/gid_map` where `{pid}` is the PID of the child
    /// process. See [`user_namespaces(7)`] for more details.
    ///
    /// [`user_namespaces(7)`]: https://man7.org/linux/man-pages/man7/user_namespaces.7.html
    pub fn map_gid_range(
        &mut self,
        starting_inside_gid: libc::gid_t,
        starting_outside_gid: libc::gid_t,
        count: u32,
    ) -> &mut Self {
        self.namespace |= Namespace::USER;
        self.gid_map
            .push((starting_inside_gid, starting_outside_gid, count));
        self
    }

    /// Sets the hostname of the container.
    ///
    /// Implies `Namespace::UTS`, which requires `CAP_SYS_ADMIN`.
    ///
    /// ```no_run
    /// use reverie_process::Container;
    ///
    /// let container = Container::new().map_root().hostname("foobar.local");
    /// ```
    pub fn hostname<S: Into<OsString>>(&mut self, hostname: S) -> &mut Self {
        self.namespace |= Namespace::UTS;
        self.hostname = Some(hostname.into());
        self
    }

    /// Sets the domain name of the container.
    ///
    /// Implies `Namespace::UTS`, which requires `CAP_SYS_ADMIN`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use reverie_process::Container;
    ///
    /// let container = Container::new().map_root().domainname("foobar");
    /// ```
    pub fn domainname<S: Into<OsString>>(&mut self, domainname: S) -> &mut Self {
        self.namespace |= Namespace::UTS;
        self.domainname = Some(domainname.into());
        self
    }

    /// Gets the hostname of the container.
    pub fn get_hostname(&self) -> Option<&OsStr> {
        self.hostname.as_ref().map(AsRef::as_ref)
    }

    /// Gets the domainname of the container.
    pub fn get_domainname(&self) -> Option<&OsStr> {
        self.domainname.as_ref().map(AsRef::as_ref)
    }

    /// Adds a file system to be mounted. Note that these are mounted in the same
    /// order as given.
    ///
    /// Implies `Namespace::MOUNT`. Note that `Namespace::USER` should also have
    /// been set and `map_uid` should have been called in order to gain the
    /// privileges required to mount.
    pub fn mount(&mut self, mount: Mount) -> &mut Self {
        self.namespace |= Namespace::MOUNT;
        self.mounts.push(mount);
        self
    }

    /// Adds multiple mounts.
    pub fn mounts<I>(&mut self, mounts: I) -> &mut Self
    where
        I: IntoIterator<Item = Mount>,
    {
        self.namespace |= Namespace::MOUNT;
        self.mounts.extend(mounts);
        self
    }

    /// Sets up the container to have local networking only. This will prevent
    /// any network communication to the outside world.
    ///
    /// Implies `Namespace::NETWORK` and `Namespace::MOUNT`.
    ///
    /// This also causes a fresh `/sys` to be mounted to avoid seeing the host
    /// network interfaces in `/sys/class/net`.
    pub fn local_networking_only(&mut self) -> &mut Self {
        if !self.local_networking_only {
            self.local_networking_only = true;
            self.namespace |= Namespace::NETWORK;
            self.mount(Mount::sysfs("/sys"));
        }
        self
    }

    /// Sets the seccomp filter. The filter is loaded immediately before `execve`
    /// and *after* all `pre_exec` callbacks have been executed. Thus, you will
    /// still be able to call filtered syscalls from `pre_exec` callbacks.
    pub fn seccomp(&mut self, filter: seccomp::Filter) -> &mut Self {
        self.seccomp = Some(filter);
        self
    }

    /// Indicates that we want to listen for seccomp events using
    /// [seccomp_unotify(2)](https://man7.org/linux/man-pages/man2/seccomp_unotify.2.html).
    ///
    /// If this is set, the seccomp listener file descriptor will be accessible
    /// via the `Child`.
    pub fn seccomp_notify(&mut self) -> &mut Self {
        self.seccomp_notify = true;
        self
    }

    /// Sets the controlling pseudoterminal for the child process).
    ///
    /// In the child process, this has the effect of:
    ///  1. Creating a new session (with `setsid()`).
    ///  2. Using an `ioctl` to set the controlling terminal.
    ///  3. Setting this file descriptor as the stdio streams.
    ///
    /// NOTE: Since this modifies the stdio streams, calling this will reset
    /// [`Self::stdin`], [`Self::stdout`], and [`Self::stderr`] back to
    /// [`Stdio::inherit()`].
    pub fn pty(&mut self, child: PtyChild) -> &mut Self {
        self.pty = Some(child);
        self.stdin = Stdio::inherit();
        self.stdout = Stdio::inherit();
        self.stderr = Stdio::inherit();
        self
    }

    /// Sets the CPU to which the child threads/processes will be pinned.
    pub fn affinity(&mut self, affinity: usize) -> &mut Self {
        self.affinity = Some(affinity);
        self
    }

    /// Whether the calling thread is its process's thread-group leader.
    ///
    /// Sampled in the PARENT, before `clone`, because `PR_SET_PDEATHSIG` binds to
    /// the specific parent THREAD that cloned -- not to the parent process. If a
    /// non-leader thread clones and later exits while its process lives on, the
    /// child is killed even though nothing it depends on has died. Arming only
    /// for the leader makes "parent thread died" and "parent process died" the
    /// same event, which is the property the guard actually wants.
    pub(super) fn cloned_from_group_leader() -> bool {
        // gettid() == getpid() exactly for the thread-group leader.
        unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t == libc::getpid() }
    }

    /// Arm the parent-death signal when this container makes its child the init
    /// of a new PID namespace.
    ///
    /// WHY THIS IS HERE AND NOT IN THE CALLERS. A process that is PID 1 of a PID
    /// namespace does not get default signal dispositions: the kernel DISCARDS
    /// any signal whose disposition the init has not explicitly set. So a guest
    /// that becomes namespace-init ignores SIGTERM, and `timeout` -- even
    /// `timeout --kill-after` -- cannot reap it. On 2026-08-17 that let three
    /// runs survive 28 hours and fill the filesystem.
    ///
    /// The first fix armed this at every launch path someone could find: a
    /// closure wrapper, six record hooks, and finally `--namespace-only`, which
    /// an adversarial review turned up only after the first two were believed
    /// complete. That works and stays correct exactly until the next launch
    /// primitive is added. `setup` is on the path of BOTH `Container::run` and
    /// `Command::spawn`, so arming here makes the guard something every path
    /// passes through rather than something applied to every path someone
    /// remembered.
    ///
    /// WHAT THIS DOES NOT DO, stated so it is not mistaken for more:
    ///
    /// * It is not a full race closure. The window between `clone` returning in
    ///   the parent and this `prctl` running in the child is open; a parent that
    ///   dies inside it leaves an unguarded child. Arming here shrinks that
    ///   window to the few syscalls above rather than the lifetime of a run, but
    ///   closing it needs a parent/child lifetime handshake, which is more
    ///   machinery than a guard.
    /// * A CREDENTIAL-CHANGING `execve` CLEARS the parent-death signal. For
    ///   `Container::run` there is no exec and this is moot; for `Command::spawn`
    ///   a setuid guest silently loses the guard. The durable answer is a
    ///   non-execing PID 1 supervisor with the guest as PID 2, which is a change
    ///   of process topology rather than a guard.
    /// * `execve` also resets caught signals to `SIG_DFL`, so a handler armed
    ///   before exec cannot help an exec'd namespace-init. Only the death signal
    ///   survives, so only the death signal is set here.
    ///
    /// NOTE: called between `clone` and `execve`, so it may only use
    /// async-signal-safe calls and must not allocate.
    fn guard_pid_namespace_init(&self, context: &ChildContext) -> Result<(), Error> {
        if !self.namespace.contains(Namespace::PID) {
            // No new PID namespace: this child gets ordinary signal
            // dispositions and needs no guard.
            return Ok(());
        }

        if !context.cloned_from_group_leader {
            // Deliberately NOT armed. Arming would bind the child's life to a
            // thread whose death does not imply the parent process is gone, so
            // a caller that spawns from a worker thread would see its container
            // killed when that thread finishes. Leaving it unarmed preserves
            // today's behaviour for such callers instead of introducing a new
            // way to lose a running container.
            return Ok(());
        }

        Error::result(
            unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) },
            Context::PDeathSig,
        )?;

        Ok(())
    }

    /// Called by the child process after `clone` to get itself set up for either
    /// `execve` or running an arbitrary function.
    ///
    /// NOTE: Although this function takes `&mut self`, it is only called in the
    /// context of the child process (which has a copy-on-write view of the
    /// parent's virtual memory). Thus, the parent's version isn't actually
    /// modified.
    pub(super) fn setup(
        &mut self,
        context: &ChildContext,
        pre_exec: &mut [Box<dyn FnMut() -> Result<(), Errno> + Send + Sync>],
    ) -> Result<(), Error> {
        // NOTE: This function MUST NOT allocate or deallocate any memory! Doing
        // so can cause random, difficult to diagnose deadlocks.

        if let Some(pty) = self.pty.take() {
            // NOTE: This is done *before* setting the stdio streams so that the
            // user can still override individual streams if they only want them
            // to be partially attached to the tty.
            pty.login().context(Context::Tty)?;
        }

        if let Some(fd) = context.stdin {
            fd.dup2(libc::STDIN_FILENO)
                .context(Context::Stdio)?
                .leave_open();
        }
        if let Some(fd) = context.stdout {
            fd.dup2(libc::STDOUT_FILENO)
                .context(Context::Stdio)?
                .leave_open();
        }
        if let Some(fd) = context.stderr {
            fd.dup2(libc::STDERR_FILENO)
                .context(Context::Stdio)?
                .leave_open();
        }

        unsafe { reset_signal_handling() }.context(Context::ResetSignals)?;

        // As early as the child can manage: everything below here (uid maps,
        // mounts, chroot, seccomp) can fail or block, and an unguarded
        // namespace-init is exactly what we are trying not to leave behind.
        self.guard_pid_namespace_init(context)?;

        // Set up UID and GID maps.
        if !context.uid_map.is_empty() {
            context.map_uid().context(Context::MapUid)?;
        }

        if !context.gid_map.is_empty() {
            context.setgroups(false).context(Context::MapGid)?;
            context.map_gid().context(Context::MapGid)?;
        }

        // Set host name, if any.
        if let Some(name) = &self.hostname {
            Error::result(
                unsafe { libc::sethostname(name.as_bytes().as_ptr() as *const _, name.len()) },
                Context::Hostname,
            )?;
        }

        // Set domain name, if any.
        if let Some(name) = &self.domainname {
            Error::result(
                unsafe { libc::setdomainname(name.as_bytes().as_ptr() as *const _, name.len()) },
                Context::Domainname,
            )?;
        }

        // Mount all the things.
        for mount in &mut self.mounts {
            mount.mount().context(Context::Mount)?;
        }

        // Change root directory. Note that we do this *after* mounting anything
        // so that bind mounts sources that live outside of the chroot directory
        // can work.
        if let Some(chroot) = &self.chroot {
            Error::result(unsafe { libc::chroot(chroot.as_ptr()) }, Context::Chroot)?;
        }

        // Set working directory, if any.
        if let Some(current_dir) = &self.current_dir {
            Error::result(unsafe { libc::chdir(current_dir.as_ptr()) }, Context::Chdir)?;
        }

        // Configure networking.
        // TODO: Generalize this a bit to allow more complex configuration.
        if self.local_networking_only {
            // Need a socket to access the network interface.
            let sock = Fd::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_IP)
                .context(Context::Network)?;

            let loopback = IfName::LOOPBACK;

            // Bring up the loopback interface in the newly mounted sysfs.
            let flags = loopback.get_flags(&sock).context(Context::Network)?;
            let flags = flags | libc::IFF_UP as i16;
            loopback.set_flags(&sock, flags).context(Context::Network)?;
        }

        if let Some(cpu) = self.affinity {
            let mut cpu_set = CpuSet::new();
            cpu_set.set(cpu).context(Context::Affinity)?;
            sched_setaffinity(nix::unistd::Pid::from_raw(0), &cpu_set)
                .context(Context::Affinity)?;
        }

        // NOTE: We must call our pre_exec callbacks BEFORE installing the
        // seccomp filter because our callbacks could be calling syscalls that
        // our seccomp filter may be intending to block.
        for f in pre_exec {
            f().context(Context::PreExec)?;
        }

        // Set up the seccomp filter, if any.
        if let Some(filter) = &self.seccomp {
            use core::sync::atomic::Ordering;

            // no_new_privs must be set or seccomp will not work.
            Error::result(
                unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
                Context::Seccomp,
            )?;

            // NOTE: If the supervisor (parent process) wants to listen for
            // seccomp notifications, we need to be able to pass the file
            // descriptor to the parent. The most common way to do this is to
            // set up a socket connection and send the file descriptor. However,
            // since we just set up a seccomp filter, the filter could apply to
            // any syscalls we make from here on out. This is especially
            // troublesome if we're also ptracing this child because our syscall
            // could result in a premature seccomp stop and cause a deadlock.
            // Thus, instead, we should pass the file descriptor to the parent
            // process without making any syscalls. The only way to do that is
            // to create some shared memory and atomically set an integer.
            if let Some(shared_fd) = context.seccomp_fd {
                use std::os::unix::io::IntoRawFd;

                let fd = filter
                    .load_and_listen()
                    .context(Context::Seccomp)?
                    .into_raw_fd();

                shared_fd.store(fd, Ordering::Relaxed);

                // Wait until the parent changes the value back. The parent only
                // does this after it calls pidfd_getfd to copy the file
                // descriptor into its own file descriptor table. After this,
                // the file descriptor can be safely closed, but we won't do
                // that in order to avoid doing a syscall. The fd will be closed
                // automatically when execve happens anyway.
                //
                // NOTE: Again, we must not perform any syscalls after the
                // seccomp filter has been installed (except for execve of
                // course).
                while shared_fd.load(Ordering::Relaxed) == fd {
                    // Spin spin spin
                }
            } else {
                filter.load().context(Context::Seccomp)?;
            }
        }

        Ok(())
    }

    /// Runs a function in a new process with the specified namespaces unshared. This
    /// blocks until the function itself returns and the process has exited.
    ///
    /// # Safety
    ///
    ///  - This should be called early on in the life of a process, before any
    ///    other threads are created. This reduces the chance that any global
    ///    resources (like the Tokio runtime) have been created yet.
    ///
    ///  - Memory allocated in the parent must not be freed in the child,
    ///    especially if using jemalloc where a separate thread does deallocations.
    pub fn run<F, T>(&mut self, mut f: F) -> Result<T, RunError>
    where
        F: FnMut() -> T,
        T: Serialize + DeserializeOwned,
    {
        let clone_flags = self.namespace.bits() | libc::SIGCHLD;

        let uid_map = &make_id_map(&self.uid_map);
        let gid_map = &make_id_map(&self.gid_map);

        let context = ChildContext {
            // TODO: Honor stdio options. For now, always inherit from the
            // parent process.
            stdin: None,
            stdout: None,
            stderr: None,
            uid_map,
            gid_map,
            seccomp_fd: None,
            cloned_from_group_leader: Container::cloned_from_group_leader(),
        };

        // Use a pipe for getting the result of the function out of the child
        // process.
        let (mut reader, writer) = pipe()?;

        let writer_fd = writer.as_raw_fd();

        // NOTE: Must use a dynamically allocated stack here. Programs expect to
        // have at least 2 MB of stack space and if we've already used up some
        // stack space before this is called we could overflow the stack.
        let mut stack = vec![0u8; 1024 * 1024 * 2];

        // Disable io redirection just before forking. We want the child process to
        // be able to call `println!()` and have that output go to stdout.
        //
        // See: https://github.com/rust-lang/rust/issues/35136
        //
        // Another way around this weirdness is to not use the default
        // `print!()` and `println!()` macros so that we can completely bypass
        // this output capturing.
        #[cfg(feature = "nightly")]
        let output_capture = std::io::set_output_capture(None);

        let result = clone_with_stack(
            || {
                let value = self.setup(&context, &mut []).map(|()| f());

                let mut writer = std::io::BufWriter::new(Fd::new(writer_fd));

                // Serialize this result with bincode and send it to the parent
                // process via a pipe.
                //
                // TODO: Handle serialization errors(?)
                bincode::serde::encode_into_std_write(
                    &value,
                    &mut writer,
                    bincode::config::legacy(),
                )
                .expect("Failed to serialize return value");

                0
            },
            clone_flags,
            &mut stack,
        );

        #[cfg(feature = "nightly")]
        std::io::set_output_capture(output_capture);

        let child = WaitGuard::new(result?);

        // The writer end must be dropped first so that our reader doesn't block
        // forever.
        drop(writer);

        // Read the return value. Note that we do this *before* waiting on the
        // process to exit. Otherwise, for return values that exceed the pipe
        // capacity, we would deadlock.
        let mut buf = Vec::new();
        match reader.read_to_end(&mut buf) {
            Ok(0) => {
                // The writer end was closed before anything could be written.
                // This indicates that the process exited before the return
                // value could be serialized. The only thing we can do in this
                // case is collect the exit status of the process.
                //
                // NOTE: Since we always send `Result<T, _>` through the pipe,
                // we can guarantee that a successful serialization will never
                // be 0 bytes (since it always takes more than 0 bytes to encode
                // that type).
                //
                // NOTE: Since `WaitGuard` is used, we guarantee that the
                // process will be waited on in the other cases.
                Err(RunError::ExitStatus(child.wait()?))
            }
            Ok(n) => {
                // FIXME: Handle errors
                let value: Result<T, Error> =
                    bincode::serde::decode_from_slice(&buf[0..n], bincode::config::legacy())
                        .unwrap()
                        .0;
                Ok(value.unwrap())
            }
            Err(err) => {
                // FIXME: Handle this error
                panic!("Got unexpected error: {}", err)
            }
        }
    }
}

pub(super) struct ChildContext<'a> {
    pub stdin: Option<&'a Fd>,
    pub stdout: Option<&'a Fd>,
    pub stderr: Option<&'a Fd>,
    pub uid_map: &'a [u8],
    pub gid_map: &'a [u8],
    pub seccomp_fd: Option<&'a core::sync::atomic::AtomicI32>,
    /// Whether the thread that called `clone` is its process's thread-group
    /// leader. See `guard_pid_namespace_init` for why this is load-bearing.
    pub cloned_from_group_leader: bool,
}

impl<'a> ChildContext<'a> {
    fn map_uid(&self) -> Result<(), Errno> {
        write_bytes(b"/proc/self/uid_map\0", self.uid_map)
    }

    fn map_gid(&self) -> Result<(), Errno> {
        write_bytes(b"/proc/self/gid_map\0", self.gid_map)
    }

    fn setgroups(&self, allow: bool) -> Result<(), Errno> {
        write_bytes(
            b"/proc/self/setgroups\0",
            if allow { b"allow\0" } else { b"deny\0" },
        )
    }
}

/// An error that ocurred while running a containerized function.
#[derive(thiserror::Error, Debug, Eq, PartialEq)]
pub enum RunError {
    /// An error that occurred while spawning the container.
    #[error("Process failed to spawn: {0}")]
    Spawn(#[from] Error),

    /// The function exited prematurely. This can happen if the function called
    /// `std::process::exit(0)`, preventing the return value from being sent to
    /// the parent. It can also happen if the process panics.
    #[error("Process exited with code: {0:?}")]
    ExitStatus(ExitStatus),
}

impl From<Errno> for RunError {
    fn from(errno: Errno) -> Self {
        Self::Spawn(Error::from(errno))
    }
}

// Helper guard for making sure that the process gets waited on even if an error
// is encountered.
struct WaitGuard(Option<Pid>);

impl WaitGuard {
    pub fn new(pid: Pid) -> Self {
        Self(Some(pid))
    }

    /// Eagerly waits for the pid. Otherwise, it'll get waited on upon drop.
    pub fn wait(mut self) -> Result<ExitStatus, Errno> {
        let pid = self.0.take().unwrap();

        let mut status = 0;
        let ret = Errno::result(unsafe { libc::waitpid(pid.as_raw(), &mut status, 0) })?;
        assert_ne!(ret, 0);

        Ok(ExitStatus::from_raw(status))
    }
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            let mut status = 0;
            unsafe {
                libc::waitpid(pid.as_raw(), &mut status, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use nix::sys::signal::Signal;

    use super::*;

    #[test]
    fn can_panic() {
        let result = Container::new().run::<_, ()>(|| panic!());
        assert!(
            matches!(
                result,
                Err(RunError::ExitStatus(ExitStatus::Signaled(
                    Signal::SIGABRT,
                    _
                )))
            ),
            "Expected Err(ExitStatus(Signaled(SIGABRT, _))), got {:?}",
            result
        );
    }

    #[test]
    fn is_new_process() {
        let my_pid = unsafe { libc::getpid() };

        assert_eq!(
            Container::new().run(|| {
                assert_ne!(unsafe { libc::getpid() }, 1);
                assert_ne!(unsafe { libc::getpid() }, my_pid);
                assert_eq!(unsafe { libc::getppid() }, my_pid);
            }),
            Ok(())
        );
    }

    #[test]
    fn pid_namespace() {
        assert_eq!(
            Container::new()
                .unshare(Namespace::USER | Namespace::PID)
                .run(|| {
                    // New PID namespace, so this should be the init process.
                    assert_eq!(unsafe { libc::getpid() }, 1);
                }),
            Ok(())
        );
    }

    /// Reads back the calling thread's parent-death signal.
    fn pdeathsig() -> libc::c_int {
        let mut sig: libc::c_int = -1;
        assert_eq!(unsafe { libc::prctl(libc::PR_GET_PDEATHSIG, &mut sig) }, 0);
        sig
    }

    /// A PID-namespace init is guarded, end to end, through `setup`.
    ///
    /// THE HARNESS CANNOT HOST THIS DIRECTLY. libtest runs every `#[test]` on a
    /// spawned thread -- even under `--test-threads=1`, which was tried -- so in
    /// a plain unit test the cloning thread is not the group leader and the
    /// guard correctly declines. A test written the obvious way therefore
    /// asserts nothing.
    ///
    /// So it forks first. After `fork` the child process has exactly one thread
    /// and that thread IS the thread-group leader, which is precisely the
    /// condition the guard arms for and also the condition `Container::run`
    /// documents that it wants. The child does the real thing and reports
    /// through its exit status; the parent judges.
    ///
    /// The child must not assert: `setup` runs where allocation is forbidden,
    /// and a failing `assert_eq!` formats its message -- which allocated and
    /// turned a clean failure into a SIGSEGV while this was being written.
    #[test]
    fn pid_namespace_init_is_guarded_structurally() {
        const OK: i32 = 0;
        const NOT_LEADER: i32 = 2;
        const RUN_FAILED: i32 = 3;
        const NOT_INIT: i32 = 4;
        const NOT_GUARDED: i32 = 5;

        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");

        if pid == 0 {
            // Single-threaded child: the group leader by construction.
            let code = if !Container::cloned_from_group_leader() {
                NOT_LEADER
            } else {
                match Container::new()
                    .unshare(Namespace::USER | Namespace::PID)
                    .run(|| (unsafe { libc::getpid() }, pdeathsig()))
                {
                    Ok((1, sig)) if sig == libc::SIGKILL => OK,
                    Ok((1, _)) => NOT_GUARDED,
                    Ok(_) => NOT_INIT,
                    Err(_) => RUN_FAILED,
                }
            };
            unsafe { libc::_exit(code) };
        }

        let mut status: libc::c_int = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(
            libc::WIFEXITED(status),
            "guard probe died on a signal: {status:#x}"
        );
        let code = libc::WEXITSTATUS(status);
        assert_eq!(
            code,
            OK,
            "{}",
            match code {
                NOT_LEADER => "forked child was not the group leader (test bug)",
                RUN_FAILED => "Container::run failed inside the probe",
                NOT_INIT => "child was not PID 1 of a new namespace",
                NOT_GUARDED =>
                    "namespace init had NO parent-death signal -- setup did not guard it",
                _ => "unexpected probe exit code",
            }
        );
    }

    /// The decision itself, on the arming branch, without needing the main
    /// thread. This is what keeps coverage if the end-to-end test above is
    /// skipped.
    #[test]
    fn guard_arms_for_a_group_leader_clone_into_a_pid_namespace() {
        let before = pdeathsig();
        let mut container = Container::new();
        container.unshare(Namespace::USER | Namespace::PID);
        let context = ChildContext {
            stdin: None,
            stdout: None,
            stderr: None,
            uid_map: &[],
            gid_map: &[],
            seccomp_fd: None,
            cloned_from_group_leader: true,
        };
        assert_eq!(container.guard_pid_namespace_init(&context), Ok(()));
        assert_eq!(pdeathsig(), libc::SIGKILL, "guard did not arm");
        // Leave the harness thread as we found it.
        unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, before) };
    }

    /// The guard is scoped to the case that needs it.
    ///
    /// A child that is NOT a namespace init keeps ordinary signal dispositions,
    /// so it is reapable already and arming would only add a way to lose it.
    #[test]
    fn ordinary_child_is_not_guarded() {
        assert_eq!(
            Container::new().run(|| {
                assert_ne!(unsafe { libc::getpid() }, 1);
                assert_eq!(pdeathsig(), 0, "guard applied outside a PID namespace");
            }),
            Ok(())
        );
    }

    /// Cloning from a non-leader thread must NOT arm the guard.
    ///
    /// `PR_SET_PDEATHSIG` fires when the cloning THREAD dies, not when the
    /// parent process does, so arming for a worker thread would kill a healthy
    /// container the moment that thread finished.
    ///
    /// This drives `guard_pid_namespace_init` directly rather than through
    /// `Container::run`. That is not a shortcut: `run` documents that it must be
    /// called before any other threads exist, and calling it from a spawned
    /// thread HANGS -- observed here, a test binary stuck for 241s before it was
    /// killed. Which is itself the point. The threading model of the caller is
    /// real, so the guard decides rather than assumes.
    #[test]
    fn non_leader_thread_does_not_arm_the_guard() {
        let observed = std::thread::spawn(Container::cloned_from_group_leader)
            .join()
            .expect("worker thread panicked");
        assert!(!observed, "a spawned thread reported itself as group leader");

        // The decision, exercised on this thread so the prctl side effect (if
        // the guard wrongly fired) would be visible.
        let before = pdeathsig();
        let mut container = Container::new();
        container.unshare(Namespace::USER | Namespace::PID);
        let context = ChildContext {
            stdin: None,
            stdout: None,
            stderr: None,
            uid_map: &[],
            gid_map: &[],
            seccomp_fd: None,
            cloned_from_group_leader: false,
        };
        assert_eq!(container.guard_pid_namespace_init(&context), Ok(()));
        assert_eq!(
            pdeathsig(),
            before,
            "guard armed despite being cloned from a non-leader thread"
        );
    }

    #[test]
    fn return_value() {
        assert_eq!(Container::new().run(|| 42), Ok(42));

        assert_eq!(
            Container::new().run(|| String::from("foobar")),
            Ok("foobar".into())
        );
    }

    #[test]
    fn huge_return_value() {
        assert_eq!(
            Container::new().run(|| {
                // Need something larger than /proc/sys/fs/pipe-max-size, which
                // is typically 1MB.
                vec![42; 10 * 1024 * 1024 /* 10 MB */]
            }),
            Ok(vec![42; 10 * 1024 * 1024])
        );
    }

    #[test]
    pub fn bind_to_low_port() {
        use std::net::Ipv4Addr;
        use std::net::SocketAddrV4;
        use std::net::TcpListener;

        let addr = Container::new()
            .map_root()
            .local_networking_only()
            .run(|| {
                let listener = TcpListener::bind("127.0.0.1:80").unwrap();
                listener.local_addr().unwrap()
            })
            .unwrap();

        assert_eq!(
            addr,
            SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 80).into()
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    pub fn pin_affinity_to_all_cores() -> Result<(), Error> {
        use std::collections::HashMap;

        use raw_cpuid::CpuId;

        let cpus = num_cpus::get();
        println!("Total cpus {}", cpus);

        // Map the apic_id to the number of times we observed it:
        let mut results: HashMap<u8, usize> = HashMap::new();
        for core in 0..cpus {
            println!("  Launching guest with affinity set to {}", core);
            let mut container = Container::new();
            container.affinity(core);
            let which_core = container
                .run(|| {
                    let cpuid = CpuId::new();
                    cpuid
                        .get_feature_info()
                        .expect("cpuid failed")
                        .initial_local_apic_id()
                })
                .unwrap();
            println!("    Guest sees its on APIC id {}", which_core);
            *results.entry(which_core).or_default() += 1;
        }

        println!("Final table size {:?}", results.len());
        assert_eq!(results.values().fold(0, |n, v| std::cmp::max(n, *v)), 1);
        Ok(())
    }
}
