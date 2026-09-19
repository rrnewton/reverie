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
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
#[cfg(test)]
use std::sync::atomic::Ordering;

use nix::sched::CpuSet;
use nix::sched::sched_setaffinity;
use serde::Serialize;
use serde::de::DeserializeOwned;
use syscalls::Errno;

use super::clone::child_stack;
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
        self.setup_before_filter(context, pre_exec)?;
        self.setup_filter(context)
    }

    fn setup_before_filter(
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

        Ok(())
    }

    fn setup_filter(&self, context: &ChildContext) -> Result<(), Error> {
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
        };

        // Use a pipe for getting the result of the function out of the child
        // process.
        let (mut reader, writer) = pipe()?;

        let writer_fd = writer.as_raw_fd();

        // NOTE: Must use a dynamically allocated stack here. Programs expect to
        // have at least 2 MB of stack space and if we've already used up some
        // stack space before this is called we could overflow the stack.
        let mut stack = child_stack();

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
                let value: Result<T, Error> =
                    bincode::serde::decode_from_slice(&buf[0..n], bincode::config::legacy())
                        .unwrap()
                        .0;
                value.map_err(RunError::Spawn)
            }
            Err(err) => {
                // FIXME: Handle this error
                panic!("Got unexpected error: {}", err)
            }
        }
    }

    /// Runs child setup and a parent readiness callback before installing the
    /// unchanged seccomp filter and entering the child workload.
    ///
    /// `child_start` runs after namespace/filesystem setup, without creating a
    /// helper task. It returns child-local state and may transfer up to
    /// [`MAX_STARTUP_FDS`] owned descriptors through its context. `parent_start`
    /// runs in the original process, with the actual owned child and received
    /// descriptors, and must return only when its external resources are ready.
    /// Its returned owner stays in the parent. Only then may `run` consume the
    /// child state. Startup endpoint aliases close before seccomp is installed.
    ///
    /// The positive, representable `timeout` gives the entire protocol one
    /// monotonic I/O deadline, including time spent in callbacks. It does not
    /// preempt arbitrary callback code or destructors. Failure cancels and
    /// reaps the owned child; actual cleanup errors remain errors. Kernel waits
    /// for an uninterruptible child still require outer process supervision.
    ///
    /// Like [`Self::run`], call this before starting other threads. No signal
    /// handler or other thread may reap this child; SIGCHLD auto-reaping is
    /// rejected before clone. Callbacks must obey the existing fork-safety
    /// rules, close unrelated inherited descriptors, and not fork workers in
    /// the child. This API does not prove capture completion or guest teardown.
    ///
    /// Results are drained before wait, including large values. `run` returns
    /// a deferred cleanup value just as [`Self::run_with_deferred_drop`] does;
    /// the returned handle's [`DeferredContainerRun::finalize_with_status`]
    /// checks the real terminal status before yielding the value.
    pub fn run_with_startup<P, C, F, O, S, T, D>(
        &mut self,
        timeout: std::time::Duration,
        parent_start: P,
        mut child_start: C,
        mut run: F,
    ) -> Result<(O, DeferredContainerRun<T>), StartupRunError>
    where
        P: FnOnce(ParentStartContext<'_>) -> Result<O, StartupError>,
        C: FnMut(&mut ChildStartContext) -> Result<S, StartupError>,
        F: FnMut(S) -> (T, D),
        T: Serialize + DeserializeOwned,
    {
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or(StartupRunError::BeforeClone(StartupError::InvalidTimeout))?;
        let mut disposition: libc::sigaction = unsafe { std::mem::zeroed() };
        Errno::result(unsafe {
            libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut disposition)
        })
        .map_err(|error| StartupRunError::BeforeClone(error.into()))?;
        if disposition.sa_sigaction == libc::SIG_IGN
            || disposition.sa_flags & libc::SA_NOCLDWAIT != 0
        {
            return Err(StartupRunError::BeforeClone(StartupError::Io(
                Errno::ECHILD,
            )));
        }
        let (parent_socket, child_socket) =
            StartupSocket::pair(deadline).map_err(StartupRunError::BeforeClone)?;
        let uid_map = &make_id_map(&self.uid_map);
        let gid_map = &make_id_map(&self.gid_map);
        let context = ChildContext {
            stdin: None,
            stdout: None,
            stderr: None,
            uid_map,
            gid_map,
            seccomp_fd: None,
        };
        let (mut reader, writer) =
            pipe().map_err(|error| StartupRunError::BeforeClone(error.into()))?;
        let writer_fd = writer.as_raw_fd();
        let reader_fd = reader.as_raw_fd();
        let parent_fd = parent_socket.fd.as_raw_fd();
        let child_fd = child_socket.fd.as_raw_fd();
        let mut stack = child_stack();
        let clone_flags = self.namespace.bits() | libc::SIGCHLD;
        #[cfg(feature = "nightly")]
        let output_capture = std::io::set_output_capture(None);
        let result = clone_with_stack(
            || {
                // The outer Rust owners live only in the parent. This branch
                // owns its inherited child endpoint and result writer only.
                unsafe {
                    libc::close(parent_fd);
                    libc::close(reader_fd);
                }
                let socket = StartupSocket {
                    fd: Fd::new(child_fd),
                    deadline,
                };
                let startup = (|| {
                    self.setup_before_filter(&context, &mut [])
                        .map_err(StartupError::Setup)?;
                    let mut child_context = ChildStartContext {
                        deadline,
                        descriptors: StartupFds::default(),
                        failure: None,
                    };
                    let state = child_start(&mut child_context)?;
                    if let Some(error) = child_context.failure {
                        return Err(error);
                    }
                    socket.send(STARTUP_REQUEST, &child_context.descriptors, None)?;
                    drop(child_context);
                    socket.close_write()?;
                    socket.receive(Some(STARTUP_READY))?;
                    socket.receive(None)?; // Require completed final permission.

                    Ok(state)
                })();
                let state = match startup {
                    Ok(state) => state,
                    Err(error) => {
                        let _ = socket.send(STARTUP_FAILURE, &StartupFds::default(), Some(error));
                        drop(socket);
                        let mut writer = std::io::BufWriter::new(Fd::new(writer_fd));
                        bincode::serde::encode_into_std_write(
                            Err::<T, StartupError>(error),
                            &mut writer,
                            bincode::config::legacy(),
                        )
                        .expect("Failed to serialize startup refusal");
                        writer.flush().expect("Failed to flush startup refusal");
                        drop(writer);
                        return 1;
                    }
                };
                drop(socket);
                let (value, deferred) = match self.setup_filter(&context) {
                    Ok(()) => {
                        let (value, deferred) = run(state);
                        (Ok(value), Some(deferred))
                    }
                    Err(error) => (Err(StartupError::Setup(error)), None),
                };
                let mut writer = std::io::BufWriter::new(Fd::new(writer_fd));
                bincode::serde::encode_into_std_write(
                    &value,
                    &mut writer,
                    bincode::config::legacy(),
                )
                .expect("Failed to serialize return value");
                writer.flush().expect("Failed to flush return value");
                drop(writer);
                drop(deferred);
                0
            },
            clone_flags,
            &mut stack,
        );
        #[cfg(feature = "nightly")]
        std::io::set_output_capture(output_capture);
        let pid = result.map_err(|error| StartupRunError::BeforeClone(error.into()))?;
        let mut child = StartupChild {
            wait: Some(WaitGuard::new(pid)),
            pidfd: None,
        };
        drop(child_socket);
        drop(writer);
        child.pidfd = match Fd::pidfd_open(pid.as_raw(), 0) {
            Ok(fd) => Some(fd),
            Err(error) => return Err(child.fail(error.into())),
        };
        let descriptors =
            match parent_socket
                .receive(Some(STARTUP_REQUEST))
                .and_then(|descriptors| {
                    // Authorize nothing until the one request, including all ancillary
                    // rights and its true stream EOF, has been validated.
                    parent_socket.receive(None)?;
                    Ok(descriptors)
                }) {
                Ok(descriptors) => descriptors,
                Err(error) => return Err(child.fail(error)),
            };
        let owner = match parent_start(ParentStartContext {
            child: &child,
            deadline,
            descriptors,
        }) {
            Ok(owner) => owner,
            Err(error) => return Err(child.fail(error)),
        };
        let ready = (|| {
            parent_socket.send(STARTUP_READY, &StartupFds::default(), None)?;
            parent_socket.close_write()?;
            // No fallible startup validation remains after final permission.
            #[cfg(test)]
            if STARTUP_TEST_FAULT.with(|fault| fault.get())
                == StartupTestFault::ObservePermissionLate
            {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Ok::<_, StartupError>(())
        })();
        if let Err(error) = ready {
            // Reap before dropping the parent's resource owner.
            return Err(child.fail(error));
        }
        drop(parent_socket);
        let mut bytes = Vec::new();
        match reader.read_to_end(&mut bytes) {
            Ok(0) => return Err(child.fail(StartupError::MissingResult)),
            Ok(_) => (),
            Err(error) => {
                return Err(child.fail(StartupError::Io(Errno::new(
                    error.raw_os_error().unwrap_or(libc::EIO),
                ))));
            }
        }
        let value = match bincode::serde::decode_from_slice::<Result<T, StartupError>, _>(
            &bytes,
            bincode::config::legacy(),
        ) {
            Ok((Ok(value), used)) if used == bytes.len() => value,
            Ok((Err(error), used)) if used == bytes.len() => return Err(child.fail(error)),
            _ => return Err(child.fail(StartupError::Protocol)),
        };
        Ok((
            owner,
            DeferredContainerRun {
                value: Some(value),
                child: child.into_wait(),
            },
        ))
    }

    /// Runs a function in a new process, publishes its result, and only then
    /// drops a child-owned cleanup value.
    ///
    /// The returned handle owns the mandatory wait for the child. Callers may
    /// inspect the provisional value while doing independent work, but can
    /// only take ownership of it through
    /// [`DeferredContainerRun::finalize`], which rejects an unsuccessful child
    /// exit. Dropping the handle still reaps the child, but yields no value.
    ///
    /// A caller therefore cannot accidentally destructure the value away from
    /// the mandatory cleanup check:
    ///
    /// ```compile_fail
    /// use reverie_process::Container;
    /// let (value, cleanup) = Container::new()
    ///     .run_with_deferred_drop(|| (42, ()))
    ///     .unwrap();
    /// ```
    ///
    /// This has the same fork-safety requirements as [`Container::run`].
    pub fn run_with_deferred_drop<F, T, D>(
        &mut self,
        mut f: F,
    ) -> Result<DeferredContainerRun<T>, RunError>
    where
        F: FnMut() -> (T, D),
        T: Serialize + DeserializeOwned,
    {
        let clone_flags = self.namespace.bits() | libc::SIGCHLD;
        let uid_map = &make_id_map(&self.uid_map);
        let gid_map = &make_id_map(&self.gid_map);
        let context = ChildContext {
            stdin: None,
            stdout: None,
            stderr: None,
            uid_map,
            gid_map,
            seccomp_fd: None,
        };
        let (mut reader, writer) = pipe()?;
        let writer_fd = writer.as_raw_fd();
        let mut stack = child_stack();

        #[cfg(feature = "nightly")]
        let output_capture = std::io::set_output_capture(None);

        let result = clone_with_stack(
            || {
                let (value, deferred) = match self.setup(&context, &mut []) {
                    Ok(()) => {
                        let (value, deferred) = f();
                        (Ok(value), Some(deferred))
                    }
                    Err(error) => (Err(error), None),
                };
                let mut writer = std::io::BufWriter::new(Fd::new(writer_fd));
                bincode::serde::encode_into_std_write(
                    &value,
                    &mut writer,
                    bincode::config::legacy(),
                )
                .expect("Failed to serialize return value");
                writer.flush().expect("Failed to flush return value");
                drop(writer);
                drop(deferred);
                0
            },
            clone_flags,
            &mut stack,
        );

        #[cfg(feature = "nightly")]
        std::io::set_output_capture(output_capture);

        let child = WaitGuard::new(result?);
        drop(writer);

        let mut buf = Vec::new();
        match reader.read_to_end(&mut buf) {
            Ok(0) => Err(RunError::ExitStatus(child.wait()?)),
            Ok(n) => {
                let value: Result<T, Error> =
                    bincode::serde::decode_from_slice(&buf[0..n], bincode::config::legacy())
                        .unwrap()
                        .0;
                Ok(DeferredContainerRun {
                    value: Some(value.map_err(RunError::Spawn)?),
                    child,
                })
            }
            Err(error) => panic!("Got unexpected error: {error}"),
        }
    }
}

/// Maximum number of owned descriptors transferred by one container startup.
pub const MAX_STARTUP_FDS: usize = 8;

/// Failure of the finite container startup exchange.
#[derive(
    thiserror::Error,
    Debug,
    Copy,
    Clone,
    Eq,
    PartialEq,
    Serialize,
    serde::Deserialize
)]
pub enum StartupError {
    /// Container namespace/filesystem/filter setup failed.
    #[error("container setup failed: {0}")]
    Setup(Error),
    /// A startup syscall failed.
    #[error("startup syscall failed: {0}")]
    Io(Errno),
    /// The caller supplied a zero or unrepresentable startup timeout.
    #[error("startup timeout must be positive and representable")]
    InvalidTimeout,
    /// The single monotonic startup deadline elapsed.
    #[error("startup deadline elapsed")]
    TimedOut,
    /// A callback refused startup.
    #[error("startup callback refused")]
    Refused,
    /// The peer closed its endpoint before completing the exchange.
    #[error("startup peer closed prematurely")]
    PeerClosed,
    /// A frame, phase, descriptor count or result encoding was invalid.
    #[error("invalid startup or result protocol")]
    Protocol,
    /// The child exited before publishing its ordinary result.
    #[error("child exited before publishing its result")]
    MissingResult,
}

impl From<Errno> for StartupError {
    fn from(error: Errno) -> Self {
        Self::Io(error)
    }
}

/// A startup failure together with the actual owned-child cleanup outcome.
#[derive(thiserror::Error, Debug, Eq, PartialEq)]
pub enum StartupRunError {
    /// Failure before a child was successfully cloned.
    #[error("before clone: {0}")]
    BeforeClone(StartupError),
    /// Failure after clone; the child was reaped with this actual status.
    #[error("{cause}; child terminal status: {status:?}")]
    Child {
        /// Original startup or result failure.
        cause: StartupError,
        /// Actual status returned by waitpid, not an inferred success.
        status: ExitStatus,
    },
    /// Cleanup itself failed; no terminal status is claimed.
    #[error("{cause}; child cleanup failed: {errno}")]
    Cleanup {
        /// Original startup or result failure.
        cause: StartupError,
        /// Actual cancellation/wait error.
        errno: Errno,
    },
}

#[derive(Default)]
struct StartupFds {
    values: [Option<std::os::fd::OwnedFd>; MAX_STARTUP_FDS],
    len: usize,
}

impl StartupFds {
    fn push(&mut self, fd: std::os::fd::OwnedFd) -> Result<(), StartupError> {
        if self.len == MAX_STARTUP_FDS {
            return Err(StartupError::Protocol);
        }
        self.values[self.len] = Some(fd);
        self.len += 1;
        Ok(())
    }
}

/// Child-only setup context, constructed after namespace/filesystem setup.
///
/// It is neither clonable nor serializable. Transferred descriptors are sent
/// once with SCM_RIGHTS, then the child's originals are closed before seccomp.
/// This context creates no worker and carries no authority to emit capture data.
pub struct ChildStartContext {
    deadline: std::time::Instant,
    descriptors: StartupFds,
    failure: Option<StartupError>,
}

impl ChildStartContext {
    /// The same finite monotonic deadline used by both endpoints.
    pub fn deadline(&self) -> std::time::Instant {
        self.deadline
    }

    /// Transfers ownership of a descriptor to the parent startup callback.
    /// Exceeding [`MAX_STARTUP_FDS`] refuses startup and closes the supplied FD.
    pub fn transfer_fd(&mut self, fd: std::os::fd::OwnedFd) -> Result<(), StartupError> {
        let result = self.descriptors.push(fd);
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }
}

/// Parent-only startup context bound to this invocation's unreaped child.
///
/// The PID is a locator; the borrowed pidfd and privately held wait guard bind
/// the actual child generation. Constructors are private. No raw PID or FD
/// supplied by a caller can manufacture this context.
pub struct ParentStartContext<'a> {
    child: &'a StartupChild,
    deadline: std::time::Instant,
    descriptors: StartupFds,
}

impl ParentStartContext<'_> {
    /// The owned child PID in the parent's namespace; do not reap it separately.
    pub fn child_pid(&self) -> Pid {
        self.child.wait.as_ref().unwrap().0.unwrap()
    }

    /// Borrows the owned child generation's pidfd for identity-sensitive setup.
    pub fn child_pidfd(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::BorrowedFd;
        // SAFETY: The context borrows StartupChild, which owns this descriptor.
        unsafe { BorrowedFd::borrow_raw(self.child.pidfd.as_ref().unwrap().as_raw_fd()) }
    }

    /// The same finite monotonic deadline used by both endpoints.
    pub fn deadline(&self) -> std::time::Instant {
        self.deadline
    }

    /// Number of descriptors supplied by the child (at most [`MAX_STARTUP_FDS`]).
    pub fn descriptor_count(&self) -> usize {
        self.descriptors.len
    }

    /// Takes one received descriptor exactly once. Untaken descriptors close
    /// when this context is dropped. An out-of-range index returns `None`.
    pub fn take_fd(&mut self, index: usize) -> Option<std::os::fd::OwnedFd> {
        self.descriptors.values.get_mut(index)?.take()
    }
}

// A fixed, private readiness exchange, not an evidence/event transport. The
// sole pair is created before clone; each branch closes the other endpoint.
const STARTUP_REQUEST: u8 = 1;
const STARTUP_READY: u8 = 2;
const STARTUP_FAILURE: u8 = 4;
const STARTUP_FRAME_SIZE: usize = 64;

struct StartupSocket {
    fd: Fd,
    deadline: std::time::Instant,
}

impl StartupSocket {
    fn pair(deadline: std::time::Instant) -> Result<(Self, Self), StartupError> {
        let mut pair = [-1; 2];
        Errno::result(unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
                pair.as_mut_ptr(),
            )
        })?;
        Ok((
            Self {
                fd: Fd::new(pair[0]),
                deadline,
            },
            Self {
                fd: Fd::new(pair[1]),
                deadline,
            },
        ))
    }

    fn poll(&self, events: libc::c_short) -> Result<(), StartupError> {
        loop {
            let remaining = self
                .deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|value| !value.is_zero())
                .ok_or(StartupError::TimedOut)?;
            let millis = remaining
                .as_millis()
                .saturating_add(1)
                .min(i32::MAX as u128) as i32;
            let mut fd = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events,
                revents: 0,
            };
            match Errno::result(unsafe { libc::poll(&mut fd, 1, millis) }) {
                Ok(0) | Err(Errno::EINTR) => continue,
                Ok(_) if fd.revents & libc::POLLNVAL != 0 => {
                    return Err(StartupError::Io(Errno::EBADF));
                }
                Ok(_) => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn send_bytes(&self, bytes: &[u8], fds: &StartupFds) -> Result<(), StartupError> {
        // The first successful send carries rights exactly once, even when its
        // data is partial. Subsequent sends carry only remaining frame bytes.
        let mut ancillary = [0usize; 16];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        if fds.len != 0 {
            message.msg_control = ancillary.as_mut_ptr().cast();
            message.msg_controllen =
                unsafe { libc::CMSG_SPACE((fds.len * std::mem::size_of::<i32>()) as u32) as usize };
            assert!(message.msg_controllen <= std::mem::size_of_val(&ancillary));
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&message);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len =
                    libc::CMSG_LEN((fds.len * std::mem::size_of::<i32>()) as u32) as usize;
                let data = libc::CMSG_DATA(header).cast::<i32>();
                for index in 0..fds.len {
                    data.add(index)
                        .write(fds.values[index].as_ref().unwrap().as_raw_fd());
                }
            }
        }
        let mut offset = 0;
        while offset < bytes.len() {
            self.poll(libc::POLLOUT)?;
            let mut iov = libc::iovec {
                iov_base: bytes[offset..].as_ptr().cast_mut().cast(),
                iov_len: bytes.len() - offset,
            };
            #[cfg(test)]
            if STARTUP_TEST_FAULT.with(|fault| fault.get()) == StartupTestFault::Fragmented {
                iov.iov_len = 1;
            }
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            match Errno::result(unsafe {
                libc::sendmsg(self.fd.as_raw_fd(), &message, libc::MSG_NOSIGNAL)
            }) {
                Ok(0) => return Err(StartupError::Protocol),
                Ok(size) => {
                    offset += size as usize;
                    message.msg_control = std::ptr::null_mut();
                    message.msg_controllen = 0;
                }
                Err(Errno::EINTR | Errno::EAGAIN) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn send(
        &self,
        phase: u8,
        fds: &StartupFds,
        failure: Option<StartupError>,
    ) -> Result<(), StartupError> {
        let mut frame = [0u8; STARTUP_FRAME_SIZE];
        frame[..4].copy_from_slice(b"RVS1");
        frame[4] = phase;
        frame[5] = fds.len as u8;
        if let Some(error) = failure {
            frame[6] = bincode::serde::encode_into_slice(
                error,
                &mut frame[8..],
                bincode::config::legacy(),
            )
            .map_err(|_| StartupError::Protocol)? as u8;
        }
        #[cfg(test)]
        if let Some(result) = self.inject_test_fault(phase, &frame, fds) {
            return result;
        }
        self.send_bytes(&frame, fds)
    }

    fn receive(&self, phase: Option<u8>) -> Result<StartupFds, StartupError> {
        use std::os::fd::FromRawFd;
        let mut frame = [0u8; STARTUP_FRAME_SIZE];
        let mut offset = 0;
        let mut fds = StartupFds::default();
        loop {
            self.poll(libc::POLLIN)?;
            let mut ancillary = [0usize; 16];
            let capacity = if phase.is_none() {
                1
            } else {
                frame.len() - offset
            };
            let mut iov = libc::iovec {
                iov_base: frame[offset..].as_mut_ptr().cast(),
                iov_len: capacity,
            };
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            message.msg_control = ancillary.as_mut_ptr().cast();
            message.msg_controllen = unsafe {
                libc::CMSG_SPACE((MAX_STARTUP_FDS * std::mem::size_of::<i32>()) as u32) as usize
            };
            assert!(message.msg_controllen <= std::mem::size_of_val(&ancillary));
            let size = match Errno::result(unsafe {
                libc::recvmsg(self.fd.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC)
            }) {
                Ok(size) => size as usize,
                Err(Errno::EINTR | Errno::EAGAIN) => continue,
                Err(error) => return Err(error.into()),
            };
            let mut malformed = message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0;
            let previous_fds = fds.len;
            // Own every received FD before checking bytes/phase, including
            // trailing data and EOF. Linux closes rights lost to MSG_CTRUNC.
            unsafe {
                let mut header = libc::CMSG_FIRSTHDR(&message);
                while !header.is_null() {
                    if (*header).cmsg_level == libc::SOL_SOCKET
                        && (*header).cmsg_type == libc::SCM_RIGHTS
                        && (*header).cmsg_len >= libc::CMSG_LEN(0) as usize
                    {
                        let bytes = (*header).cmsg_len - libc::CMSG_LEN(0) as usize;
                        malformed |= !bytes.is_multiple_of(std::mem::size_of::<i32>());
                        let data = libc::CMSG_DATA(header).cast::<i32>();
                        for index in 0..bytes / std::mem::size_of::<i32>() {
                            let fd = std::os::fd::OwnedFd::from_raw_fd(data.add(index).read());
                            if fds.push(fd).is_err() {
                                malformed = true;
                            }
                        }
                    } else {
                        malformed = true;
                    }
                    header = libc::CMSG_NXTHDR(&message, header);
                }
            }
            if malformed
                || (fds.len != previous_fds && (offset != 0 || phase != Some(STARTUP_REQUEST)))
            {
                return Err(StartupError::Protocol);
            }
            if size == 0 {
                // SOCK_STREAM has no zero-length data messages. Unlike
                // SEQPACKET, this is genuine EOF, never an empty packet.
                return if phase.is_none() && fds.len == 0 {
                    Ok(fds)
                } else if offset == 0 && fds.len == 0 {
                    Err(StartupError::PeerClosed)
                } else {
                    Err(StartupError::Protocol)
                };
            }
            if phase.is_none() {
                return Err(StartupError::Protocol);
            }
            offset += size;
            if offset < frame.len() {
                continue;
            }
            if &frame[..4] != b"RVS1" || frame[5] as usize != fds.len || frame[7] != 0 {
                return Err(StartupError::Protocol);
            }
            if frame[4] == STARTUP_FAILURE
                && fds.len == 0
                && frame[6] != 0
                && frame[6] as usize <= frame.len() - 8
            {
                let end = 8 + frame[6] as usize;
                let (error, used) = bincode::serde::decode_from_slice::<StartupError, _>(
                    &frame[8..end],
                    bincode::config::legacy(),
                )
                .map_err(|_| StartupError::Protocol)?;
                if used != end - 8 || frame[end..].iter().any(|byte| *byte != 0) {
                    return Err(StartupError::Protocol);
                }
                return Err(error);
            }
            if phase != Some(frame[4])
                || frame[6..].iter().any(|byte| *byte != 0)
                || (frame[4] != STARTUP_REQUEST && fds.len != 0)
            {
                return Err(StartupError::Protocol);
            }
            return Ok(fds);
        }
    }

    fn close_write(&self) -> Result<(), StartupError> {
        Errno::result(unsafe { libc::shutdown(self.fd.as_raw_fd(), libc::SHUT_WR) })?;
        Ok(())
    }
}

// Test-only wire corruption. It substitutes bytes at the actual private send
// boundary; it never bypasses the production decoder, readiness or workload
// gate. Thread-local state keeps unrelated container tests independent.
#[cfg(test)]
#[derive(Copy, Clone, Eq, PartialEq)]
enum StartupTestFault {
    None,
    Fragmented,
    RequestEmptyTrailing,
    RequestDuplicate,
    RequestMalformed,
    RequestTrailingRights,
    PermissionEmptyTrailing,
    PermissionDuplicate,
    PermissionMalformed,
    PermissionTrailingRights,
    ObservePermissionLate,
}

#[cfg(test)]
std::thread_local! {
    static STARTUP_TEST_FAULT: std::cell::Cell<StartupTestFault> = const { std::cell::Cell::new(StartupTestFault::None) };
}

#[cfg(test)]
impl StartupSocket {
    fn inject_test_fault(
        &self,
        phase: u8,
        frame: &[u8],
        fds: &StartupFds,
    ) -> Option<Result<(), StartupError>> {
        use StartupTestFault::*;
        let fault = STARTUP_TEST_FAULT.with(|value| value.get());
        let request = phase == STARTUP_REQUEST;
        let permission = phase == STARTUP_READY;
        if (request && fault == RequestEmptyTrailing)
            || (permission && fault == PermissionEmptyTrailing)
        {
            return Some({
                assert_eq!(
                    unsafe {
                        libc::send(self.fd.as_raw_fd(), std::ptr::null(), 0, libc::MSG_NOSIGNAL)
                    },
                    0
                );
                self.send_bytes(b"X", &StartupFds::default())
            });
        }
        if (request && fault == RequestMalformed) || (permission && fault == PermissionMalformed) {
            let mut bad = frame.to_vec();
            bad[0] ^= 1;
            return Some(self.send_bytes(&bad, fds));
        }
        if (request && fault == RequestDuplicate) || (permission && fault == PermissionDuplicate) {
            return Some(
                self.send_bytes(frame, fds)
                    .and_then(|()| self.send_bytes(frame, fds)),
            );
        }
        if (request && fault == RequestTrailingRights)
            || (permission && fault == PermissionTrailingRights)
        {
            return Some((|| {
                self.send_bytes(frame, fds)?;
                let mut trailing = StartupFds::default();
                trailing.push(std::fs::File::open("/dev/null").unwrap().into())?;
                self.send_bytes(b"X", &trailing)
            })());
        }
        Option::None
    }
}

// Owns cancellation only during startup/result acquisition. Old WaitGuard and
// deferred-result drop semantics remain unchanged after successful acquisition.
struct StartupChild {
    wait: Option<WaitGuard>,
    pidfd: Option<Fd>,
}

impl StartupChild {
    fn cancel(&mut self) -> Result<ExitStatus, Errno> {
        let wait = self.wait.as_ref().unwrap();
        let result = match &self.pidfd {
            Some(fd) => Errno::result(unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            })
            .map(|_| ()),
            // Before pidfd_open completes, this is still the unreaped child
            // returned by clone. Callers must not install an auto-reaper or
            // wait for this invocation's child from another thread/handler.
            None => Errno::result(unsafe { libc::kill(wait.0.unwrap().as_raw(), libc::SIGKILL) })
                .map(|_| ()),
        };
        match result {
            Ok(()) | Err(Errno::ESRCH) => (),
            Err(error) => return Err(error),
        }
        self.wait.take().unwrap().wait()
    }

    fn fail(&mut self, cause: StartupError) -> StartupRunError {
        match self.cancel() {
            Ok(status) => StartupRunError::Child { cause, status },
            Err(errno) => StartupRunError::Cleanup { cause, errno },
        }
    }

    fn into_wait(mut self) -> WaitGuard {
        self.wait.take().unwrap()
    }
}

impl Drop for StartupChild {
    fn drop(&mut self) {
        if self.wait.is_some() {
            let _ = self.cancel();
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
        self.wait_inner()
    }

    fn wait_inner(&mut self) -> Result<ExitStatus, Errno> {
        let pid = self.0.expect("child wait guard has already been consumed");
        #[cfg(test)]
        let instrumented_wait = WAITPID_TEST_PID.load(Ordering::Acquire) == pid.as_raw();
        let mut status = 0;
        loop {
            #[cfg(test)]
            if instrumented_wait {
                WAITPID_ENTERED.store(true, Ordering::Release);
            }
            match Errno::result(unsafe { libc::waitpid(pid.as_raw(), &mut status, 0) }) {
                Ok(ret) => {
                    assert_eq!(ret, pid.as_raw());
                    self.0 = None;
                    return Ok(ExitStatus::from_raw(status));
                }
                Err(Errno::EINTR) => {
                    #[cfg(test)]
                    {
                        if instrumented_wait {
                            WAITPID_INTERRUPTED.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Err(Errno::ECHILD) => {
                    self.0 = None;
                    return Err(Errno::ECHILD);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

/// A provisional container result whose successful value remains owned by its
/// mandatory cleanup check.
#[must_use = "a deferred container result must be finalized before its value can be returned"]
pub struct DeferredContainerRun<T> {
    value: Option<T>,
    child: WaitGuard,
}

impl<T> DeferredContainerRun<T> {
    /// Borrows the value while the child finishes cleanup.
    pub fn provisional(&self) -> &T {
        self.value.as_ref().expect("provisional value is present")
    }

    /// Waits for cleanup and returns the value only after a successful exit.
    pub fn finalize(self) -> Result<T, RunError> {
        self.finalize_with_status().map(|(value, _status)| value)
    }

    /// Waits for cleanup and returns the value and actual successful child
    /// status. A nonzero/signal status remains [`RunError::ExitStatus`]. This
    /// observes this container child only, not any guest's separate teardown.
    pub fn finalize_with_status(mut self) -> Result<(T, ExitStatus), RunError> {
        let status = self.child.wait()?;
        if !status.success() {
            return Err(RunError::ExitStatus(status));
        }
        Ok((
            self.value.take().expect("provisional value is present"),
            status,
        ))
    }
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        if self.0.is_some() {
            let _ = self.wait_inner();
        }
    }
}

#[cfg(test)]
static WAITPID_TEST_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
#[cfg(test)]
static WAITPID_ENTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static WAITPID_INTERRUPTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use std::time::Instant;

    use nix::sys::signal::SaFlags;
    use nix::sys::signal::SigAction;
    use nix::sys::signal::SigHandler;
    use nix::sys::signal::SigSet;
    use nix::sys::signal::Signal;
    use nix::sys::signal::sigaction;

    use super::*;

    struct StartupFaultGuard;

    impl StartupFaultGuard {
        fn install(fault: StartupTestFault) -> Self {
            STARTUP_TEST_FAULT.with(|value| {
                assert!(value.get() == StartupTestFault::None);
                value.set(fault);
            });
            Self
        }
    }

    impl Drop for StartupFaultGuard {
        fn drop(&mut self) {
            STARTUP_TEST_FAULT.with(|value| value.set(StartupTestFault::None));
        }
    }

    #[test]
    fn startup_corrupt_request_or_permission_never_runs_workload() {
        use StartupTestFault::*;
        for fault in [
            RequestEmptyTrailing,
            RequestDuplicate,
            RequestMalformed,
            RequestTrailingRights,
            PermissionEmptyTrailing,
            PermissionDuplicate,
            PermissionMalformed,
            PermissionTrailingRights,
        ] {
            let _fault = StartupFaultGuard::install(fault);
            let (mapping, shared) = new_shared_drop_state();
            let mut parent_called = false;
            let result = Container::new().run_with_startup(
                Duration::from_secs(2),
                |_| {
                    parent_called = true;
                    Ok(())
                },
                |_| Ok(()),
                |()| {
                    unsafe { &*shared }.started.store(true, Ordering::Release);
                    ((), ())
                },
            );
            assert!(matches!(
                result,
                Err(StartupRunError::Child {
                    cause: StartupError::Protocol,
                    ..
                })
            ));
            assert_eq!(
                parent_called,
                matches!(
                    fault,
                    PermissionEmptyTrailing
                        | PermissionDuplicate
                        | PermissionMalformed
                        | PermissionTrailingRights
                )
            );
            assert!(!unsafe { &*shared }.started.load(Ordering::Acquire));
            unsafe { unmap_shared_drop_state(mapping, shared) };
        }
    }

    #[test]
    fn startup_fragmented_request_transfers_each_right_exactly_once() {
        let _fault = StartupFaultGuard::install(StartupTestFault::Fragmented);
        let (pid, handle) = Container::new()
            .run_with_startup(
                Duration::from_secs(2),
                |mut context| {
                    assert_eq!(context.descriptor_count(), MAX_STARTUP_FDS);
                    for index in 0..MAX_STARTUP_FDS {
                        assert!(context.take_fd(index).is_some());
                    }
                    Ok(context.child_pid())
                },
                |context| {
                    for _ in 0..MAX_STARTUP_FDS {
                        context.transfer_fd(std::fs::File::open("/dev/null").unwrap().into())?;
                    }
                    Ok(())
                },
                |()| (42, ()),
            )
            .unwrap();
        assert_eq!(
            handle.finalize_with_status(),
            Ok((42, ExitStatus::Exited(0)))
        );
        assert_reaped(pid);
    }

    #[test]
    fn startup_final_permission_has_no_later_parent_deadline_validation() {
        // Deliberately delay the parent after final permission is sent and its
        // write side closed. This models scheduling after release, without
        // bypassing any protocol checks. The child's genuine result still must
        // be drained and its actual terminal status checked.
        let _fault = StartupFaultGuard::install(StartupTestFault::ObservePermissionLate);
        let (pid, handle) = Container::new()
            .run_with_startup(
                Duration::from_millis(100),
                |context| Ok(context.child_pid()),
                |_| Ok(()),
                |()| (42, ()),
            )
            .unwrap();
        assert_eq!(
            handle.finalize_with_status(),
            Ok((42, ExitStatus::Exited(0)))
        );
        assert_reaped(pid);
    }

    #[test]
    fn startup_ignored_descriptor_overflow_still_refuses_workload() {
        let (mapping, shared) = new_shared_drop_state();
        let result = Container::new().run_with_startup(
            Duration::from_secs(2),
            |_| Ok(()),
            |context| {
                for _ in 0..=MAX_STARTUP_FDS {
                    let _ = context.transfer_fd(std::fs::File::open("/dev/null").unwrap().into());
                }
                Ok(())
            },
            |()| {
                unsafe { &*shared }.started.store(true, Ordering::Release);
                ((), ())
            },
        );
        assert!(matches!(
            result,
            Err(StartupRunError::Child {
                cause: StartupError::Protocol,
                ..
            })
        ));
        assert!(!unsafe { &*shared }.started.load(Ordering::Acquire));
        unsafe { unmap_shared_drop_state(mapping, shared) };
    }

    fn assert_reaped(pid: Pid) {
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid.as_raw(), &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(Errno::last(), Errno::ECHILD);
    }

    #[test]
    fn startup_binds_parent_child_and_transfers_owned_descriptor_once() {
        use std::os::fd::AsFd;
        let parent = Pid::this();
        let (mapping, shared) = new_shared_drop_state();
        let (owner, handle) = Container::new()
            .run_with_startup(
                Duration::from_secs(2),
                |mut context| {
                    assert_eq!(Pid::this(), parent);
                    assert_ne!(context.child_pid(), parent);
                    assert!(context.child_pidfd().as_raw_fd() >= 0);
                    assert_eq!(context.descriptor_count(), 1);
                    let fd = context.take_fd(0).unwrap();
                    assert!(context.take_fd(0).is_none());
                    assert!(context.take_fd(MAX_STARTUP_FDS).is_none());
                    assert_ne!(
                        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                        0
                    );
                    let mut file = std::fs::File::from(fd);
                    let mut contents = String::new();
                    file.read_to_string(&mut contents).unwrap();
                    assert!(contents.starts_with(&format!("{} ", context.child_pid())));
                    unsafe { &*shared }.release.store(true, Ordering::Release);
                    Ok(context.child_pid())
                },
                |context| {
                    let file = std::fs::File::open("/proc/self/stat").unwrap();
                    context.transfer_fd(file.as_fd().try_clone_to_owned().unwrap())?;
                    Ok(Pid::this())
                },
                |pid| {
                    assert!(unsafe { &*shared }.release.load(Ordering::Acquire));
                    assert_eq!(pid, Pid::this());
                    assert_eq!(Pid::parent(), parent);
                    (pid, ())
                },
            )
            .unwrap();
        assert_eq!(
            handle.finalize_with_status(),
            Ok((owner, ExitStatus::Exited(0)))
        );
        assert_reaped(owner);
        unsafe { unmap_shared_drop_state(mapping, shared) };
    }

    fn namespace_population_probe() -> (i32, i32, i32) {
        let root = Pid::this().as_raw();
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            unsafe { libc::_exit(0) }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert_eq!(ExitStatus::from_raw(status), ExitStatus::Exited(0));
        let thread = std::thread::spawn(|| unsafe { libc::syscall(libc::SYS_gettid) as i32 })
            .join()
            .unwrap();
        (root, child, thread)
    }

    #[test]
    fn startup_does_not_allocate_a_child_namespace_helper_pid() {
        let baseline = Container::new()
            .unshare(Namespace::USER | Namespace::PID)
            .run(namespace_population_probe)
            .unwrap();
        assert_eq!(baseline, (1, 2, 3));
        let parent = Pid::this();
        let ((), handle) = Container::new()
            .unshare(Namespace::USER | Namespace::PID)
            .run_with_startup(
                Duration::from_secs(2),
                |context| {
                    assert_eq!(Pid::this(), parent);
                    assert_ne!(context.child_pid(), Pid::from_raw(1));
                    Ok(())
                },
                |_| {
                    assert_eq!(Pid::this().as_raw(), 1);
                    Ok(())
                },
                |()| (namespace_population_probe(), ()),
            )
            .unwrap();
        assert_eq!(
            handle.finalize_with_status(),
            Ok((baseline, ExitStatus::Exited(0)))
        );
        // Opposing control: a genuine in-child helper consumes a PID and must
        // change this exact observation. Do not normalize namespace identities.
        let wrong = Container::new()
            .unshare(Namespace::USER | Namespace::PID)
            .run(|| {
                std::thread::spawn(|| ()).join().unwrap();
                namespace_population_probe()
            })
            .unwrap();
        assert_eq!(wrong, (1, 3, 4));
        assert_ne!(wrong, baseline);
    }

    #[test]
    fn startup_precedes_seccomp_without_widening_the_filter() {
        use syscalls::Sysno;

        use super::seccomp::Action;
        use super::seccomp::FilterBuilder;
        let filter = || {
            FilterBuilder::new()
                .default_action(Action::Allow)
                .syscalls([
                    (Sysno::sendmsg, Action::Errno(Errno::EPERM)),
                    (Sysno::recvmsg, Action::Errno(Errno::EPERM)),
                    (Sysno::poll, Action::Errno(Errno::EPERM)),
                    (Sysno::getppid, Action::Errno(Errno::EPERM)),
                ])
                .build()
        };
        let denied = || Errno::result(unsafe { libc::syscall(libc::SYS_getppid) });
        assert_eq!(
            Container::new().seccomp(filter()).run(denied),
            Ok(Err(Errno::EPERM))
        );
        let ((), handle) = Container::new()
            .seccomp(filter())
            .run_with_startup(
                Duration::from_secs(2),
                |_| Ok(()),
                |_| Ok(()),
                |()| (denied(), ()),
            )
            .unwrap();
        assert_eq!(
            handle.finalize_with_status(),
            Ok((Err(Errno::EPERM), ExitStatus::Exited(0)))
        );
    }

    #[test]
    fn startup_drains_large_result_before_deferred_cleanup_and_actual_wait() {
        let (mapping, shared) = new_shared_drop_state();
        let (pid, handle) = Container::new()
            .run_with_startup(
                Duration::from_secs(2),
                |context| Ok(context.child_pid()),
                |_| Ok(()),
                |()| (vec![42u8; 10 * 1024 * 1024], BlockingDrop { shared }),
            )
            .unwrap();
        assert_eq!(handle.provisional(), &vec![42u8; 10 * 1024 * 1024]);
        let shared_ref = unsafe { &*shared };
        while !shared_ref.started.load(Ordering::Acquire) {
            unsafe { libc::sched_yield() };
        }
        assert!(!shared_ref.finished.load(Ordering::Acquire));
        shared_ref.release.store(true, Ordering::Release);
        let (value, status) = handle.finalize_with_status().unwrap();
        assert_eq!(value, vec![42u8; 10 * 1024 * 1024]);
        assert_eq!(status, ExitStatus::Exited(0));
        assert!(shared_ref.finished.load(Ordering::Acquire));
        assert_reaped(pid);
        unsafe { unmap_shared_drop_state(mapping, shared) };
    }

    #[test]
    fn startup_keeps_cleanup_failure_and_drop_reap_semantics() {
        let (pid, handle) = Container::new()
            .run_with_startup(
                Duration::from_secs(2),
                |context| Ok(context.child_pid()),
                |_| Ok(()),
                |()| (42, ExitDuringDrop(71)),
            )
            .unwrap();
        assert_eq!(handle.provisional(), &42);
        assert_eq!(
            handle.finalize_with_status(),
            Err(RunError::ExitStatus(ExitStatus::Exited(71)))
        );
        assert_reaped(pid);
        let (pid, handle) = Container::new()
            .run_with_startup(
                Duration::from_secs(2),
                |context| Ok(context.child_pid()),
                |_| Ok(()),
                |()| (42, ()),
            )
            .unwrap();
        drop(handle);
        assert_reaped(pid);
    }

    #[test]
    fn startup_parent_refusal_cancels_owned_child_without_running_workload() {
        let (mapping, shared) = new_shared_drop_state();
        let mut pid = None;
        let result = Container::new().run_with_startup(
            Duration::from_secs(2),
            |context| {
                pid = Some(context.child_pid());
                Err::<(), _>(StartupError::Refused)
            },
            |_| Ok(()),
            |()| {
                unsafe { &*shared }.started.store(true, Ordering::Release);
                ((), ())
            },
        );
        assert!(matches!(
            result,
            Err(StartupRunError::Child {
                cause: StartupError::Refused,
                status: ExitStatus::Signaled(Signal::SIGKILL, false)
            })
        ));
        assert!(!unsafe { &*shared }.started.load(Ordering::Acquire));
        assert_reaped(pid.unwrap());
        unsafe { unmap_shared_drop_state(mapping, shared) };
    }

    #[test]
    fn startup_child_setup_and_callback_refusals_are_not_readiness() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("absent");
        let result = Container::new().current_dir(missing).run_with_startup(
            Duration::from_secs(2),
            |_| -> Result<(), StartupError> {
                panic!("parent hook must not see failed child setup")
            },
            |_| -> Result<(), StartupError> { panic!("child hook must not see failed setup") },
            |()| -> ((), ()) { panic!("workload must not run") },
        );
        assert!(matches!(
            result,
            Err(StartupRunError::Child {
                cause: StartupError::Setup(Error { .. }),
                ..
            })
        ));
        if let Err(StartupRunError::Child {
            cause: StartupError::Setup(error),
            ..
        }) = result
        {
            assert_eq!(error, Error::new(Errno::ENOENT, Context::Chdir));
        }
        let result = Container::new().run_with_startup(
            Duration::from_secs(2),
            |_| -> Result<(), StartupError> {
                panic!("parent hook must not see refused child setup")
            },
            |_| Err::<(), _>(StartupError::Refused),
            |()| ((), ()),
        );
        assert!(matches!(
            result,
            Err(StartupRunError::Child {
                cause: StartupError::Refused,
                ..
            })
        ));
    }

    #[test]
    fn startup_premature_child_exit_retains_actual_status() {
        let result = Container::new().run_with_startup(
            Duration::from_secs(2),
            |_| Ok(()),
            |_| -> Result<(), StartupError> { unsafe { libc::_exit(73) } },
            |()| ((), ()),
        );
        assert!(matches!(
            result,
            Err(StartupRunError::Child {
                cause: StartupError::PeerClosed,
                status: ExitStatus::Exited(73)
            })
        ));
    }

    #[test]
    fn startup_deadline_kills_a_child_stuck_before_readiness() {
        let result = Container::new().run_with_startup(
            Duration::from_millis(100),
            |_| Ok(()),
            |_| -> Result<(), StartupError> {
                loop {
                    unsafe { libc::pause() };
                }
            },
            |()| ((), ()),
        );
        assert!(matches!(
            result,
            Err(StartupRunError::Child {
                cause: StartupError::TimedOut,
                status: ExitStatus::Signaled(Signal::SIGKILL, false)
            })
        ));
    }

    #[test]
    fn startup_late_parent_callback_cannot_release_workload() {
        let (mapping, shared) = new_shared_drop_state();
        let mut pid = None;
        let result = Container::new().run_with_startup(
            Duration::from_millis(100),
            |context| {
                pid = Some(context.child_pid());
                std::thread::sleep(Duration::from_millis(200));
                Ok(())
            },
            |_| Ok(()),
            |()| {
                unsafe { &*shared }.started.store(true, Ordering::Release);
                ((), ())
            },
        );
        assert!(matches!(
            result,
            Err(StartupRunError::Child {
                cause: StartupError::TimedOut,
                ..
            })
        ));
        assert!(!unsafe { &*shared }.started.load(Ordering::Acquire));
        assert_reaped(pid.unwrap());
        unsafe { unmap_shared_drop_state(mapping, shared) };
    }

    #[test]
    fn startup_invalid_timeout_refuses_before_clone_or_callbacks() {
        for timeout in [Duration::ZERO, Duration::MAX] {
            let result = Container::new().run_with_startup(
                timeout,
                |_| -> Result<(), StartupError> { panic!("no parent callback") },
                |_| -> Result<(), StartupError> { panic!("no child callback") },
                |()| ((), ()),
            );
            assert!(matches!(
                result,
                Err(StartupRunError::BeforeClone(StartupError::InvalidTimeout))
            ));
        }
    }

    #[test]
    fn startup_protocol_rejects_malformed_wrong_phase_and_trailing_frames() {
        let good = {
            let mut frame = [0u8; STARTUP_FRAME_SIZE];
            frame[..4].copy_from_slice(b"RVS1");
            frame[4] = STARTUP_READY;
            frame
        };
        let mut cases = vec![
            vec![0],
            good[..STARTUP_FRAME_SIZE - 1].to_vec(),
            [good.as_slice(), &[0]].concat(),
        ];
        let mut wrong = good;
        wrong[4] = 3;
        cases.push(wrong.to_vec());
        let mut wrong = good;
        wrong[5] = 1;
        cases.push(wrong.to_vec());
        let mut wrong = good;
        wrong[7] = 1;
        cases.push(wrong.to_vec());
        for frame in cases {
            let (a, b) = StartupSocket::pair(Instant::now() + Duration::from_secs(2)).unwrap();
            assert_eq!(
                unsafe {
                    libc::send(
                        a.fd.as_raw_fd(),
                        frame.as_ptr().cast(),
                        frame.len(),
                        libc::MSG_NOSIGNAL,
                    )
                },
                frame.len() as isize
            );
            a.close_write().unwrap();
            let result = b.receive(Some(STARTUP_READY)).and_then(|_| b.receive(None));
            assert!(matches!(result, Err(StartupError::Protocol)));
        }
        let (a, b) = StartupSocket::pair(Instant::now() + Duration::from_secs(2)).unwrap();
        a.send(STARTUP_READY, &StartupFds::default(), None).unwrap();
        a.send(STARTUP_READY, &StartupFds::default(), None).unwrap();
        a.close_write().unwrap();
        b.receive(Some(STARTUP_READY)).unwrap();
        assert!(matches!(b.receive(None), Err(StartupError::Protocol)));
        let (a, b) = StartupSocket::pair(Instant::now() + Duration::from_secs(2)).unwrap();
        drop(a);
        assert!(matches!(
            b.receive(Some(STARTUP_READY)),
            Err(StartupError::PeerClosed)
        ));
    }

    #[test]
    fn startup_descriptor_cardinality_is_finite_and_refusal_closes_rights() {
        let mut context = ChildStartContext {
            deadline: Instant::now() + Duration::from_secs(2),
            descriptors: StartupFds::default(),
            failure: None,
        };
        for _ in 0..MAX_STARTUP_FDS {
            context
                .transfer_fd(std::fs::File::open("/dev/null").unwrap().into())
                .unwrap();
        }
        let extra: std::os::fd::OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
        let extra_number = extra.as_raw_fd();
        assert_eq!(context.transfer_fd(extra), Err(StartupError::Protocol));
        assert_eq!(unsafe { libc::fcntl(extra_number, libc::F_GETFD) }, -1);
        assert_eq!(Errno::last(), Errno::EBADF);
        let (a, b) = StartupSocket::pair(context.deadline).unwrap();
        a.send(STARTUP_REQUEST, &context.descriptors, None).unwrap();
        let received = b.receive(Some(STARTUP_REQUEST)).unwrap();
        assert_eq!(received.len, MAX_STARTUP_FDS);
        let numbers: Vec<_> = received
            .values
            .iter()
            .map(|fd| fd.as_ref().unwrap().as_raw_fd())
            .collect();
        drop(received);
        for fd in numbers {
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
            assert_eq!(Errno::last(), Errno::EBADF);
        }
    }

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

    #[test]
    fn return_value() {
        assert_eq!(Container::new().run(|| 42), Ok(42));

        assert_eq!(
            Container::new().run(|| String::from("foobar")),
            Ok("foobar".into())
        );
    }

    struct BlockingDrop {
        shared: *mut SharedDropState,
    }

    impl Drop for BlockingDrop {
        fn drop(&mut self) {
            let shared = unsafe { &*self.shared };
            shared.started.store(true, Ordering::Release);
            while !shared.release.load(Ordering::Acquire) {
                unsafe { libc::sched_yield() };
            }
            shared.finished.store(true, Ordering::Release);
        }
    }

    struct SharedDropState {
        started: AtomicBool,
        release: AtomicBool,
        finished: AtomicBool,
    }

    fn new_shared_drop_state() -> (*mut libc::c_void, *mut SharedDropState) {
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                std::mem::size_of::<SharedDropState>(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        let shared = mapping.cast::<SharedDropState>();
        unsafe {
            shared.write(SharedDropState {
                started: AtomicBool::new(false),
                release: AtomicBool::new(false),
                finished: AtomicBool::new(false),
            });
        }
        (mapping, shared)
    }

    unsafe fn unmap_shared_drop_state(mapping: *mut libc::c_void, shared: *mut SharedDropState) {
        unsafe {
            std::ptr::drop_in_place(shared);
            assert_eq!(
                libc::munmap(mapping, std::mem::size_of::<SharedDropState>()),
                0
            );
        }
    }

    #[test]
    fn deferred_drop_publishes_result_before_cleanup_completes() {
        let (mapping, shared) = new_shared_drop_state();

        let run = Container::new()
            .run_with_deferred_drop(|| (42, BlockingDrop { shared }))
            .unwrap();
        assert_eq!(run.provisional(), &42);

        let shared_ref = unsafe { &*shared };
        while !shared_ref.started.load(Ordering::Acquire) {
            unsafe { libc::sched_yield() };
        }
        shared_ref.release.store(true, Ordering::Release);
        assert_eq!(run.finalize(), Ok(42));
        assert!(shared_ref.finished.load(Ordering::Acquire));

        unsafe { unmap_shared_drop_state(mapping, shared) };
    }

    #[test]
    fn dropping_cleanup_handle_still_reaps_the_child() {
        let (mapping, shared) = new_shared_drop_state();
        let run = Container::new()
            .run_with_deferred_drop(|| ((), BlockingDrop { shared }))
            .unwrap();
        let shared_ref = unsafe { &*shared };
        while !shared_ref.started.load(Ordering::Acquire) {
            unsafe { libc::sched_yield() };
        }
        shared_ref.release.store(true, Ordering::Release);
        drop(run);
        assert!(shared_ref.finished.load(Ordering::Acquire));

        unsafe { unmap_shared_drop_state(mapping, shared) };
    }

    static WAITPID_SIGNAL_TEST: Mutex<()> = Mutex::new(());

    extern "C" fn ignore_test_signal(_signal: libc::c_int) {}

    #[test]
    fn deferred_finalize_retries_an_interrupted_wait_and_reaps() {
        let _serial = WAITPID_SIGNAL_TEST.lock().unwrap();
        WAITPID_ENTERED.store(false, Ordering::Release);
        WAITPID_INTERRUPTED.store(0, Ordering::Release);

        let action = SigAction::new(
            SigHandler::Handler(ignore_test_signal),
            SaFlags::empty(),
            SigSet::empty(),
        );
        let previous = unsafe { sigaction(Signal::SIGUSR2, &action) }.unwrap();

        let (mapping, shared) = new_shared_drop_state();
        let run = Container::new()
            .run_with_deferred_drop(|| (42, BlockingDrop { shared }))
            .unwrap();
        let child_pid = run.child.0.expect("deferred child pid");
        WAITPID_TEST_PID.store(child_pid.as_raw(), Ordering::Release);
        let waiting_thread = unsafe { libc::pthread_self() };
        let shared_address = shared as usize;
        let interrupter = std::thread::spawn(move || {
            while !WAITPID_ENTERED.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while WAITPID_INTERRUPTED.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
                assert_eq!(
                    unsafe { libc::pthread_kill(waiting_thread, libc::SIGUSR2) },
                    0
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            let shared = unsafe { &*(shared_address as *mut SharedDropState) };
            shared.release.store(true, Ordering::Release);
        });

        assert_eq!(run.finalize(), Ok(42));
        interrupter.join().unwrap();
        unsafe { sigaction(Signal::SIGUSR2, &previous) }.unwrap();
        WAITPID_TEST_PID.store(0, Ordering::Release);
        assert!(
            WAITPID_INTERRUPTED.load(Ordering::Acquire) > 0,
            "the signal did not interrupt waitpid"
        );
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(child_pid.as_raw(), &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(Errno::last(), Errno::ECHILD);

        unsafe { unmap_shared_drop_state(mapping, shared) };
    }

    struct ExitDuringDrop(i32);

    impl Drop for ExitDuringDrop {
        fn drop(&mut self) {
            unsafe { libc::_exit(self.0) }
        }
    }

    #[test]
    fn deferred_drop_exposes_cleanup_failure() {
        let run = Container::new()
            .run_with_deferred_drop(|| (42, ExitDuringDrop(71)))
            .unwrap();

        assert_eq!(run.provisional(), &42);
        assert_eq!(
            run.finalize(),
            Err(RunError::ExitStatus(ExitStatus::Exited(71)))
        );
    }

    #[test]
    fn mount_error_from_child_is_returned() {
        let source_dir = tempfile::tempdir().unwrap();
        let missing_source = source_dir.path().join("missing");

        let result = Container::new()
            .unshare(Namespace::USER | Namespace::MOUNT)
            .map_root()
            .mount(Mount::bind(missing_source, "/test"))
            .run(|| 42);

        assert_eq!(
            result,
            Err(RunError::Spawn(Error::new(Errno::ENOENT, Context::Mount)))
        );
    }

    #[test]
    fn test_directory_is_available_after_mount() {
        let result = Container::new()
            .unshare(Namespace::USER | Namespace::MOUNT)
            .map_root()
            .mount(Mount::tmpfs("/test").touch_target())
            .run(|| Path::new("/test").is_dir());

        assert_eq!(result, Ok(true));
    }

    /// A read-only bind of a source on a `nosuid`/`nodev` filesystem must work.
    ///
    /// ⚠️ REGRESSION TEST FOR A WHOLE LOST VALIDATE ARM, not a corner case.
    /// Inside a user namespace the kernel LOCKS the flags of mounts inherited
    /// from the parent namespace and refuses any remount that would clear one.
    /// A read-only bind is bind-then-remount, and the remount used to pass
    /// `MS_RDONLY` alone -- which asks to drop every other flag the source had.
    /// The mount returned EPERM and the container never spawned, so the guest
    /// did not fail, it never existed.
    ///
    /// Measured 2026-08-27: Hermit puts its frozen `/etc/group` and empty nscd
    /// directory in TMPDIR and binds each read-only, so a TMPDIR on
    /// `/run/user/<uid>` -- `nosuid,nodev` on any systemd host -- killed every
    /// container spawn. 610 of one arm's 612 e2e rows came from this one mount.
    ///
    /// ⚠️ THE SOURCE MUST BE MOUNTED BY THE HOST, NOT BY THIS CONTAINER. A tmpfs
    /// this test mounts itself lives in the container's own namespace, so its
    /// flags are NOT locked and the remount succeeds even unfixed -- a test that
    /// cannot fail. `/dev/shm` is host-mounted and carries both flags, so it
    /// reproduces the inheritance that makes them locked.
    #[test]
    fn a_readonly_bind_survives_a_nosuid_nodev_source() {
        let shm = Path::new("/dev/shm");
        // Fail closed rather than silently stop exercising the condition.
        let flags = nix::sys::statvfs::statvfs(shm).expect("statvfs /dev/shm");
        assert!(
            flags
                .flags()
                .contains(nix::sys::statvfs::FsFlags::ST_NOSUID)
                || flags.flags().contains(nix::sys::statvfs::FsFlags::ST_NODEV),
            "/dev/shm carries neither nosuid nor nodev on this host, so this test \
             would pass without exercising the locked-flag remount at all"
        );

        let source = tempfile::tempdir_in(shm).unwrap();
        let target = tempfile::tempdir().unwrap();

        let result = Container::new()
            .unshare(Namespace::USER | Namespace::MOUNT)
            .map_root()
            .mount(Mount::bind(source.path(), target.path()).readonly())
            .run(|| Path::new("/proc/self/mounts").is_file());

        assert_eq!(
            result,
            Ok(true),
            "a read-only bind whose source is nosuid/nodev must not fail; \
             EPERM here means the remount is dropping the source's locked flags"
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

    /// Pinning each guest to a different CPU must put it on a different CPU.
    ///
    /// ⚠️ THE IDENTIFIER THIS READS IS 32-BIT ON PURPOSE, AND AN 8-BIT ONE MADE
    /// THIS TEST UNPASSABLE ON THIS HOST. It used to read
    /// `get_feature_info().initial_local_apic_id()`, the LEGACY CPUID leaf 1
    /// `EBX[31:24]` field, which is a `u8` and so has 256 possible values.
    /// Measured at reverie main `b181b1bba20c846d277f500c25182a00e18add9a` on a
    /// 316-CPU host:
    ///
    /// ```text
    /// 316 observations, min id 0, max id 255, 256 distinct ids,
    /// exactly 60 ids seen twice -- and 316 - 256 = 60.
    /// ```
    ///
    /// So `max(count) == 1` could not hold for ANY amount of correct pinning,
    /// and the failure was deterministic rather than flaky. It had been
    /// recorded several times as "pre-existing and host-specific" and routinely
    /// skipped, which was true and left `validate.sh` step 2 red on main.
    ///
    /// ⚠️ BUT THE 256 CEILING IS NOT WHERE IT BREAKS, AND ASSUMING SO WOULD
    /// LEAVE THE NEXT READER ON A 64-CORE BOX BELIEVING THEY ARE SAFE. The
    /// legacy field drops the HIGH TOPOLOGY BITS, so ids repeat across sockets
    /// long before the count reaches 256: on this host the first collision is
    /// core 32 against core 0, both reporting id 0. Measured by enumerating
    /// only the first N cores with each identifier:
    ///
    /// ```text
    /// cores    16   32   33   64  128  256  316
    /// 8-bit    ok   ok  FAIL FAIL FAIL FAIL FAIL
    /// 32-bit   ok   ok   ok   ok   ok   ok   ok
    /// ```
    ///
    /// So this is not a test that was always broken. It is a test whose hidden
    /// assumption -- that the enumerated CPUs have distinct LEGACY apic ids --
    /// held on the smaller machines it was written against and stopped holding
    /// here, at 33 cores rather than at 257.
    ///
    /// CPUID leaf `0x0B` reports the 32-bit x2APIC id, which distinguishes as
    /// many CPUs as the machine has. Reading it does not weaken the assertion --
    /// the assertion is unchanged, and it is now able to fail for the reason it
    /// was written to catch instead of failing for arithmetic. Verified in that
    /// direction too: with affinity broken outright the repaired test reports
    /// `left: 316, right: 1`, and with affinity broken for half the cores it
    /// also fails.
    #[cfg(target_arch = "x86_64")]
    #[test]
    pub fn pin_affinity_to_all_cores() -> Result<(), Error> {
        use std::collections::HashMap;

        use raw_cpuid::CpuId;

        let cpus = num_cpus::get();
        println!("Total cpus {}", cpus);

        // Map the x2APIC id to the number of times we observed it:
        let mut results: HashMap<u32, usize> = HashMap::new();
        for core in 0..cpus {
            println!("  Launching guest with affinity set to {}", core);
            let mut container = Container::new();
            container.affinity(core);
            let which_core = container
                .run(|| {
                    let cpuid = CpuId::new();
                    // Every level of leaf 0x0B reports the same x2APIC id for
                    // the executing logical processor, so the first is enough.
                    cpuid
                        .get_extended_topology_info()
                        .and_then(|mut levels| levels.next())
                        .map(|level| level.x2apic_id())
                })
                .unwrap();
            // ⚠️ REFUSE RATHER THAN FALL BACK TO THE 8-BIT FIELD. On a host
            // this large the legacy id provably cannot answer the question, so
            // silently using it would restore exactly the defect above.
            let which_core = which_core.unwrap_or_else(|| {
                panic!(
                    "CPUID leaf 0x0B (extended topology) is unavailable, so no \
                     32-bit x2APIC id can be read; with {cpus} CPUs the legacy \
                     8-bit APIC id cannot distinguish them and this test cannot \
                     decide anything"
                )
            });
            println!("    Guest sees its on x2APIC id {}", which_core);
            *results.entry(which_core).or_default() += 1;
        }

        println!("Final table size {:?}", results.len());
        assert_eq!(
            results.values().fold(0, |n, v| std::cmp::max(n, *v)),
            1,
            "two guests pinned to different CPUs reported the same x2APIC id, \
             so affinity did not place them on distinct CPUs"
        );
        Ok(())
    }
}
