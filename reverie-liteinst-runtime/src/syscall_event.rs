//! Native SUD entry classification, separate from kernel effect permission.

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Metadata {
    pub(crate) signal: i32,
    pub(crate) errno: i32,
    pub(crate) code: i32,
    padding: i32,
    pub(crate) call_address: u64,
    pub(crate) number: i32,
    pub(crate) arch: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SyscallEvent {
    pub(crate) number: i64,
    pub(crate) site: u64,
    pub(crate) resume: u64,
}

/// Whether the number is a well-formed native x86-64 syscall *number
/// encoding*.
///
/// Part of frame shape, not of execution policy, and not a table lookup: it
/// says nothing about whether Linux implements the number. A negative value,
/// or one carrying the x32 bit, is a malformed request under
/// `AUDIT_ARCH_X86_64` whatever operation its low bits resemble.
/// [`SyscallEvent::decode`] applies it, so it rejects before any question of
/// support is asked.
fn native_number(number: i32) -> bool {
    number >= 0 && number & 0x4000_0000 == 0
}

/// Finite returning effects and the unchanged private fixture admission set.
pub(crate) fn backed_returning(number: i64) -> bool {
    matches!(
        number,
        libc::SYS_getpid | libc::SYS_read | libc::SYS_openat | libc::SYS_fstat | libc::SYS_close
    )
}

/// Finite owned kernel injection coverage, not an observation subscription set.
pub(crate) fn injectable(number: i64) -> bool {
    backed_returning(number)
        || matches!(
            number,
            libc::SYS_gettid
                | libc::SYS_getppid
                | libc::SYS_access
                | libc::SYS_unlink
                | libc::SYS_mkdir
                | libc::SYS_rename
                | libc::SYS_renameat
                | libc::SYS_rmdir
                | libc::SYS_newfstatat
                | libc::SYS_inotify_init1
                | libc::SYS_inotify_add_watch
                | libc::SYS_pipe2
                | libc::SYS_getcwd
                | libc::SYS_statfs
                | libc::SYS_getdents64
                | libc::SYS_socket
                | libc::SYS_bind
                | libc::SYS_getsockname
                | libc::SYS_sendto
                | libc::SYS_recvfrom
                | libc::SYS_dup
                | libc::SYS_linkat
                | libc::SYS_unlinkat
                | libc::SYS_symlink
                | libc::SYS_pread64
                | libc::SYS_set_tid_address
                | libc::SYS_set_robust_list
                | libc::SYS_lseek
                | libc::SYS_fsync
                | libc::SYS_ftruncate
                | libc::SYS_write
                | libc::SYS_writev
                | libc::SYS_pwritev
                | libc::SYS_pwrite64
                | libc::SYS_readv
                | libc::SYS_preadv
        )
}

pub(crate) fn observable(number: i64) -> bool {
    i32::try_from(number).is_ok_and(native_number)
}

impl SyscallEvent {
    /// Decodes a structurally valid guest SIGSYS syscall frame, asking nothing
    /// about whether the operation is supported.
    ///
    /// Every original shape invariant is applied here and nowhere else: SIGSYS
    /// with `si_code == 2`, which for this backend is `SYS_USER_DISPATCH`, the
    /// syscall-user-dispatch provenance — not `SYS_SECCOMP`, which is 1 — then
    /// `AUDIT_ARCH_X86_64`, a well-formed native syscall number encoding, the
    /// reported call address agreeing with the resume address, a
    /// nonzero site two bytes back, both addresses inside one mapped range and
    /// below the canonical boundary, literal `0f 05` at the site, and `RCX`
    /// holding the continuation the `syscall` instruction placed there.
    ///
    /// Returning `Some` permits neither kernel injection nor a guest return.
    pub(crate) fn decode(
        info: Metadata,
        registers: &[libc::greg_t; 23],
        bytes: &[u8],
        mapping: &std::ops::Range<u64>,
    ) -> Option<Self> {
        let resume = registers[libc::REG_RIP as usize] as u64;
        let site = resume.checked_sub(2)?;
        (info.signal == libc::SIGSYS
            && info.code == 2
            && info.arch == 0xc000_003e
            && info.call_address == resume
            && native_number(info.number)
            && site != 0
            && resume < 1 << 47
            && mapping.contains(&site)
            && mapping.contains(&resume)
            && bytes.starts_with(&[0x0f, 0x05])
            && registers[libc::REG_RCX as usize] as u64 == resume)
            .then_some(Self {
                number: i64::from(info.number),
                site,
                resume,
            })
    }

    /// Admits an entry for owned dispatch; ownership and one-use completion
    /// remain checked by the capture caller. Kernel effects are gated separately.
    pub(crate) fn admit(
        info: Metadata,
        registers: &[libc::greg_t; 23],
        bytes: &[u8],
        mapping: &std::ops::Range<u64>,
    ) -> Option<Self> {
        Self::decode(info, registers, bytes, mapping)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measured_inotify_init1_admission() {
        assert!(injectable(libc::SYS_inotify_init1));
    }

    #[test]
    fn measured_pipe2_admission() {
        assert!(injectable(libc::SYS_pipe2));
    }

    #[test]
    fn measured_getcwd_admission() {
        assert!(injectable(libc::SYS_getcwd));
    }

    #[test]
    fn measured_socket_admission() {
        assert!(injectable(libc::SYS_socket));
    }

    #[test]
    fn measured_linkat_admission() {
        assert!(injectable(libc::SYS_linkat));
    }

    #[test]
    fn fixture_followup_admission_preserves_unrelated_boundaries() {
        for number in [
            libc::SYS_inotify_add_watch,
            libc::SYS_statfs,
            libc::SYS_getdents64,
            libc::SYS_bind,
            libc::SYS_getsockname,
            libc::SYS_sendto,
            libc::SYS_recvfrom,
            libc::SYS_dup,
            libc::SYS_unlinkat,
            libc::SYS_symlink,
        ] {
            assert!(injectable(number), "missing fixture followup: {number}");
            assert!(!backed_returning(number));
            assert!(!injectable(number | 0x4000_0000));
        }
        for number in [
            libc::SYS_inotify_rm_watch,
            libc::SYS_pipe,
            libc::SYS_socketpair,
            libc::SYS_connect,
            libc::SYS_accept,
            libc::SYS_dup2,
            libc::SYS_fcntl,
            libc::SYS_ioctl,
            libc::SYS_symlinkat,
            libc::SYS_signalfd4,
            libc::SYS_rt_sigaction,
            libc::SYS_tgkill,
        ] {
            assert!(!injectable(number), "unrelated injection: {number}");
        }
    }

    #[test]
    fn directory_effect_admission_preserves_neighbor_boundaries() {
        for number in [
            libc::SYS_mkdir,
            libc::SYS_rename,
            libc::SYS_renameat,
            libc::SYS_rmdir,
            libc::SYS_unlinkat,
        ] {
            assert!(observable(number));
            assert!(injectable(number), "missing directory injection: {number}");
            assert!(!backed_returning(number));
            assert!(!injectable(number | 0x4000_0000));
        }
        for number in [
            libc::SYS_mkdirat,
            libc::SYS_renameat2,
            libc::SYS_chdir,
            libc::SYS_chroot,
            libc::SYS_execve,
            -1,
        ] {
            assert!(!injectable(number), "unrelated injection: {number}");
        }
    }

    #[test]
    fn nr_is_from_siginfo_not_saved_rax_and_pc_is_already_continuation() {
        let info = Metadata {
            signal: libc::SIGSYS,
            errno: 0,
            code: 2,
            padding: 0,
            call_address: 0x4002,
            number: libc::SYS_getpid as i32,
            arch: 0xc000_003e,
        };
        let mut registers = [0; 23];
        registers[libc::REG_RIP as usize] = 0x4002;
        registers[libc::REG_RCX as usize] = 0x4002;
        registers[libc::REG_RAX as usize] = -1234;
        let event =
            SyscallEvent::admit(info, &registers, &[0x0f, 0x05], &(0x4000..0x5000)).unwrap();
        assert_eq!(
            event,
            SyscallEvent {
                number: libc::SYS_getpid,
                site: 0x4000,
                resume: 0x4002
            }
        );
        for invalid in [
            Metadata {
                arch: 0x4000_0003,
                ..info
            },
            Metadata { code: 1, ..info },
            Metadata {
                number: 0x4000_0027,
                ..info
            },
            Metadata {
                call_address: 0x4000,
                ..info
            },
        ] {
            assert!(
                SyscallEvent::admit(invalid, &registers, &[0x0f, 0x05], &(0x4000..0x5000))
                    .is_none()
            );
        }
        let exec = Metadata {
            number: libc::SYS_execve as i32,
            ..info
        };
        let observed =
            SyscallEvent::admit(exec, &registers, &[0x0f, 0x05], &(0x4000..0x5000)).unwrap();
        assert_eq!(observed.number, libc::SYS_execve);
        assert!(!injectable(observed.number));
        assert!(SyscallEvent::admit(info, &registers, &[0x0f, 0x34], &(0x4000..0x5000)).is_none());
        assert!(SyscallEvent::admit(info, &registers, &[0x0f, 0x05], &(0x4000..0x4002)).is_none());
    }

    /// The two classes are separate questions and the wider one must not leak
    /// into execution admission.
    #[test]
    fn backed_returning_is_the_fd_closure_and_excludes_injection_only_numbers() {
        for number in [
            libc::SYS_getpid,
            libc::SYS_read,
            libc::SYS_openat,
            libc::SYS_fstat,
            libc::SYS_close,
        ] {
            assert!(backed_returning(number));
            assert!(injectable(number));
        }
        for number in [
            libc::SYS_gettid,
            libc::SYS_getppid,
            libc::SYS_pread64,
            libc::SYS_set_tid_address,
            libc::SYS_set_robust_list,
            libc::SYS_lseek,
            libc::SYS_write,
            libc::SYS_writev,
            libc::SYS_pwrite64,
            libc::SYS_readv,
            libc::SYS_preadv,
        ] {
            assert!(injectable(number), "{number} is an auxiliary injection");
            assert!(!backed_returning(number), "{number} is never staged");
        }
        for number in [
            libc::SYS_fcntl,
            libc::SYS_statx,
            libc::SYS_open,
            libc::SYS_mmap,
            libc::SYS_mprotect,
            libc::SYS_brk,
            libc::SYS_execve,
            libc::SYS_exit_group,
            libc::SYS_rt_sigaction,
        ] {
            assert!(!backed_returning(number), "{number} must not be backed");
            assert!(!injectable(number), "{number} must not be injectable");
        }
    }

    #[test]
    fn access_effect_does_not_expand_fixture_or_observation_admission() {
        assert!(observable(libc::SYS_access));
        assert!(injectable(libc::SYS_access));
        assert!(!backed_returning(libc::SYS_access));
        for number in [
            libc::SYS_access | 0x4000_0000,
            -1,
            libc::SYS_chdir,
            libc::SYS_setuid,
            libc::SYS_arch_prctl,
            libc::SYS_execve,
            libc::SYS_rt_sigaction,
            libc::SYS_open,
        ] {
            assert!(!injectable(number));
        }
    }

    #[test]
    fn pread_effect_admission_preserves_other_effect_boundaries() {
        assert!(observable(libc::SYS_pread64));
        assert!(injectable(libc::SYS_pread64));
        assert!(!backed_returning(libc::SYS_pread64));
        for number in [
            libc::SYS_pread64 | 0x4000_0000,
            -1,
            libc::SYS_preadv2,
            libc::SYS_fdatasync,
            libc::SYS_pwritev2,
            libc::SYS_truncate,
            libc::SYS_mmap,
            libc::SYS_execve,
            libc::SYS_arch_prctl,
        ] {
            assert!(!injectable(number));
        }
    }

    fn shape(number: i32) -> (Metadata, [libc::greg_t; 23], std::ops::Range<u64>) {
        let mut registers = [0 as libc::greg_t; 23];
        registers[libc::REG_RIP as usize] = 0x4002;
        registers[libc::REG_RCX as usize] = 0x4002;
        let info = Metadata {
            signal: libc::SIGSYS,
            errno: 0,
            code: 2,
            padding: 0,
            call_address: 0x4002,
            number,
            arch: 0xc000_003e,
        };
        (info, registers, 0x4000..0x5000)
    }

    #[test]
    fn native_unsupported_effects_are_observed_without_injection_permission() {
        for number in [
            libc::SYS_pwritev2,
            libc::SYS_execve,
            libc::SYS_mmap,
            libc::SYS_mprotect,
            libc::SYS_munmap,
            libc::SYS_brk,
            libc::SYS_rt_sigaction,
            libc::SYS_rt_sigreturn,
            libc::SYS_clone,
            libc::SYS_exit_group,
            libc::SYS_preadv2,
        ] {
            let (info, registers, mapping) = shape(number as i32);
            let decoded = SyscallEvent::decode(info, &registers, &[0x0f, 0x05], &mapping);
            assert_eq!(
                decoded,
                Some(SyscallEvent {
                    number,
                    site: 0x4000,
                    resume: 0x4002,
                }),
                "{number} is a valid frame shape"
            );
            assert_eq!(
                SyscallEvent::admit(info, &registers, &[0x0f, 0x05], &mapping),
                decoded
            );
            assert!(!injectable(number));
        }
    }

    #[test]
    fn ordinary_file_effects_preserve_native_shape_and_fixture_refusal() {
        for number in [
            libc::SYS_unlink,
            libc::SYS_pwritev,
            libc::SYS_fsync,
            libc::SYS_ftruncate,
            libc::SYS_newfstatat,
            libc::SYS_lseek,
            libc::SYS_write,
            libc::SYS_writev,
            libc::SYS_pwrite64,
            libc::SYS_readv,
            libc::SYS_preadv,
        ] {
            let (info, registers, mapping) = shape(number as i32);
            let expected = Some(SyscallEvent {
                number,
                site: 0x4000,
                resume: 0x4002,
            });
            assert_eq!(
                SyscallEvent::decode(info, &registers, &[0x0f, 0x05], &mapping),
                expected
            );
            assert_eq!(
                SyscallEvent::admit(info, &registers, &[0x0f, 0x05], &mapping),
                expected
            );
            assert!(injectable(number));
            assert!(!backed_returning(number));
        }
    }

    /// An x32 request is a shape failure, not an unsupported operation: it must
    /// be rejected by `decode` even though its low bits name a backed number.
    #[test]
    fn x32_and_negative_numbers_fail_shape_not_merely_execution() {
        for number in [
            libc::SYS_read as i32 | 0x4000_0000,
            libc::SYS_openat as i32 | 0x4000_0000,
            0x4000_0000,
            -1,
            i32::MIN,
        ] {
            let (info, registers, mapping) = shape(number);
            assert!(
                SyscallEvent::decode(info, &registers, &[0x0f, 0x05], &mapping).is_none(),
                "{number} must fail shape decoding"
            );
            assert!(SyscallEvent::admit(info, &registers, &[0x0f, 0x05], &mapping).is_none());
        }
    }

    /// Every original shape invariant still rejects, and a backed number buys no
    /// exemption from any of them.
    #[test]
    fn malformed_shapes_are_refused_even_for_backed_operations() {
        for number in [
            libc::SYS_getpid,
            libc::SYS_read,
            libc::SYS_openat,
            libc::SYS_fstat,
            libc::SYS_close,
            libc::SYS_execve,
            libc::SYS_rt_sigreturn,
            0x3fff_ffff,
        ] {
            let (info, registers, mapping) = shape(number as i32);
            assert!(SyscallEvent::decode(info, &registers, &[0x0f, 0x05], &mapping).is_some());
            assert!(SyscallEvent::admit(info, &registers, &[0x0f, 0x05], &mapping).is_some());

            let mut mismatched_rcx = registers;
            mismatched_rcx[libc::REG_RCX as usize] = 0x4004;
            let mut unmapped = registers;
            unmapped[libc::REG_RIP as usize] = 0x9002;
            let mut noncanonical = registers;
            noncanonical[libc::REG_RIP as usize] = 1 << 47;
            noncanonical[libc::REG_RCX as usize] = 1 << 47;
            let broken = [
                (info, registers, [0x0f, 0x34], mapping.clone()),
                (info, registers, [0x0f, 0x05], 0x4000..0x4002),
                (
                    Metadata {
                        signal: libc::SIGTRAP,
                        ..info
                    },
                    registers,
                    [0x0f, 0x05],
                    mapping.clone(),
                ),
                (
                    Metadata { code: 1, ..info },
                    registers,
                    [0x0f, 0x05],
                    mapping.clone(),
                ),
                (
                    Metadata {
                        arch: 0x4000_003e,
                        ..info
                    },
                    registers,
                    [0x0f, 0x05],
                    mapping.clone(),
                ),
                (
                    Metadata {
                        call_address: 0x4004,
                        ..info
                    },
                    registers,
                    [0x0f, 0x05],
                    mapping.clone(),
                ),
                (info, mismatched_rcx, [0x0f, 0x05], mapping.clone()),
                (
                    Metadata {
                        call_address: 0x9002,
                        ..info
                    },
                    unmapped,
                    [0x0f, 0x05],
                    mapping.clone(),
                ),
                (
                    Metadata {
                        call_address: 1 << 47,
                        ..info
                    },
                    noncanonical,
                    [0x0f, 0x05],
                    (1 << 47) - 2..(1 << 47) + 8,
                ),
            ];

            for (index, (info, registers, bytes, mapping)) in broken.into_iter().enumerate() {
                assert!(
                    SyscallEvent::decode(info, &registers, &bytes, &mapping).is_none(),
                    "{number}: malformed shape {index} must fail decoding"
                );
                assert!(
                    SyscallEvent::admit(info, &registers, &bytes, &mapping).is_none(),
                    "{number}: malformed shape {index} must fail admission"
                );
            }
        }
    }
}
