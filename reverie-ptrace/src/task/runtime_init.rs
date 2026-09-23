/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The ptrace controller owns these calls. No Tool or scheduler is installed
//! in the target, and no controller syscall is dispatched to the host Tool.

use std::os::unix::fs::MetadataExt;

use super::*;

const STACK_BYTES: usize = 2 * 1024 * 1024;
const PAGE: usize = 4096;
const MAX_CONTROL_STOPS: usize = 16384;

#[derive(Clone, Copy)]
enum CallKind {
    Loader,
    Initializer,
}

// Linux ptrace takes the kernel sigset size (64 signals), not glibc sigset_t.
fn signal_mask(task: &Stopped, replacement: Option<u64>) -> Result<u64, Errno> {
    let mut mask = replacement.unwrap_or_default();
    let operation = if replacement.is_some() {
        libc::PTRACE_SETSIGMASK
    } else {
        libc::PTRACE_GETSIGMASK
    };
    // SAFETY: task is stopped and the live eight-byte word is exactly the size
    // given to ptrace. Neither operation executes any target instruction.
    let result = unsafe {
        libc::ptrace(
            operation,
            task.pid().as_raw(),
            8_usize,
            &mut mask as *mut u64,
        )
    };
    if result == -1 {
        Err(Errno::new(
            std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO),
        ))
    } else {
        Ok(mask)
    }
}

fn register_words(r: &libc::user_regs_struct) -> [u64; 27] {
    [
        r.r15, r.r14, r.r13, r.r12, r.rbp, r.rbx, r.r11, r.r10, r.r9, r.r8, r.rax, r.rcx, r.rdx,
        r.rsi, r.rdi, r.orig_rax, r.rip, r.cs, r.eflags, r.rsp, r.ss, r.fs_base, r.gs_base, r.ds,
        r.es, r.fs, r.gs,
    ]
}

impl<L: Tool + 'static> TracedTask<L> {
    fn controller_error(&self, message: impl ToString) -> Error {
        Error::runtime(
            self.tid(),
            "LiteInst controller initialization",
            message.to_string(),
        )
    }

    pub(super) fn is_liteinst_controller_entry(&self, task: &Stopped) -> Result<bool, TraceError> {
        if !self
            .global_state
            .liteinst_runtime
            .as_ref()
            .is_some_and(|config| config.initialization.is_some())
        {
            return Ok(false);
        }
        let Some(guard) = self.liteinst_entry_guard else {
            return Ok(false);
        };
        if task.getregs()?.rip != guard.address.checked_add(1).ok_or(Errno::EOVERFLOW)?
            || !is_expected_breakpoint_trap(task, guard.address, false)?
        {
            return Ok(false);
        }
        let observed: u64 =
            task.read_value(Addr::from_raw(guard.address as usize).ok_or(Errno::EFAULT)?)?;
        Ok(observed == (guard.saved_instruction & !0xff) | 0xcc)
    }

    async fn controller_syscall(
        &mut self,
        task: Stopped,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<(Stopped, u64), Error> {
        let saved_regs = task.getregs()?;
        let result = self.untraced_syscall(task, nr, args).await??;
        let task = Stopped::new_unchecked(self.tid());
        // Ordinary syscall injection exposes RAX to its caller. This private
        // operation has no guest result and must preserve that register too.
        task.setregs(&saved_regs)?;
        if register_words(&task.getregs()?) != register_words(&saved_regs) {
            return Err(self.controller_error("private syscall register restoration mismatch"));
        }
        Ok((task, result as u64))
    }

    fn controller_write(
        &self,
        task: &mut Stopped,
        address: usize,
        bytes: &[u8],
    ) -> Result<(), Error> {
        task.write_exact(AddrMut::from_raw(address).ok_or(Errno::EFAULT)?, bytes)?;
        let mut observed = vec![0; bytes.len()];
        task.read_exact(address, &mut observed)?;
        if observed != bytes {
            return Err(self.controller_error("target staging readback mismatch"));
        }
        Ok(())
    }

    // Create the file only after the original loader has completed. Inherited
    // staging descriptors would change descriptors allocated by guest ctors.
    async fn controller_stage_image(
        &mut self,
        mut task: Stopped,
        scratch: usize,
        name: &[u8],
        bytes: &[u8],
    ) -> Result<(Stopped, u64, (u64, u64, u64)), Error> {
        self.controller_write(&mut task, scratch, name)?;
        let (task, fd) = self
            .controller_syscall(
                task,
                Sysno::memfd_create,
                SyscallArgs::new(
                    scratch,
                    (libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) as usize,
                    0,
                    0,
                    0,
                    0,
                ),
            )
            .await?;
        let (task, _) = self
            .controller_syscall(
                task,
                Sysno::ftruncate,
                SyscallArgs::new(fd as usize, bytes.len(), 0, 0, 0, 0),
            )
            .await?;
        let (mut task, buffer) = self
            .controller_syscall(
                task,
                Sysno::mmap,
                SyscallArgs::new(
                    0,
                    bytes.len(),
                    (libc::PROT_READ | libc::PROT_WRITE) as usize,
                    libc::MAP_SHARED as usize,
                    fd as usize,
                    0,
                ),
            )
            .await?;
        self.controller_write(&mut task, buffer as usize, bytes)?;
        let (task, _) = self
            .controller_syscall(
                task,
                Sysno::munmap,
                SyscallArgs::new(buffer as usize, bytes.len(), 0, 0, 0, 0),
            )
            .await?;
        let seals =
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        let (task, _) = self
            .controller_syscall(
                task,
                Sysno::fcntl,
                SyscallArgs::new(
                    fd as usize,
                    libc::F_ADD_SEALS as usize,
                    seals as usize,
                    0,
                    0,
                    0,
                ),
            )
            .await?;
        let (task, observed) = self
            .controller_syscall(
                task,
                Sysno::fcntl,
                SyscallArgs::new(fd as usize, libc::F_GET_SEALS as usize, 0, 0, 0, 0),
            )
            .await?;
        if observed != seals as u64 {
            return Err(self.controller_error("target image seals mismatch"));
        }
        let metadata = std::fs::metadata(format!("/proc/{}/fd/{fd}", task.pid()))
            .map_err(|e| self.controller_error(e))?;
        let identity = (
            libc::major(metadata.dev()) as u64,
            libc::minor(metadata.dev()) as u64,
            metadata.ino(),
        );
        Ok((task, fd, identity))
    }

    async fn controller_call(
        &mut self,
        mut task: Stopped,
        function: u64,
        stack_top: usize,
        arguments: [u64; 2],
        kind: CallKind,
    ) -> Result<(Stopped, u64), Error> {
        let guard = self.liteinst_entry_guard.ok_or(Errno::EPROTO)?;
        self.controller_write(&mut task, stack_top - 8, &guard.address.to_ne_bytes())?;
        let mut regs = task.getregs()?;
        regs.rip = function;
        regs.rsp = (stack_top - 8) as u64;
        regs.rdi = arguments[0];
        regs.rsi = arguments[1];
        regs.orig_rax = u64::MAX;
        regs.eflags = liteinst_helper_entry_rflags(regs.eflags);
        task.setregs(&regs)?;
        let mut begin = false;
        let mut ready = false;
        for _ in 0..MAX_CONTROL_STOPS {
            let wait = self.resume_stopped(task, None)?.next_state().await?;
            self.arm_liteinst_wait(&wait);
            task = match wait {
                Wait::Stopped(stopped, Event::Seccomp) => stopped,
                Wait::Stopped(stopped, Event::Signal(Signal::SIGTRAP)) => {
                    let regs = stopped.getregs()?;
                    if regs.rip == guard.address + 1 {
                        if !self.is_liteinst_controller_entry(&stopped)?
                            || regs.rsp != stack_top as u64
                        {
                            return Err(
                                self.controller_error("invalid controller call return provenance")
                            );
                        }
                        if matches!(kind, CallKind::Initializer)
                            && (!begin || !ready || regs.rax != 0)
                        {
                            return Err(self.controller_error(format!(
                                "initializer returned {} without successful Begin/Ready",
                                regs.rax as i64
                            )));
                        }
                        return Ok((stopped, regs.rax));
                    }
                    // A marker in RAX alone is never sufficient: validate the
                    // kernel trap, exact mapped image, frame, and trap RIP.
                    if !matches!(kind, CallKind::Initializer)
                        || !is_expected_breakpoint_trap(
                            &stopped,
                            regs.rip.saturating_sub(1),
                            false,
                        )?
                    {
                        return Err(self.controller_error("unexpected controller call trap"));
                    }
                    let opcode: u8 = stopped.read_value(
                        Addr::from_raw(regs.rip.saturating_sub(1) as usize).ok_or(Errno::EFAULT)?,
                    )?;
                    if opcode != 0xcc {
                        return Err(self.controller_error("handshake trap instruction changed"));
                    }
                    match self.classify_liteinst_trap(&stopped, &regs) {
                        Some(LiteinstTrap::HandshakeBegin) if !begin && !ready => begin = true,
                        Some(LiteinstTrap::HandshakeReady) if begin && !ready => ready = true,
                        _ => return Err(self.controller_error("invalid or repeated Begin/Ready")),
                    }
                    stopped
                }
                Wait::Stopped(_, event) => {
                    return Err(
                        self.controller_error(format!("unexpected controller event: {event:?}"))
                    );
                }
                Wait::Exited(_, status) => {
                    return Err(self.controller_error(format!(
                        "target exited during controller call: {status:?}"
                    )));
                }
            };
        }
        Err(self.controller_error("controller call stop bound exceeded"))
    }

    async fn controller_load_image(
        &mut self,
        task: Stopped,
        init: &crate::LiteinstRuntimeInit,
        scratch: usize,
        stack_top: usize,
        name: &[u8],
        bytes: &[u8],
    ) -> Result<(Stopped, (u64, u64, u64)), Error> {
        let (mut task, fd, identity) = self
            .controller_stage_image(task, scratch, name, bytes)
            .await?;
        // glibc caches dlopen filenames even after their descriptor is closed.
        // Use distinct names for the two sealed files if Linux reuses the fd.
        let path = if name == b"reverie-liteinst-runtime\0" {
            format!("/proc/{}/fd/{fd}\0", task.pid())
        } else {
            format!("/proc/self/fd/{fd}\0")
        };
        self.controller_write(&mut task, scratch, path.as_bytes())?;
        let loader = crate::target_loader::resolve_dlopen(
            &task,
            &init.expected_loader,
            &init.loader_version,
        )
        .map_err(|e| self.controller_error(e))?;
        let (task, handle) = self
            .controller_call(
                task,
                loader.address,
                stack_top,
                [
                    scratch as u64,
                    libc::RTLD_NOW as u64 | libc::RTLD_LOCAL as u64,
                ],
                CallKind::Loader,
            )
            .await?;
        if handle == 0 {
            return Err(self.controller_error("dlopen refused sealed controller image"));
        }
        let (task, _) = self
            .controller_syscall(
                task,
                Sysno::close,
                SyscallArgs::new(fd as usize, 0, 0, 0, 0, 0),
            )
            .await?;
        Ok((task, identity))
    }

    /// Preserve the same initialization deadline and typed failure path whether
    /// entry was reached by ordinary execution or by precise single stepping.
    /// Success leaves the original entry instruction stopped and unexecuted.
    pub(super) async fn initialize_liteinst_at_entry_bounded(
        &mut self,
        task: Stopped,
    ) -> Result<Stopped, TraceError> {
        let initialized = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            Box::pin(self.initialize_liteinst_at_entry(task)),
        )
        .await;
        let result = match initialized {
            Ok(result) => result,
            Err(_) => Err(self.controller_error("deadline exceeded")),
        };
        result.map_err(|error| {
            self.record_liteinst_failure(
                LiteinstActivationFailureReason::UnexpectedActivationTrap,
                error,
            );
            Errno::EPROTO.into()
        })
    }

    async fn initialize_liteinst_at_entry(&mut self, task: Stopped) -> Result<Stopped, Error> {
        if !self.is_liteinst_controller_entry(&task)? {
            return Err(self.controller_error("unauthenticated executable-entry stop"));
        }
        let config = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .ok_or(Errno::EPROTO)?;
        let init = Arc::clone(config.initialization.as_ref().ok_or(Errno::EPROTO)?);
        if config.root_tid.get() != Some(&task.pid())
            || config.multi_task.load(Ordering::SeqCst)
            || self.pending_signal.is_some()
            || self.pending_syscall.is_some()
            || self.injected_syscall_frame.is_some()
            || self.liteinst_runtime.lock().unwrap().phase != LiteinstRuntimePhase::Waiting
        {
            return Err(self.controller_error("initialization requires an idle, single root task"));
        }
        let tasks = std::fs::read_dir(format!("/proc/{}/task", task.pid()))
            .map_err(|e| self.controller_error(e))?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| self.controller_error(e))?;
        if tasks.len() != 1 {
            return Err(self.controller_error("initialization requires all tasks quiescent"));
        }
        let vdso = self
            .liteinst_runtime
            .lock()
            .unwrap()
            .initialization_vdso
            .clone()
            .ok_or_else(|| self.controller_error("initial kernel vDSO observation is absent"))?;
        let libgcc_loaded =
            crate::target_loader::validate_runtime_init_boundary(&task, &init, None, &vdso)
                .map_err(|e| self.controller_error(e))?;
        let mut original_regs = task.getregs()?;
        original_regs.rip = self.liteinst_entry_guard.ok_or(Errno::EPROTO)?.address;
        let original_xstate = task.getxstate()?;
        let original_mask = signal_mask(&task, None)?;
        let blocked_mask = original_mask | (1_u64 << (reverie::PERF_EVENT_SIGNAL as i32 - 1));
        signal_mask(&task, Some(blocked_mask))?;
        if signal_mask(&task, None)? != blocked_mask {
            return Err(self.controller_error("signal mask readback mismatch"));
        }
        let timer = self.timer.suspend_for_controller()?;

        // Two inaccessible guard pages bracket a fresh controller stack. The
        // original stack, including the red zone and loader arguments, is idle.
        let (task, allocation) = self
            .controller_syscall(
                task,
                Sysno::mmap,
                SyscallArgs::new(
                    0,
                    STACK_BYTES + 2 * PAGE,
                    libc::PROT_NONE as usize,
                    (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as usize,
                    usize::MAX,
                    0,
                ),
            )
            .await?;
        let scratch = allocation as usize + PAGE;
        let stack_top = scratch + STACK_BYTES;
        let (task, _) = self
            .controller_syscall(
                task,
                Sysno::mprotect,
                SyscallArgs::new(
                    scratch,
                    STACK_BYTES,
                    (libc::PROT_READ | libc::PROT_WRITE) as usize,
                    0,
                    0,
                    0,
                ),
            )
            .await?;
        let mut saved = LiteinstHelperSavedState {
            cpuid_policy: LiteinstCpuidPolicy::Unsupported,
            tsc_policy: LiteinstTscPolicy::Unsupported,
            regs: original_regs,
            xstate: original_xstate,
            stack_address: scratch,
            stack_value: 0,
        };
        let (task, policy) = self.prepare_liteinst_helper_cpuid(task).await;
        saved.cpuid_policy = policy.map_err(|e| self.controller_error(e))?;
        let (mut task, policy) = self.prepare_liteinst_helper_tsc(task, scratch).await;
        saved.tsc_policy = policy.map_err(|e| self.controller_error(e))?;
        let errno_function = crate::target_loader::resolve_runtime_errno(&task, &init)
            .map_err(|e| self.controller_error(e))?;
        let errno_address;
        (task, errno_address) = self
            .controller_call(task, errno_function, stack_top, [0, 0], CallKind::Loader)
            .await?;
        let errno_end = errno_address.checked_add(3).ok_or(Errno::EOVERFLOW)?;
        if !guest_maps(task.pid()).ok_or(Errno::EIO)?.iter().any(|map| {
            map.readable
                && map.writable
                && !map.executable
                && map.contains(errno_address)
                && map.contains(errno_end)
        }) {
            return Err(self.controller_error("libc errno location is not writable thread storage"));
        }
        let original_errno: i32 =
            task.read_value(Addr::from_raw(errno_address as usize).ok_or(Errno::EFAULT)?)?;
        if !libgcc_loaded {
            (task, _) = self
                .controller_load_image(
                    task,
                    &init,
                    scratch,
                    stack_top,
                    b"reverie-liteinst-libgcc\0",
                    &init.expected_libgcc,
                )
                .await?;
        }
        if !crate::target_loader::validate_runtime_init_boundary(&task, &init, None, &vdso)
            .map_err(|e| self.controller_error(e))?
        {
            return Err(self.controller_error("bound libgcc absent after loading"));
        }
        let identity;
        (task, identity) = self
            .controller_load_image(
                task,
                &init,
                scratch,
                stack_top,
                b"reverie-liteinst-runtime\0",
                &init.runtime,
            )
            .await?;
        crate::target_loader::validate_runtime_init_boundary(&task, &init, Some(identity), &vdso)
            .map_err(|e| self.controller_error(e))?;
        self.liteinst_runtime.lock().unwrap().runtime_identity = Some(identity);
        self.controller_write(&mut task, scratch, &init.config_words[0].to_ne_bytes())?;
        self.controller_write(&mut task, scratch + 8, &init.config_words[1].to_ne_bytes())?;
        let initializer = crate::target_loader::resolve_host_initializer(&task, &init.runtime)
            .map_err(|e| self.controller_error(e))?;
        if initializer.mapping_identity != identity {
            return Err(self.controller_error("initializer image identity changed"));
        }
        (task, _) = self
            .controller_call(
                task,
                initializer.address,
                stack_top,
                [scratch as u64, 0],
                CallKind::Initializer,
            )
            .await?;
        self.controller_write(
            &mut task,
            errno_address as usize,
            &original_errno.to_ne_bytes(),
        )?;
        let failures;
        (task, failures) = self.restore_liteinst_helper_state(task, &saved).await;
        if !failures.is_empty() {
            return Err(self.controller_error(failures.join("; ")));
        }
        (task, _) = self
            .controller_syscall(
                task,
                Sysno::munmap,
                SyscallArgs::new(allocation as usize, STACK_BYTES + 2 * PAGE, 0, 0, 0, 0),
            )
            .await?;
        if register_words(&task.getregs()?) != register_words(&saved.regs)
            || task.getxstate()? != saved.xstate
        {
            return Err(self.controller_error("architectural context restoration mismatch"));
        }
        self.restore_liteinst_entry_guard(&mut task)?;
        signal_mask(&task, Some(original_mask))?;
        if signal_mask(&task, None)? != original_mask {
            return Err(self.controller_error("signal mask restoration mismatch"));
        }
        self.timer.resume_from_controller(timer)?;
        {
            let mut state = self.liteinst_runtime.lock().unwrap();
            if state.phase != LiteinstRuntimePhase::Bootstrap {
                return Err(self.controller_error("initializer did not reach Bootstrap"));
            }
            state.phase = LiteinstRuntimePhase::Ready;
            state.ready_generation = Some(state.generation);
        }
        Ok(task)
    }
}
