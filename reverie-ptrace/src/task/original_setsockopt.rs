//! Preserve one original setsockopt entry when only optval is substituted.
//! Selection is not source authority or permission to resume a numeric TID.
use super::*;

pub(super) struct OriginalSetsockoptEntry {
    owner: TerminalCleanup,
    entry: safeptrace::SyscallEntry,
}

pub(super) fn select_optval_rewrite(
    pending: Option<(Sysno, SyscallArgs)>,
    nr: Sysno,
    requested: SyscallArgs,
    injected_frame: bool,
    skipped: bool,
) -> bool {
    let Some((original_nr, original)) = pending else {
        return false;
    };
    !injected_frame
        && !skipped
        && original_nr == Sysno::setsockopt
        && nr == original_nr
        && original.arg3 != requested.arg3
        && original.arg0 == requested.arg0
        && original.arg1 == requested.arg1
        && original.arg2 == requested.arg2
        && original.arg4 == requested.arg4
        && original.arg5 == requested.arg5
}

/// A matching request must consume its capture result, including failure.
/// Only an inapplicable request may use the existing ordinary injection route.
/// T is the retained entry in production; pure controls cannot create one.
///
/// `retained` is the Tool's latched `retain_original_syscall_entries` opt-in.
/// Without it no entry was captured and every request takes the ordinary
/// injection route, exactly as before this contract existed.
pub(super) fn route_optval_rewrite<T>(
    retained: bool,
    origin: InjectionOrigin,
    pending: Option<(Sysno, SyscallArgs)>,
    nr: Sysno,
    requested: SyscallArgs,
    injected_frame: bool,
    skipped: bool,
    captured: Option<Result<T, TraceError>>,
) -> Result<Option<T>, TraceError> {
    if !retained
        || origin != InjectionOrigin::Tool
        || !select_optval_rewrite(pending, nr, requested, injected_frame, skipped)
    {
        return Ok(None);
    }
    // Missing custody and an actual capture error both refuse before any
    // skip/resume. An authentication/read failure is not an unsupported call.
    captured.ok_or(Errno::EPROTO)?.map(Some)
}

fn same_native_entry(
    captured: safeptrace::SyscallEntry,
    actual: safeptrace::SyscallEntry,
    same_generation: bool,
    cs: u64,
) -> bool {
    same_generation
        && cs == 0x33
        && captured == actual
        && actual.arch == 0xc000003e
        && actual.seccomp
        && actual.number == Sysno::setsockopt as u64
}

impl OriginalSetsockoptEntry {
    pub(super) fn capture(task: &Stopped, args: SyscallArgs) -> Result<Self, TraceError> {
        let entry = task.syscall_entry()?;
        let regs = task.getregs()?;
        original_context::check_entry(
            task,
            Sysno::setsockopt,
            args,
            regs.ip(),
            regs.stack_ptr(),
            true,
        )?;
        if !same_native_entry(entry, entry, true, regs.cs) {
            return Err(Errno::EPROTO.into());
        }
        #[cfg(test)]
        native_tests::observe_capture_for_test(task, args)?;
        Ok(Self {
            owner: task.terminal_cleanup(),
            entry,
        })
    }

    /// Consumed before effect. No await separates this from take_original_entry.
    pub(super) fn validate(self, task: &Stopped) -> Result<(), TraceError> {
        let actual = task.syscall_entry()?;
        let regs = task.getregs()?;
        if !same_native_entry(
            self.entry,
            actual,
            self.owner.same_generation(&task.terminal_cleanup()),
            regs.cs,
        ) {
            return Err(Errno::EPROTO.into());
        }
        original_context::check_entry(
            task,
            Sysno::setsockopt,
            SyscallArgs::new(
                actual.arguments[0] as usize,
                actual.arguments[1] as usize,
                actual.arguments[2] as usize,
                actual.arguments[3] as usize,
                actual.arguments[4] as usize,
                actual.arguments[5] as usize,
            ),
            self.entry.instruction_pointer,
            self.entry.stack_pointer,
            true,
        )
    }
}

#[cfg(test)]
mod original_setsockopt_pure_tests {
    use super::*;
    fn args() -> SyscallArgs {
        SyscallArgs::new(7, 1, 20, 0x1000, 16, 0xfeed)
    }
    fn entry() -> safeptrace::SyscallEntry {
        safeptrace::SyscallEntry {
            arch: 0xc000003e,
            number: Sysno::setsockopt as u64,
            arguments: [7, 1, 20, 0x1000, 16, 0xfeed],
            instruction_pointer: 0x123402,
            stack_pointer: 0x456000,
            seccomp: true,
        }
    }
    #[test]
    fn changed_optval_selects_original_execution() {
        let mut requested = args();
        requested.arg3 = 0x2000;
        assert!(select_optval_rewrite(
            Some((Sysno::setsockopt, args())),
            Sysno::setsockopt,
            requested,
            false,
            false
        ));
    }
    #[test]
    fn applicable_capture_failure_cannot_route_to_private_execution() {
        let mut requested = args();
        requested.arg3 = 0x2000;
        let result = route_optval_rewrite::<u8>(
            true,
            InjectionOrigin::Tool,
            Some((Sysno::setsockopt, args())),
            Sysno::setsockopt,
            requested,
            false,
            false,
            Some(Err(Errno::EIO.into())),
        );
        assert!(
            matches!(result, Err(TraceError::Errno(Errno::EIO))),
            "the production route must preserve capture failure, not select fallback: {result:?}"
        );
    }
    #[test]
    fn applicable_missing_capture_cannot_route_to_private_execution() {
        let mut requested = args();
        requested.arg3 = 0x2000;
        let result = route_optval_rewrite::<u8>(
            true,
            InjectionOrigin::Tool,
            Some((Sysno::setsockopt, args())),
            Sysno::setsockopt,
            requested,
            false,
            false,
            None,
        );
        assert!(matches!(result, Err(TraceError::Errno(Errno::EPROTO))));
    }
    #[test]
    fn applicable_capture_is_returned_to_the_original_owner() {
        let mut requested = args();
        requested.arg3 = 0x2000;
        // A value-only routing control, not a native-stop capability.
        let result = route_optval_rewrite(
            true,
            InjectionOrigin::Tool,
            Some((Sysno::setsockopt, args())),
            Sysno::setsockopt,
            requested,
            false,
            false,
            Some(Ok(7u8)),
        );
        assert!(matches!(result, Ok(Some(7))));
    }
    #[test]
    fn nonmatching_requests_preserve_existing_injection_behavior_on_capture_failure() {
        let mut requested = args();
        requested.arg3 = 0x2000;
        for (origin, pending, nr, call, frame, skipped) in [
            (
                InjectionOrigin::Backend,
                Some((Sysno::setsockopt, args())),
                Sysno::setsockopt,
                requested,
                false,
                false,
            ),
            (
                InjectionOrigin::Tool,
                Some((Sysno::setsockopt, args())),
                Sysno::setsockopt,
                args(),
                false,
                false,
            ),
            (
                InjectionOrigin::Tool,
                None,
                Sysno::setsockopt,
                requested,
                false,
                false,
            ),
            (
                InjectionOrigin::Tool,
                Some((Sysno::setsockopt, args())),
                Sysno::getsockopt,
                requested,
                false,
                false,
            ),
            (
                InjectionOrigin::Tool,
                Some((Sysno::setsockopt, args())),
                Sysno::setsockopt,
                requested,
                true,
                false,
            ),
            (
                InjectionOrigin::Tool,
                Some((Sysno::setsockopt, args())),
                Sysno::setsockopt,
                requested,
                false,
                true,
            ),
        ] {
            let result = route_optval_rewrite::<u8>(
                true,
                origin,
                pending,
                nr,
                call,
                frame,
                skipped,
                Some(Err(Errno::EIO.into())),
            );
            assert!(matches!(result, Ok(None)));
        }
    }
    #[test]
    fn without_opt_in_applicable_requests_take_the_ordinary_route() {
        // A Tool that has not opted in has no captured entry. Even a request
        // that would select the original-entry route (Tool origin, only
        // optval changed) must keep the ordinary private-injection route,
        // whatever the capture slot holds.
        let mut requested = args();
        requested.arg3 = 0x2000;
        assert!(select_optval_rewrite(
            Some((Sysno::setsockopt, args())),
            Sysno::setsockopt,
            requested,
            false,
            false
        ));
        for captured in [None, Some(Err(Errno::EIO.into())), Some(Ok(7u8))] {
            let result = route_optval_rewrite::<u8>(
                false,
                InjectionOrigin::Tool,
                Some((Sysno::setsockopt, args())),
                Sysno::setsockopt,
                requested,
                false,
                false,
                captured,
            );
            assert!(matches!(result, Ok(None)), "{result:?}");
        }
    }
    #[test]
    fn delivered_frame_guard_rejects_each_changed_register() {
        let expected = [
            0x1202, 0x3000, 0x1202, 0x246, 0x246, 7, 1, 18, 0x8000, 4, 0xfeed,
        ];
        let mut frame = [0; 23];
        for (index, value) in native_tests::FRAME_INDICES.iter().zip(expected) {
            frame[*index] = value as libc::greg_t;
        }
        assert!(native_tests::frame_matches(&frame, expected));
        for index in native_tests::FRAME_INDICES {
            let mut changed = frame;
            changed[index] ^= 1;
            assert!(
                !native_tests::frame_matches(&changed, expected),
                "frame register {index}"
            );
        }
    }
    #[test]
    fn every_other_raw_operand_prevents_selection() {
        for index in [0, 1, 2, 4, 5] {
            let mut raw = [7, 1, 20, 0x2000, 16, 0xfeed];
            raw[index] ^= 1;
            let requested = SyscallArgs::new(raw[0], raw[1], raw[2], raw[3], raw[4], raw[5]);
            assert!(
                !select_optval_rewrite(
                    Some((Sysno::setsockopt, args())),
                    Sysno::setsockopt,
                    requested,
                    false,
                    false
                ),
                "operand {index}"
            );
        }
    }
    #[test]
    fn consumed_skipped_injected_and_other_calls_do_not_select() {
        let mut requested = args();
        requested.arg3 = 0x2000;
        for (pending, nr, frame, skipped) in [
            (None, Sysno::setsockopt, false, false),
            (
                Some((Sysno::setsockopt, args())),
                Sysno::setsockopt,
                true,
                false,
            ),
            (
                Some((Sysno::setsockopt, args())),
                Sysno::setsockopt,
                false,
                true,
            ),
            (
                Some((Sysno::getsockopt, args())),
                Sysno::setsockopt,
                false,
                false,
            ),
            (
                Some((Sysno::setsockopt, args())),
                Sysno::getsockopt,
                false,
                false,
            ),
        ] {
            assert!(!select_optval_rewrite(
                pending, nr, requested, frame, skipped
            ));
        }
        assert!(!select_optval_rewrite(
            Some((Sysno::setsockopt, args())),
            Sysno::setsockopt,
            args(),
            false,
            false
        ));
    }
    #[test]
    fn native_entry_comparator_rejects_foreign_compat_and_changed_context() {
        let e = entry();
        assert!(same_native_entry(e, e, true, 0x33));
        assert!(!same_native_entry(e, e, false, 0x33));
        assert!(!same_native_entry(e, e, true, 0x23));
        for which in 0..6 {
            let mut changed = e;
            match which {
                0 => changed.arch = 0x40000003,
                1 => changed.number |= 0x40000000,
                2 => changed.seccomp = false,
                3 => changed.instruction_pointer += 2,
                4 => changed.stack_pointer += 8,
                _ => changed.arguments[5] ^= 1,
            }
            assert!(!same_native_entry(e, changed, true, 0x33));
        }
    }
    #[test]
    fn restoration_preserves_real_result_flags_and_original_clobbers() {
        let mut original: libc::user_regs_struct = unsafe { std::mem::zeroed() };
        original.rip = 0x123402;
        original.rsp = 0x456000;
        original.orig_rax = Sysno::setsockopt as u64;
        original.set_args((7, 1, 20, 0x1000, 16, 0xfeed));
        original.rcx = original.rip;
        original.r11 = 0x302;
        let mut physical = original;
        // The kernel clobbers rcx/r11 at syscall exit. They must differ from
        // the original here, or deleting the clobber restoration goes unseen.
        physical.rcx = 0x7fff_0000_1002;
        physical.r11 = 0x246;
        physical.r10 = 0x2000;
        physical.rax = (-libc::ENOPROTOOPT as i64) as u64;
        physical.eflags = 0x302;
        physical.r12 = 0xabcdef;
        let restored = restored_context_registers(physical, original, None, false);
        assert_eq!(restored.args(), original.args());
        assert_eq!(restored.rax, physical.rax);
        assert_eq!(
            (restored.rip, restored.rsp, restored.rcx, restored.r11),
            (original.rip, original.rsp, original.rcx, original.r11)
        );
        assert_eq!(
            (restored.eflags, restored.r12),
            (physical.eflags, physical.r12)
        );
        assert_eq!(restored.orig_rax, original.orig_rax);
    }
}

#[cfg(test)]
mod native_tests;
