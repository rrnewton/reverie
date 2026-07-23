/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use kvm_bindings::KVM_MAX_CPUID_ENTRIES;
use kvm_bindings::kvm_enable_cap;
use kvm_bindings::kvm_userspace_memory_region;
use kvm_ioctls::Cap;
use kvm_ioctls::Kvm;
use kvm_ioctls::VcpuExit;
use kvm_ioctls::VcpuFd;
use kvm_ioctls::VmFd;
use reverie::GlobalTool;
use reverie::Pid;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;

use crate::Error;
use crate::GuestMemory;
use crate::Result;
use crate::SyscallRequest;
use crate::guest::KvmGuest;
use crate::guest::KvmMemory;
use crate::guest::block_on;
use crate::guest::regs_from_frame;

/// KVM currently permits userspace exits for this standardized hypercall.
/// The prototype uses it as a transport opcode and places the syscall frame
/// address in the first hypercall argument.
pub const VMCALL_SYSCALL_TRANSPORT: u64 = 12;

const VMCALL: [u8; 3] = [0x0f, 0x01, 0xc1];
const VMMCALL: [u8; 3] = [0x0f, 0x01, 0xd9];
const HLT: u8 = 0xf4;

/// A single-vCPU KVM backend used to exercise the syscall transport.
pub struct KvmBackend {
    // Field order ensures the vCPU and VM are dropped before registered memory.
    vcpu: VcpuFd,
    vm: VmFd,
    memory: GuestMemory,
    _kvm: Kvm,
    hypercall_instruction: [u8; 3],
}

impl KvmBackend {
    /// Creates a VM with one real-mode vCPU and a memory slot starting at GPA 0x1000.
    pub fn new(memory_size: usize) -> Result<Self> {
        let kvm = Kvm::new()?;
        let vm = kvm.create_vm()?;
        if !vm.check_extension(Cap::ExitHypercall) {
            return Err(Error::HypercallExitUnsupported);
        }

        let hypercall_instruction = supported_hypercall_instruction(&kvm)?;
        let cap = kvm_enable_cap {
            cap: Cap::ExitHypercall as u32,
            args: [1_u64 << VMCALL_SYSCALL_TRANSPORT, 0, 0, 0],
            ..Default::default()
        };
        vm.enable_cap(&cap)?;

        let memory = GuestMemory::new(0x1000, memory_size)?;
        let region = kvm_userspace_memory_region {
            slot: 0,
            guest_phys_addr: memory.guest_base(),
            memory_size: memory.len() as u64,
            userspace_addr: memory.host_address(),
            flags: 0,
        };
        // SAFETY: memory owns a page-aligned mapping that remains live until
        // after vcpu and vm are dropped, and slot 0 is registered only once.
        unsafe {
            vm.set_user_memory_region(region)?;
        }

        let vcpu = vm.create_vcpu(0)?;
        Ok(Self {
            vcpu,
            vm,
            memory,
            _kvm: kvm,
            hypercall_instruction,
        })
    }

    /// Returns the VM's guest memory.
    pub fn memory(&self) -> &GuestMemory {
        &self.memory
    }

    /// Returns mutable access to the VM's guest memory.
    pub fn memory_mut(&mut self) -> &mut GuestMemory {
        &mut self.memory
    }

    /// Installs a syscall frame and a `vmcall`/`vmmcall; hlt` guest program.
    pub fn install_syscall(
        &mut self,
        entry_point: u64,
        frame_address: u64,
        request: SyscallRequest,
    ) -> Result<()> {
        let code = [
            self.hypercall_instruction[0],
            self.hypercall_instruction[1],
            self.hypercall_instruction[2],
            HLT,
        ];
        self.memory.write(entry_point, &code)?;
        request.write_to(&mut self.memory, frame_address)?;

        let mut sregs = self.vcpu.get_sregs()?;
        sregs.cs.base = 0;
        sregs.cs.selector = 0;
        self.vcpu.set_sregs(&sregs)?;

        let mut regs = self.vcpu.get_regs()?;
        regs.rip = entry_point;
        regs.rflags = 2;
        regs.rax = VMCALL_SYSCALL_TRANSPORT;
        regs.rbx = frame_address;
        // KVM validates the MAP_GPA_RANGE argument shape before forwarding
        // the enabled hypercall to userspace; describe one page here.
        regs.rcx = 1;
        regs.rdx = 0;
        self.vcpu.set_regs(&regs)?;
        Ok(())
    }

    /// Runs until the guest halts, invoking `handler` for each syscall vmcall.
    pub fn run<F>(&mut self, mut handler: F) -> Result<()>
    where
        F: FnMut(&SyscallRequest, &GuestMemory) -> i64,
    {
        loop {
            match self.vcpu.run()? {
                VcpuExit::Hypercall(exit) => {
                    if exit.nr != VMCALL_SYSCALL_TRANSPORT {
                        return Err(Error::UnexpectedHypercall(exit.nr));
                    }
                    let request = SyscallRequest::read_from(&self.memory, exit.args[0])?;
                    *exit.ret = handler(&request, &self.memory) as u64;
                }
                VcpuExit::Hlt => return Ok(()),
                exit => return Err(Error::UnexpectedVcpuExit(format!("{exit:?}"))),
            }
        }
    }

    /// Exposes the VM fd for future backend setup without transferring ownership.
    pub fn vm_fd(&self) -> &VmFd {
        &self.vm
    }

    /// Reads the vCPU general-purpose registers via `KVM_GET_REGS`.
    ///
    /// This is the raw KVM vCPU register building block. It is not yet wired
    /// into `Guest::regs` (which currently synthesises registers from the
    /// intercepted syscall frame) because a KVM exit handle borrows the vCPU for
    /// the duration of [`Self::run_tool`]; see `guest` module docs.
    pub fn vcpu_regs(&self) -> Result<kvm_bindings::kvm_regs> {
        Ok(self.vcpu.get_regs()?)
    }

    /// Runs the guest, dispatching every intercepted `vmcall` syscall to `tool`
    /// through a freshly built [`KvmGuest`], until the guest halts.
    ///
    /// `thread_state` is owned by the caller so tool state (e.g. a syscall
    /// count) is observable after the run. `pid` is the synthetic identity
    /// reported to the tool for the single vCPU.
    ///
    /// This is the minimal `TracerBuilder`-style entry point: it makes an
    /// arbitrary async `reverie::Tool` observe syscalls over the KVM
    /// interception path. See the `guest` module docs for scope and limitations.
    pub fn run_tool<T: Tool>(
        &mut self,
        tool: &T,
        global_state: &T::GlobalState,
        config: &<T::GlobalState as GlobalTool>::Config,
        thread_state: &mut T::ThreadState,
        pid: Pid,
    ) -> Result<()> {
        loop {
            // NOTE: `exit` borrows `self.vcpu`; we only touch the disjoint
            // `self.memory` field while it is live, so registers handed to the
            // tool are synthesised from the frame rather than read via
            // `self.vcpu_regs()` (which would re-borrow the vCPU).
            match self.vcpu.run()? {
                VcpuExit::Hypercall(exit) => {
                    if exit.nr != VMCALL_SYSCALL_TRANSPORT {
                        return Err(Error::UnexpectedHypercall(exit.nr));
                    }
                    let request = SyscallRequest::read_from(&self.memory, exit.args[0])?;
                    let regs = regs_from_frame(&request);
                    let args = *request.args();
                    let mut guest = KvmGuest::<T>::new(
                        KvmMemory::new(&self.memory),
                        regs,
                        pid,
                        pid,
                        None,
                        thread_state,
                        global_state,
                        config,
                    );
                    let syscall = Syscall::from_raw(
                        Sysno::from(request.number() as u32),
                        SyscallArgs::new(
                            args[0] as usize,
                            args[1] as usize,
                            args[2] as usize,
                            args[3] as usize,
                            args[4] as usize,
                            args[5] as usize,
                        ),
                    );
                    let value = match block_on(tool.handle_syscall_event(&mut guest, syscall)) {
                        Ok(value) => value as u64,
                        // Best-effort error encoding; structured tool errors are
                        // out of scope for this minimal milestone.
                        Err(_) => (-libc::EIO as i64) as u64,
                    };
                    *exit.ret = value;
                }
                VcpuExit::Hlt => return Ok(()),
                exit => return Err(Error::UnexpectedVcpuExit(format!("{exit:?}"))),
            }
        }
    }
}

fn supported_hypercall_instruction(kvm: &Kvm) -> Result<[u8; 3]> {
    let cpuid = kvm.get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)?;
    let supports_vmcall = cpuid
        .as_slice()
        .iter()
        .find(|entry| entry.function == 1)
        .is_some_and(|entry| entry.ecx & (1 << 5) != 0);
    if supports_vmcall {
        return Ok(VMCALL);
    }

    let supports_vmmcall = cpuid
        .as_slice()
        .iter()
        .find(|entry| entry.function == 0x8000_0001)
        .is_some_and(|entry| entry.ecx & (1 << 2) != 0);
    if supports_vmmcall {
        return Ok(VMMCALL);
    }
    Err(Error::HypercallInstructionUnsupported)
}
