/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

// Native prerequisites only, using the existing actual TRACEME child, original
// consuming wait, and ExactChildGuard. No fabricated Event or receipt. These
// tests must not run until Main separately admits this exact fixture/custody.
mod control_stop_native_tests {
    use super::*;

    #[cfg(target_arch = "x86_64")]
    fn registers(regs: &crate::Regs) -> [u64; 27] {
        [
            regs.r15,
            regs.r14,
            regs.r13,
            regs.r12,
            regs.rbp,
            regs.rbx,
            regs.r11,
            regs.r10,
            regs.r9,
            regs.r8,
            regs.rax,
            regs.rcx,
            regs.rdx,
            regs.rsi,
            regs.rdi,
            regs.orig_rax,
            regs.rip,
            regs.cs,
            regs.eflags,
            regs.rsp,
            regs.ss,
            regs.fs_base,
            regs.gs_base,
            regs.ds,
            regs.es,
            regs.fs,
            regs.gs,
        ]
    }

    #[test]
    fn genuine_wait_only_and_no_duplicate_issuance() {
        let (_cleanup, stopped) = child_stop();
        assert!(matches!(
            Stopped::new_unchecked(stopped.pid()).control_stop(),
            Err(Errno::ENODATA)
        ));
        let raw = Wait::from_raw(stopped.pid(), (libc::SIGSTOP << 8) | 0x7f)
            .unwrap()
            .assume_stopped()
            .0;
        assert!(raw.control_stop().is_err());
        let stop = stopped.control_stop().unwrap();
        stop.validate_current().unwrap();
        assert!(matches!(stopped.control_stop(), Err(Errno::EALREADY)));
        drop(stop);
        assert!(matches!(stopped.control_stop(), Err(Errno::EALREADY)));
        finish(stopped);
    }

    #[test]
    fn successful_register_write_advances_only_distinct_authority() {
        let (_cleanup, stopped) = child_stop();
        let legacy = stopped.source_stop().unwrap();
        let stop = stopped.control_stop().unwrap();
        let regs = stopped.getregs().unwrap();
        let changed = {
            let mut changed = regs;
            // The compiled target is x86_64: demonstrate a real
            // register mutation and restore it before any guest instruction.
            #[cfg(target_arch = "x86_64")]
            {
                changed.r15 ^= 1;
            }
            #[cfg(target_arch = "aarch64")]
            {
                changed.regs[15] ^= 1;
            }
            changed
        };
        let stop = stop.setregs(&changed).unwrap();
        stop.validate_current().unwrap();
        #[cfg(target_arch = "x86_64")]
        assert_eq!(registers(&stopped.getregs().unwrap()), registers(&changed));
        assert_eq!(legacy.validate_current(), Err(Errno::ESTALE));
        assert!(legacy.begin_acquisition().is_err());
        assert!(stopped.source_stop().is_err());
        assert!(stopped.control_stop().is_err());
        // A second *actual completed operation* advances the new authority;
        // merely asking the old Stopped for another witness never does.
        let stop = stop.setregs(&regs).unwrap();
        stop.validate_current().unwrap();
        #[cfg(target_arch = "x86_64")]
        assert_eq!(registers(&stopped.getregs().unwrap()), registers(&regs));
        assert_eq!(legacy.validate_current(), Err(Errno::ESTALE));
        finish(stopped);
        assert_eq!(stop.validate_current(), Err(Errno::ESTALE));
    }

    #[test]
    fn ordinary_alias_mutation_cannot_be_renewed() {
        let (_cleanup, stopped) = child_stop();
        let stop = stopped.control_stop().unwrap();
        let regs = stopped.getregs().unwrap();
        Stopped::new_unchecked(stopped.pid())
            .setregs(&regs)
            .unwrap();
        assert_eq!(stop.validate_current(), Err(Errno::ESTALE));
        assert!(matches!(
            stop.setregs(&regs),
            Err(crate::Error::Errno(Errno::ESTALE))
        ));
        assert!(stopped.control_stop().is_err());
        finish(stopped);
    }

    #[test]
    fn abandoned_operation_cannot_manufacture_completion() {
        let (_cleanup, stopped) = child_stop();
        let legacy = stopped.source_stop().unwrap();
        let stop = stopped.control_stop().unwrap();
        control_stop::abandon_register_write(stop).unwrap();
        assert_eq!(legacy.validate_current(), Err(Errno::ESTALE));
        assert!(stopped.control_stop().is_err());
        assert!(stopped.source_stop().is_err());
        // Admission was released, but no new stopped witness was published.
        finish(stopped);
    }

    #[test]
    fn acquisition_exclusion_applies_to_new_control_operation() {
        let (_cleanup, stopped) = child_stop();
        let legacy = stopped.source_stop().unwrap();
        let stop = stopped.control_stop().unwrap();
        let acquisition = legacy.begin_acquisition().unwrap();
        let regs = stopped.getregs().unwrap();
        assert!(matches!(
            stop.setregs(&regs),
            Err(crate::Error::Errno(Errno::EBUSY))
        ));
        legacy.validate_current().unwrap();
        acquisition.finish_binding();
        finish(stopped);
    }

    #[test]
    fn original_ptracer_thread_is_required() {
        let (_cleanup, stopped) = child_stop();
        let stop = stopped.control_stop().unwrap();
        let stop = std::thread::spawn(move || {
            assert_eq!(stop.validate_current(), Err(Errno::EPERM));
            stop
        })
        .join()
        .unwrap();
        stop.validate_current().unwrap();
        finish(stopped);
        assert_eq!(stop.validate_current(), Err(Errno::ESTALE));
    }

    #[cfg(target_arch = "x86_64")]
    fn change_field(r: &mut crate::Regs, index: usize) {
        let fields = [
            &mut r.r15,
            &mut r.r14,
            &mut r.r13,
            &mut r.r12,
            &mut r.rbp,
            &mut r.rbx,
            &mut r.r11,
            &mut r.r10,
            &mut r.r9,
            &mut r.r8,
            &mut r.rax,
            &mut r.rcx,
            &mut r.rdx,
            &mut r.rsi,
            &mut r.rdi,
            &mut r.orig_rax,
            &mut r.rip,
            &mut r.cs,
            &mut r.eflags,
            &mut r.rsp,
            &mut r.ss,
            &mut r.fs_base,
            &mut r.gs_base,
            &mut r.ds,
            &mut r.es,
            &mut r.fs,
            &mut r.gs,
        ];
        *fields.into_iter().nth(index).unwrap() ^= 1;
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn checked_register_write_reads_back_and_never_renews_legacy_source() {
        let (_cleanup, stopped) = child_stop();
        let legacy = stopped.source_stop().unwrap();
        let stop = stopped.control_stop().unwrap();
        let revision = stop.control_revision();
        let original = stopped.getregs().unwrap();
        let mut desired = original;
        desired.r15 ^= 1;
        let stop = stop.setregs_checked(&original, &desired).unwrap();
        assert_eq!(stop.control_revision(), revision + 1);
        stop.validate_current().unwrap();
        assert!(ControlStop::registers_equal(
            &stopped.getregs().unwrap(),
            &desired
        ));
        assert_eq!(legacy.validate_current(), Err(Errno::ESTALE));
        assert!(stopped.source_stop().is_err());
        assert!(stopped.control_stop().is_err());
        let stop = stop.setregs_checked(&desired, &original).unwrap();
        assert_eq!(stop.control_revision(), revision + 2);
        assert!(ControlStop::registers_equal(
            &stopped.getregs().unwrap(),
            &original
        ));
        assert_eq!(legacy.validate_current(), Err(Errno::ESTALE));
        finish(stopped);
        assert_eq!(stop.validate_current(), Err(Errno::ESTALE));
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn checked_register_write_refuses_each_of_27_wrong_exit_fields_before_write() {
        for field in 0..27 {
            let (cleanup, stopped) = child_stop();
            let legacy = stopped.source_stop().unwrap();
            let stop = stopped.control_stop().unwrap();
            let original = stopped.getregs().unwrap();
            let mut expected = original;
            change_field(&mut expected, field);
            assert!(!ControlStop::registers_equal(&expected, &original));
            let result = stop.setregs_checked(&expected, &original);
            let after = stopped.getregs().unwrap();
            let legacy_stale = legacy.validate_current();
            let no_new_stop = stopped.control_stop().is_err();
            cleanup
                .cleanup()
                .expect("original child must be killed and reaped");
            assert!(
                matches!(result, Err(crate::Error::Errno(Errno::ESTALE))),
                "field{field}"
            );
            assert!(
                ControlStop::registers_equal(&after, &original),
                "field{field}"
            );
            assert_eq!(legacy_stale, Err(Errno::ESTALE));
            assert!(no_new_stop);
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn checked_register_write_refuses_short_real_pre_and_post_replies() {
        for phase in [0, 1] {
            let (cleanup, stopped) = child_stop();
            let legacy = stopped.source_stop().unwrap();
            let stop = stopped.control_stop().unwrap();
            let original = stopped.getregs().unwrap();
            let mut desired = original;
            desired.r15 ^= 1;
            // Only the returned length of an actual successful GETREGSET is
            // shortened. This cannot fabricate a positive register receipt.
            control_stop::shorten_checked_reply_for_test(Some(phase));
            let result = stop.setregs_checked(&original, &desired);
            control_stop::shorten_checked_reply_for_test(None);
            let after = stopped.getregs().unwrap();
            let legacy_stale = legacy.validate_current();
            let no_new_stop = stopped.control_stop().is_err();
            cleanup
                .cleanup()
                .expect("original child must be killed and reaped");
            assert!(matches!(result, Err(crate::Error::Errno(Errno::EPROTO))));
            assert!(ControlStop::registers_equal(
                &after,
                if phase == 0 { &original } else { &desired }
            ));
            assert_eq!(legacy_stale, Err(Errno::ESTALE));
            assert!(no_new_stop);
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn checked_register_write_requires_actual_readback_not_only_successful_set() {
        let (cleanup, stopped) = child_stop();
        let stop = stopped.control_stop().unwrap();
        let original = stopped.getregs().unwrap();
        let mut desired = original;
        // Linux preserves the privileged IF bit in ptrace's EFLAGS setter.
        // SETREGSET succeeds but its full readback must reject this demand.
        desired.eflags ^= 1 << 9;
        let result = stop.setregs_checked(&original, &desired);
        let after = stopped.getregs().unwrap();
        let no_new_stop = stopped.control_stop().is_err();
        cleanup
            .cleanup()
            .expect("original child must be killed and reaped");
        assert!(matches!(result, Err(crate::Error::Errno(Errno::EPROTO))));
        assert!(!ControlStop::registers_equal(&after, &desired));
        assert_eq!(after.eflags & (1 << 9), original.eflags & (1 << 9));
        assert!(no_new_stop);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn checked_register_write_refuses_changed_then_restored_alias() {
        let (cleanup, stopped) = child_stop();
        let stop = stopped.control_stop().unwrap();
        let original = stopped.getregs().unwrap();
        let mut changed = original;
        changed.r15 ^= 1;
        stopped.setregs(&changed).unwrap();
        stopped.setregs(&original).unwrap();
        assert!(ControlStop::registers_equal(
            &stopped.getregs().unwrap(),
            &original
        ));
        let result = stop.setregs_checked(&original, &original);
        let no_new_stop = stopped.control_stop().is_err();
        cleanup
            .cleanup()
            .expect("original child must be killed and reaped");
        assert!(matches!(result, Err(crate::Error::Errno(Errno::ESTALE))));
        assert!(no_new_stop);
    }
}
