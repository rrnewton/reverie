/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Classic-BPF seccomp filter for LD_PRELOAD instrumentation.
//!
//! The filter traps every real syscall entry with `SECCOMP_RET_TRAP` (delivering
//! a thread-directed `SIGSYS`) **except**:
//!
//! * the runtime's exact `rt_sigreturn` restorer site and the process libc's
//!   byte-validated exact restorer site; and
//! * the runtime's other exact trusted syscall sites (see [`crate::trap`]). The
//!   lifecycle installs a private-memory gate and a guest-permissions gate.
//!   The sites let the handler and
//!   dispatcher execute real syscalls without re-trapping, avoiding infinite
//!   recursion.
//!
//! Exact-IP allowlisting is a routing boundary, not control-flow integrity.
//! The supported trusted-guest model excludes crafted transfers into hidden
//! runtime text (including libc's hidden restorer), just as it already excludes
//! jumps into the ordinary syscall gates. Within that boundary, a custom signal
//! restorer at an ordinary guest address traps: only the two validated
//! restorers' exact syscall/return addresses may execute `rt_sigreturn`.
//!
//! Coverage boundaries (proven by the `research-ldpreload-derisking` task) are
//! documented on [`SeccompFilter`]: vDSO fast paths and the ~40 loader/startup
//! syscalls before the constructor runs are *not* covered, and this filter is
//! for trusted, glibc-linked, non-`AT_SECURE`, no-exec guests only. Installation
//! recognizes one exact nine-byte canonical x86-64 glibc restorer encoding and
//! fails closed before seccomp activation for any other libc encoding.

use std::io;
use std::ptr;

/// `AUDIT_ARCH_X86_64` from `<linux/audit.h>`.
const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;

// Offsets into `struct seccomp_data`.
const SECCOMP_DATA_NR_OFFSET: u32 = 0;
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
const SECCOMP_DATA_IP_LOW_OFFSET: u32 = 8;
const SECCOMP_DATA_IP_HIGH_OFFSET: u32 = 12;

const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_TRAP: u32 = 0x0003_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_RET_K: u16 = 0x06;

/// The two instruction-pointer values associated with a trusted syscall gate.
///
/// Both addresses must live in the same 4 GiB half so the classic-BPF filter
/// can match the 64-bit instruction pointer with a single high-word compare
/// plus a low-word compare.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrustedGate {
    /// Address of the trusted `syscall` instruction.
    pub syscall_ip: u64,
    /// Address immediately after it (its return site).
    pub return_ip: u64,
}

impl TrustedGate {
    /// Preserve the original public constructor contract: classic BPF can
    /// represent any two exact addresses that share one high word.
    fn validate_legacy(&self) -> io::Result<()> {
        if self.syscall_ip >> 32 != self.return_ip >> 32 {
            return Err(io::Error::other(
                "trusted syscall gate crosses a 4GiB boundary",
            ));
        }
        Ok(())
    }

    /// Require the exact x86-64 `syscall`/return pair emitted by the new
    /// runtime-owned gate and restorer paths. This stricter contract must not
    /// leak into the backward-compatible public constructors above.
    fn validate_runtime(&self) -> io::Result<()> {
        if self.syscall_ip == 0 {
            return Err(io::Error::other(
                "trusted syscall gate has a null instruction address",
            ));
        }
        if self.syscall_ip.checked_add(2) != Some(self.return_ip) {
            return Err(io::Error::other(
                "trusted syscall gate return is not immediately after syscall",
            ));
        }
        if self.syscall_ip >> 32 != self.return_ip >> 32 {
            return Err(io::Error::other(
                "trusted syscall gate crosses a 4GiB boundary",
            ));
        }
        Ok(())
    }
}

/// A built seccomp program ready to install.
pub struct SeccompFilter {
    program: Vec<libc::sock_filter>,
}

impl SeccompFilter {
    /// Build the legacy trap-everything-but-the-gate filter for `gate`.
    ///
    /// This preserves the original public contract, in which `rt_sigreturn` is
    /// allowed independently of its instruction pointer. Runtime code needing
    /// an exact restorer must use [`Self::for_trusted_gate_with_restorer`].
    pub fn for_trusted_gate(gate: TrustedGate) -> io::Result<Self> {
        gate.validate_legacy()?;
        let high = (gate.syscall_ip >> 32) as u32;
        let program = vec![
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARCH_OFFSET),
            jump(BPF_JMP_JEQ_K, AUDIT_ARCH_X86_64, 1, 0),
            stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET),
            jump(BPF_JMP_JEQ_K, libc::SYS_rt_sigreturn as u32, 6, 0),
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_IP_HIGH_OFFSET),
            jump(BPF_JMP_JEQ_K, high, 0, 3),
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_IP_LOW_OFFSET),
            jump(BPF_JMP_JEQ_K, gate.syscall_ip as u32, 2, 0),
            jump(BPF_JMP_JEQ_K, gate.return_ip as u32, 1, 0),
            stmt(BPF_RET_K, SECCOMP_RET_TRAP),
            stmt(BPF_RET_K, SECCOMP_RET_ALLOW),
        ];
        Ok(Self { program })
    }

    /// Build the legacy exact-address filter for two ordinary trusted gates.
    ///
    /// As with [`Self::for_trusted_gate`], this preserves unrestricted
    /// `rt_sigreturn` compatibility. New runtime code should use
    /// [`Self::for_trusted_gates_with_restorer`].
    pub fn for_trusted_gates(first: TrustedGate, second: TrustedGate) -> io::Result<Self> {
        first.validate_legacy()?;
        second.validate_legacy()?;
        let program = vec![
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARCH_OFFSET),
            jump(BPF_JMP_JEQ_K, AUDIT_ARCH_X86_64, 1, 0),
            stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET),
            jump(BPF_JMP_JEQ_K, libc::SYS_rt_sigreturn as u32, 11, 0),
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_IP_HIGH_OFFSET),
            jump(BPF_JMP_JEQ_K, (first.syscall_ip >> 32) as u32, 0, 3),
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_IP_LOW_OFFSET),
            jump(BPF_JMP_JEQ_K, first.syscall_ip as u32, 7, 0),
            jump(BPF_JMP_JEQ_K, first.return_ip as u32, 6, 0),
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_IP_HIGH_OFFSET),
            jump(BPF_JMP_JEQ_K, (second.syscall_ip >> 32) as u32, 0, 3),
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_IP_LOW_OFFSET),
            jump(BPF_JMP_JEQ_K, second.syscall_ip as u32, 2, 0),
            jump(BPF_JMP_JEQ_K, second.return_ip as u32, 1, 0),
            stmt(BPF_RET_K, SECCOMP_RET_TRAP),
            stmt(BPF_RET_K, SECCOMP_RET_ALLOW),
        ];
        Ok(Self { program })
    }

    /// Build an exact-address filter for one ordinary syscall gate and one
    /// distinct runtime-owned `rt_sigreturn` restorer gate.
    pub fn for_trusted_gate_with_restorer(
        gate: TrustedGate,
        rt_sigreturn_gate: TrustedGate,
    ) -> io::Result<Self> {
        Self::for_regular_gates(&[gate], &[rt_sigreturn_gate])
    }

    /// Build an exact-address filter for runtime-private and guest-forwarding
    /// gates plus a distinct runtime-owned `rt_sigreturn` restorer. Each pair
    /// is checked independently, including its full high word; no address
    /// range or low-word alias is implicitly trusted. `rt_sigreturn` bypasses
    /// the ordinary gates, while every other syscall bypasses the restorer.
    pub fn for_trusted_gates_with_restorer(
        first: TrustedGate,
        second: TrustedGate,
        rt_sigreturn_gate: TrustedGate,
    ) -> io::Result<Self> {
        Self::for_regular_gates(&[first, second], &[rt_sigreturn_gate])
    }

    /// Build the runtime filter with multiple independently validated exact
    /// `rt_sigreturn` gates. This is crate-private so the generic public API
    /// does not grow a way to admit arbitrary caller-selected restorers.
    pub(crate) fn for_trusted_gates_with_restorers(
        first: TrustedGate,
        second: TrustedGate,
        rt_sigreturn_gates: &[TrustedGate],
    ) -> io::Result<Self> {
        Self::for_regular_gates(&[first, second], rt_sigreturn_gates)
    }

    fn for_regular_gates(
        regular_gates: &[TrustedGate],
        rt_sigreturn_gates: &[TrustedGate],
    ) -> io::Result<Self> {
        if rt_sigreturn_gates.is_empty() {
            return Err(io::Error::other(
                "runtime filter requires an exact rt_sigreturn gate",
            ));
        }
        for gate in regular_gates
            .iter()
            .copied()
            .chain(rt_sigreturn_gates.iter().copied())
        {
            gate.validate_runtime()?;
        }
        let all_gates: Vec<_> = regular_gates
            .iter()
            .copied()
            .chain(rt_sigreturn_gates.iter().copied())
            .collect();
        for (index, gate) in all_gates.iter().enumerate() {
            for other in &all_gates[index + 1..] {
                if [gate.syscall_ip, gate.return_ip]
                    .into_iter()
                    .any(|address| address == other.syscall_ip || address == other.return_ip)
                {
                    return Err(io::Error::other("trusted syscall gates overlap"));
                }
            }
        }

        let mut program = vec![
            // Reject any non-x86-64 syscall ABI before granting any gate.
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARCH_OFFSET),
            jump(BPF_JMP_JEQ_K, AUDIT_ARCH_X86_64, 1, 0),
            stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
            stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET),
            // Patched below to select the restorer-only branch.
            jump(BPF_JMP_JEQ_K, libc::SYS_rt_sigreturn as u32, 0, 0),
        ];
        let sigreturn_selector = program.len() - 1;

        let mut allow_jumps = Vec::new();
        for gate in regular_gates {
            append_gate_check(&mut program, *gate, &mut allow_jumps);
        }
        program.push(stmt(BPF_RET_K, SECCOMP_RET_TRAP));

        let rt_sigreturn_check = program.len();
        for gate in rt_sigreturn_gates {
            append_gate_check(&mut program, *gate, &mut allow_jumps);
        }
        program.push(stmt(BPF_RET_K, SECCOMP_RET_TRAP));

        let allow = program.len();
        program.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
        program[sigreturn_selector].jt = forward_offset(sigreturn_selector, rt_sigreturn_check)?;
        for index in allow_jumps {
            program[index].jt = forward_offset(index, allow)?;
        }
        Ok(Self { program })
    }

    /// The number of BPF instructions in the program.
    pub fn len(&self) -> usize {
        self.program.len()
    }

    /// Whether the program is empty (never true for a validly built filter).
    pub fn is_empty(&self) -> bool {
        self.program.is_empty()
    }

    /// Install the filter on every thread of the calling process.
    ///
    /// Sets `PR_SET_NO_NEW_PRIVS` (required to load a filter without privilege)
    /// then `seccomp(SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, ...)`.
    /// The filter is inherited atomically across `fork`/`clone` and persists
    /// across `execve`, so there is no post-fork installation race.
    ///
    /// # Safety
    ///
    /// Installs process-wide, irreversible kernel state. Call exactly once,
    /// before untrusted application threads start, after the SIGSYS handler is
    /// in place.
    pub unsafe fn install(&mut self) -> io::Result<()> {
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let program = libc::sock_fprog {
            len: u16::try_from(self.program.len())
                .map_err(|_| io::Error::other("seccomp filter too long"))?,
            filter: self.program.as_mut_ptr(),
        };
        let result = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                libc::SECCOMP_FILTER_FLAG_TSYNC,
                ptr::addr_of!(program),
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

const fn stmt(code: u16, value: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k: value,
    }
}

const fn jump(code: u16, value: u32, jump_true: u8, jump_false: u8) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: jump_true,
        jf: jump_false,
        k: value,
    }
}

fn append_gate_check(
    program: &mut Vec<libc::sock_filter>,
    gate: TrustedGate,
    allow_jumps: &mut Vec<usize>,
) {
    program.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_IP_HIGH_OFFSET));
    program.push(jump(BPF_JMP_JEQ_K, (gate.syscall_ip >> 32) as u32, 0, 3));
    program.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_IP_LOW_OFFSET));
    allow_jumps.push(program.len());
    program.push(jump(BPF_JMP_JEQ_K, gate.syscall_ip as u32, 0, 0));
    allow_jumps.push(program.len());
    program.push(jump(BPF_JMP_JEQ_K, gate.return_ip as u32, 0, 0));
}

fn forward_offset(from: usize, to: usize) -> io::Result<u8> {
    let offset = to
        .checked_sub(from + 1)
        .ok_or_else(|| io::Error::other("seccomp filter has a backward jump"))?;
    u8::try_from(offset).map_err(|_| io::Error::other("seccomp filter jump is too long"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(syscall_ip: u64) -> TrustedGate {
        TrustedGate {
            syscall_ip,
            return_ip: syscall_ip + 2,
        }
    }

    fn evaluate(filter: &SeccompFilter, arch: u32, number: i64, ip: u64) -> u32 {
        let mut accumulator = 0;
        let mut pc = 0;
        loop {
            let instruction = &filter.program[pc];
            match instruction.code {
                BPF_LD_W_ABS => {
                    accumulator = match instruction.k {
                        SECCOMP_DATA_ARCH_OFFSET => arch,
                        SECCOMP_DATA_NR_OFFSET => number as u32,
                        SECCOMP_DATA_IP_LOW_OFFSET => ip as u32,
                        SECCOMP_DATA_IP_HIGH_OFFSET => (ip >> 32) as u32,
                        offset => panic!("unexpected seccomp data offset {offset}"),
                    };
                }
                BPF_JMP_JEQ_K => {
                    pc += usize::from(if accumulator == instruction.k {
                        instruction.jt
                    } else {
                        instruction.jf
                    });
                }
                BPF_RET_K => return instruction.k,
                code => panic!("unexpected BPF instruction {code}"),
            }
            pc += 1;
        }
    }

    #[test]
    fn two_gates_split_regular_and_sigreturn_authority() {
        let first = gate(0x1234_0000_1000);
        let second = gate(0x5678_0000_2000);
        let restorer = gate(0x9abc_0000_3000);
        let filter =
            SeccompFilter::for_trusted_gates_with_restorer(first, second, restorer).unwrap();
        assert_eq!(filter.len(), 23);

        for gate in [first, second] {
            for address in [gate.syscall_ip, gate.return_ip] {
                assert_eq!(
                    evaluate(&filter, AUDIT_ARCH_X86_64, libc::SYS_write, address),
                    SECCOMP_RET_ALLOW
                );
                for forbidden in [
                    address - 1,
                    address + 1,
                    address ^ (1 << 32),
                    address as u32 as u64,
                ] {
                    assert_eq!(
                        evaluate(&filter, AUDIT_ARCH_X86_64, libc::SYS_write, forbidden),
                        SECCOMP_RET_TRAP,
                        "{forbidden:#x}"
                    );
                }
                assert_eq!(
                    evaluate(&filter, AUDIT_ARCH_X86_64, libc::SYS_rt_sigreturn, address),
                    SECCOMP_RET_TRAP,
                    "ordinary gate must not authorize rt_sigreturn"
                );
            }
        }

        for address in [restorer.syscall_ip, restorer.return_ip] {
            assert_eq!(
                evaluate(&filter, AUDIT_ARCH_X86_64, libc::SYS_rt_sigreturn, address),
                SECCOMP_RET_ALLOW
            );
            assert_eq!(
                evaluate(&filter, AUDIT_ARCH_X86_64, libc::SYS_write, address),
                SECCOMP_RET_TRAP,
                "restorer gate must not authorize another syscall"
            );
        }

        for address in [
            0,
            restorer.syscall_ip - 1,
            restorer.return_ip + 1,
            restorer.syscall_ip ^ (1 << 32),
            restorer.syscall_ip as u32 as u64,
        ] {
            assert_eq!(
                evaluate(&filter, AUDIT_ARCH_X86_64, libc::SYS_rt_sigreturn, address),
                SECCOMP_RET_TRAP,
                "custom or high-word-alias restorer {address:#x}"
            );
        }
    }

    #[test]
    fn exact_restorer_ip_is_the_documented_trusted_control_flow_boundary() {
        let ordinary = gate(0x1234_0000_1000);
        let restorer = gate(0x9abc_0000_3000);
        let filter = SeccompFilter::for_trusted_gate_with_restorer(ordinary, restorer).unwrap();

        // Exact-IP routing cannot prove how control reached hidden runtime
        // text. A crafted direct jump to this exact instruction is outside the
        // supported trusted-guest model, just like a jump to `ordinary`.
        assert_eq!(
            evaluate(
                &filter,
                AUDIT_ARCH_X86_64,
                libc::SYS_rt_sigreturn,
                restorer.syscall_ip
            ),
            SECCOMP_RET_ALLOW
        );
        assert_eq!(
            evaluate(
                &filter,
                AUDIT_ARCH_X86_64,
                libc::SYS_rt_sigreturn,
                restorer.syscall_ip - 1
            ),
            SECCOMP_RET_TRAP
        );
    }

    #[test]
    fn runtime_and_libc_restorers_are_exact_without_widening_other_sites() {
        let first = gate(0x1234_0000_1000);
        let second = gate(0x5678_0000_2000);
        let runtime = gate(0x9abc_0000_3000);
        let libc = gate(0xdef0_0000_4000);
        let filter =
            SeccompFilter::for_trusted_gates_with_restorers(first, second, &[runtime, libc])
                .unwrap();
        assert_eq!(filter.len(), 28);

        for restorer in [runtime, libc] {
            for address in [restorer.syscall_ip, restorer.return_ip] {
                assert_eq!(
                    evaluate(&filter, AUDIT_ARCH_X86_64, libc::SYS_rt_sigreturn, address,),
                    SECCOMP_RET_ALLOW,
                    "exact restorer {address:#x}"
                );
                assert_eq!(
                    evaluate(&filter, AUDIT_ARCH_X86_64, libc::SYS_write, address),
                    SECCOMP_RET_TRAP,
                    "restorer {address:#x} must not authorize ordinary syscalls"
                );
            }
        }

        for forbidden in [
            0,
            runtime.syscall_ip - 1,
            runtime.return_ip + 1,
            libc.syscall_ip - 1,
            libc.return_ip + 1,
            runtime.syscall_ip ^ (1 << 32),
            libc.syscall_ip ^ (1 << 32),
            runtime.syscall_ip as u32 as u64,
            libc.syscall_ip as u32 as u64,
        ] {
            assert_eq!(
                evaluate(
                    &filter,
                    AUDIT_ARCH_X86_64,
                    libc::SYS_rt_sigreturn,
                    forbidden,
                ),
                SECCOMP_RET_TRAP,
                "arbitrary/custom restorer {forbidden:#x} must remain trapped"
            );
        }
        for ordinary in [first, second] {
            assert_eq!(
                evaluate(
                    &filter,
                    AUDIT_ARCH_X86_64,
                    libc::SYS_rt_sigreturn,
                    ordinary.return_ip,
                ),
                SECCOMP_RET_TRAP,
                "ordinary gate must not gain rt_sigreturn authority"
            );
        }
        assert!(SeccompFilter::for_trusted_gates_with_restorers(first, second, &[]).is_err());
        assert!(
            SeccompFilter::for_trusted_gates_with_restorers(first, second, &[runtime, runtime])
                .is_err()
        );
    }

    #[test]
    fn builds_fixed_length_program_for_valid_gate() {
        let filter = SeccompFilter::for_trusted_gate_with_restorer(
            gate(0x5555_0000_1000),
            gate(0x7777_0000_2000),
        )
        .unwrap();
        assert_eq!(filter.len(), 18);
        assert!(!filter.is_empty());
    }

    #[test]
    fn architecture_mismatch_kills_before_any_gate() {
        let ordinary = gate(0x1234_0000_1000);
        let restorer = gate(0x9abc_0000_3000);
        let filter = SeccompFilter::for_trusted_gate_with_restorer(ordinary, restorer).unwrap();
        for (number, address) in [
            (libc::SYS_write, ordinary.syscall_ip),
            (libc::SYS_rt_sigreturn, restorer.syscall_ip),
            (libc::SYS_rt_sigreturn, 0),
        ] {
            assert_eq!(
                evaluate(&filter, 0x4000_0003, number, address),
                SECCOMP_RET_KILL_PROCESS
            );
        }
    }

    #[test]
    fn constructors_reject_malformed_or_overlapping_gates() {
        let ordinary = gate(0x1234_0000_1000);
        let second = gate(0x5678_0000_2000);
        let restorer = gate(0x9abc_0000_3000);
        let malformed = [
            TrustedGate {
                syscall_ip: 0,
                return_ip: 2,
            },
            TrustedGate {
                syscall_ip: 0x1000,
                return_ip: 0x1001,
            },
            TrustedGate {
                syscall_ip: 0x1000,
                return_ip: 0x1003,
            },
            TrustedGate {
                syscall_ip: 0x0000_0000_ffff_fffe,
                return_ip: 0x0000_0001_0000_0000,
            },
            TrustedGate {
                syscall_ip: u64::MAX,
                return_ip: 1,
            },
        ];
        for invalid in malformed {
            assert!(SeccompFilter::for_trusted_gate_with_restorer(invalid, restorer).is_err());
            assert!(SeccompFilter::for_trusted_gate_with_restorer(ordinary, invalid).is_err());
        }

        assert!(SeccompFilter::for_trusted_gate_with_restorer(ordinary, ordinary).is_err());
        assert!(
            SeccompFilter::for_trusted_gates_with_restorer(ordinary, ordinary, restorer).is_err()
        );
        assert!(
            SeccompFilter::for_trusted_gates_with_restorer(ordinary, second, ordinary).is_err()
        );
        assert!(
            SeccompFilter::for_trusted_gates_with_restorer(
                ordinary,
                second,
                gate(ordinary.return_ip),
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_constructors_keep_unrestricted_sigreturn_contract() {
        let first = gate(0x1234_0000_1000);
        let second = gate(0x5678_0000_2000);
        let one = SeccompFilter::for_trusted_gate(first).unwrap();
        let two = SeccompFilter::for_trusted_gates(first, second).unwrap();
        assert_eq!(one.len(), 12);
        assert_eq!(two.len(), 17);
        for filter in [&one, &two] {
            assert_eq!(
                evaluate(filter, AUDIT_ARCH_X86_64, libc::SYS_rt_sigreturn, 0),
                SECCOMP_RET_ALLOW
            );
        }
    }

    #[test]
    fn legacy_two_gates_keep_exact_full_address_and_architecture_contract() {
        let first = TrustedGate {
            syscall_ip: 0x1234_0000_1000,
            return_ip: 0x1234_0000_2000,
        };
        let second = TrustedGate {
            syscall_ip: 0x5678_0000_3000,
            return_ip: 0x5678_0000_4000,
        };
        let filter = SeccompFilter::for_trusted_gates(first, second).unwrap();

        for gate in [first, second] {
            for address in [gate.syscall_ip, gate.return_ip] {
                assert_eq!(
                    evaluate(&filter, AUDIT_ARCH_X86_64, libc::SYS_write, address),
                    SECCOMP_RET_ALLOW
                );
                for forbidden in [
                    address - 1,
                    address + 1,
                    address ^ (1 << 32),
                    address as u32 as u64,
                ] {
                    assert_eq!(
                        evaluate(&filter, AUDIT_ARCH_X86_64, libc::SYS_write, forbidden),
                        SECCOMP_RET_TRAP,
                        "{forbidden:#x}"
                    );
                }
                assert_eq!(
                    evaluate(&filter, 0x4000_0003, libc::SYS_write, address),
                    SECCOMP_RET_KILL_PROCESS
                );
            }
        }

        for cross_gate_alias in [0x1234_0000_3000, 0x5678_0000_1000] {
            assert_eq!(
                evaluate(
                    &filter,
                    AUDIT_ARCH_X86_64,
                    libc::SYS_write,
                    cross_gate_alias,
                ),
                SECCOMP_RET_TRAP,
                "a low-word alias from the other gate must not be trusted: {cross_gate_alias:#x}"
            );
        }
        assert_eq!(
            evaluate(&filter, 0x4000_0003, libc::SYS_rt_sigreturn, 0),
            SECCOMP_RET_KILL_PROCESS,
            "the unrestricted legacy rt_sigreturn exception is still architecture-scoped"
        );
    }

    #[test]
    fn legacy_constructors_accept_every_historical_same_high_word_pair() {
        let null_start = TrustedGate {
            syscall_ip: 0,
            return_ip: 0xffff,
        };
        let non_adjacent = TrustedGate {
            syscall_ip: 0x1234_0000_1000,
            return_ip: 0x1234_ffff_f000,
        };

        assert!(SeccompFilter::for_trusted_gate(null_start).is_ok());
        assert!(SeccompFilter::for_trusted_gate(non_adjacent).is_ok());
        assert!(SeccompFilter::for_trusted_gates(null_start, non_adjacent).is_ok());
    }

    #[test]
    fn legacy_constructors_reject_only_unrepresentable_cross_boundary_pairs() {
        let valid = gate(0x1234_0000_1000);
        let crossing = TrustedGate {
            syscall_ip: 0x0000_0000_ffff_fff8,
            return_ip: 0x0000_0001_0000_0000,
        };

        assert!(SeccompFilter::for_trusted_gate(crossing).is_err());
        assert!(SeccompFilter::for_trusted_gates(crossing, valid).is_err());
        assert!(SeccompFilter::for_trusted_gates(valid, crossing).is_err());
    }

    #[test]
    fn legacy_filter_retains_kill_trap_and_allow_terminals() {
        let filter = SeccompFilter::for_trusted_gate(TrustedGate {
            syscall_ip: 0x1234_5678,
            return_ip: 0x1234_9abc,
        })
        .unwrap();
        let returns: Vec<u32> = filter
            .program
            .iter()
            .filter(|instruction| instruction.code == BPF_RET_K)
            .map(|instruction| instruction.k)
            .collect();
        assert!(returns.contains(&SECCOMP_RET_KILL_PROCESS));
        assert!(returns.contains(&SECCOMP_RET_TRAP));
        assert!(returns.contains(&SECCOMP_RET_ALLOW));
    }
}
