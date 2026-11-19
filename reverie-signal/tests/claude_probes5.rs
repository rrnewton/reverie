// Reviewer probe suite committed as regression tests: claude-seat-sigdomain
// probes5 (32). A failure names a semantic deviation from Linux.

// Reviewer probes for reverie-signal at 479766ae (ported from probes3). Each test asserts what
// Linux v7.1.3 does (kernel/signal.c, arch/x86/kernel/signal.c): a PASS
// means the crate matches Linux, a FAIL names a remaining deviation.
use reverie_signal::frame::*;
use reverie_signal::*;

fn tg(tid: i32) -> TaskGen {
    TaskGen { tid, generation: 1 }
}
fn fact(task: TaskGen) -> SchedFact {
    SchedFact {
        task,
        running: false,
        exiting: false,
        stopped: false,
    }
}
fn info(signo: i32, code: i32, value: i64) -> SigInfo {
    SigInfo {
        signo,
        code,
        value,
        pid: 7,
        uid: 0,
        addr: 0,
        overrun: 0,
    }
}
fn handler(flags: u32) -> SigAction {
    SigAction {
        kind: DispositionKind::Handler,
        handler: 0x401000,
        mask: 0,
        flags,
        restorer: 0x7f0000,
    }
}
fn ign() -> SigAction {
    SigAction {
        kind: DispositionKind::Ignore,
        ..SigAction::default()
    }
}
fn xs() -> XstateDesc {
    XstateDesc::new(SUPPORTED_XFEATURES, 832, 0xffff).unwrap()
}
fn env() -> FrameEnv {
    FrameEnv {
        sp_now: 0x7fff_0000,
        xstate: xs(),
        stack_lower_bound: 0,
    }
}
const SI_QUEUE: i32 = -1;
const SI_TKILL: i32 = -6;
const SI_TIMER: i32 = -2;

// ---- round-2 probes, now written as Linux expectations ----

#[test]
fn q01_standard_tgkill_at_limit_is_siginfo_less_and_uncharged() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 0);
    d.send_thread(
        tg(100),
        info(10, SI_TKILL, 0),
        SendSource::ThreadKill,
        &mut l,
    )
    .unwrap();
    assert_eq!(l.used_by(1000), 0);
    let p = d.next_delivery(tg(100), &env(), &mut l).unwrap();
    assert_eq!((p.info.code, p.info.pid), (0, 0));
}

#[test]
fn q02_rt_ordinary_kernel_send_at_limit_gets_eagain() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 0);
    assert_eq!(
        d.send_process(info(40, 1, 5), SendSource::KernelOrdinary, &mut l)
            .queue,
        QueueResult::Overflow
    );
}

#[test]
fn q03_fallback_bit_and_later_record_deliver_once() {
    let mut l = Sigpending::new();
    let mut g = ProcessDomain::new(tg(100), 1000, 1);
    g.send_thread(tg(100), info(41, SI_QUEUE, 1), SendSource::SigQueue, &mut l);
    g.send_process(info(40, 0, 0), SendSource::UserKill, &mut l);
    let _ = g.next_delivery(tg(100), &env(), &mut l);
    g.send_process(info(40, SI_QUEUE, 77), SendSource::SigQueue, &mut l);
    let a = g.next_delivery(tg(100), &env(), &mut l).unwrap();
    assert_eq!((a.info.signo, a.info.value), (40, 77));
    assert!(g.next_delivery(tg(100), &env(), &mut l).is_none());
}

#[test]
fn q04_sigxcpu_is_not_synchronous() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.actions[9] = handler(SA_RESTORER);
    d.actions[23] = handler(SA_RESTORER);
    d.send_thread(tg(100), info(10, 0, 0), SendSource::UserKill, &mut l);
    d.send_thread(tg(100), info(24, 0, 0), SendSource::UserKill, &mut l);
    assert_eq!(
        d.next_delivery(tg(100), &env(), &mut l).unwrap().info.signo,
        10
    );
}

#[test]
fn q05_receiver_after_queueing_is_the_named_thread() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.threads.push(ThreadState::new(tg(101)));
    let facts = [fact(tg(100)), fact(tg(101))];
    d.send_process(info(10, 0, 0), SendSource::UserKill, &mut l);
    assert_eq!(d.choose_receiver(10, tg(100), &facts), Some(tg(100)));
}

#[test]
fn q06_second_process_signal_goes_to_the_next_thread() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.threads.push(ThreadState::new(tg(101)));
    let facts = [fact(tg(100)), fact(tg(101))];
    d.send_process(info(10, 0, 0), SendSource::UserKill, &mut l);
    assert_eq!(d.choose_receiver(10, tg(100), &facts), Some(tg(100)));
    d.send_process(info(12, 0, 0), SendSource::UserKill, &mut l);
    assert_eq!(d.choose_receiver(12, tg(100), &facts), Some(tg(101)));
}

#[test]
fn q08_unblocked_default_ignored_signal_is_not_queued() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    let out = d.send_process(info(17, SI_QUEUE, 0), SendSource::SigQueue, &mut l);
    assert_eq!(
        (out.queue, d.shared.len(), l.used_by(1000)),
        (QueueResult::DiscardedIgnored, 0, 0)
    );
}

#[test]
fn q09_sigaction_flushes_every_thread_and_sig_dfl_default_ignore() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.threads.push(ThreadState::new(tg(101)));
    d.thread_mut(tg(101)).unwrap().blocked = 1 << 9;
    d.send_thread(
        tg(101),
        info(10, SI_TKILL, 0),
        SendSource::ThreadKill,
        &mut l,
    );
    d.set_action(10, ign(), tg(100), &mut l);
    assert_eq!(d.thread(tg(101)).unwrap().private.len(), 0);
    let mut e = ProcessDomain::new(tg(100), 1000, 64);
    e.thread_mut(tg(100)).unwrap().blocked = 1 << 16;
    e.actions[16] = handler(SA_RESTORER);
    e.send_process(info(17, 0, 0), SendSource::UserKill, &mut l);
    e.set_action(17, SigAction::default(), tg(100), &mut l);
    assert_eq!(e.shared.len(), 0);
}

#[test]
fn q10_ss_autodisarm_reenters_the_altstack_top() {
    // on_sig_stack() is 0 under SS_AUTODISARM (get_sigframe comment).
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.actions[9] = handler(SA_RESTORER | SA_ONSTACK);
    d.thread_mut(tg(100)).unwrap().altstack = Some(AltStack {
        base: 0x10000,
        size: 0x8000,
        disabled: false,
        autodisarm: true,
    });
    d.send_thread(tg(100), info(10, 0, 0), SendSource::UserKill, &mut l);
    let e = FrameEnv {
        sp_now: 0x14000,
        ..env()
    };
    match d.next_delivery(tg(100), &e, &mut l).unwrap().delivery {
        Delivery::EnterHandler { frame } => assert!(frame.use_altstack && !frame.on_altstack_now),
        other => panic!("{other:?}"),
    }
}

#[test]
fn q11_handler_mask_blocks_a_second_copy_until_sigreturn() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.actions[39] = handler(SA_RESTORER);
    d.send_thread(tg(100), info(40, SI_QUEUE, 1), SendSource::SigQueue, &mut l);
    d.send_thread(tg(100), info(40, SI_QUEUE, 2), SendSource::SigQueue, &mut l);
    let _a = d.next_delivery(tg(100), &env(), &mut l).unwrap();
    assert!(
        d.next_delivery(tg(100), &env(), &mut l).is_none(),
        "signal_delivered blocks 40"
    );
}

#[test]
fn q12_sa_resethand_resets_only_the_handler() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.actions[9] = SigAction {
        mask: 1 << 3,
        ..handler(SA_RESTORER | SA_RESETHAND)
    };
    d.send_thread(tg(100), info(10, 0, 0), SendSource::UserKill, &mut l);
    d.next_delivery(tg(100), &env(), &mut l).unwrap();
    assert_eq!(d.actions[9].kind, DispositionKind::Default);
    assert_eq!(
        (d.actions[9].mask, d.actions[9].restorer),
        (1 << 3, 0x7f0000)
    );
}

#[test]
fn q13_frame_placement_is_linux_get_sigframe() {
    let linux_frame = |top: u64, xsize: u64| {
        let buf = (top - (xsize + 4)) & !63;
        ((buf - 440) & !15) - 8
    };
    for top in [0x7fff_f000u64, 0x7fff_f008, 0x1_0000_0517, 0x4000_1234] {
        assert_eq!(
            FrameLayout::below(top, false, xs()).unwrap().frame_address,
            linux_frame(top, 832)
        );
    }
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.actions[9] = handler(SA_RESTORER | SA_ONSTACK);
    d.thread_mut(tg(100)).unwrap().altstack = Some(AltStack {
        base: 0x10000,
        size: 1300,
        disabled: false,
        autodisarm: false,
    });
    d.send_thread(tg(100), info(10, 0, 0), SendSource::UserKill, &mut l);
    assert_eq!(
        d.next_delivery(tg(100), &env(), &mut l).unwrap().delivery,
        Delivery::ForceSegv
    );
}

#[test]
fn q14a_frame_xstate_bv_stays_within_presented_features() {
    let desc = XstateDesc::new(SUPPORTED_XFEATURES, 2700, 0xffff).unwrap();
    let mut state = XsaveImage::initialized(desc);
    state[512] |= 1 << 5;
    let img = XsaveImage::from_standard(&state, desc, false).unwrap();
    assert_eq!(img.bytes()[512] & (1 << 5), 0);
    assert!(XsaveImage::restore_from_signal(img.bytes(), desc).is_ok());
}

#[test]
fn q14b_frame_bytes_beyond_presented_components_are_not_input_bytes() {
    // Presented x87|SSE|YMM ends at 832; a host buffer's bytes past it
    // (e.g. AVX-512 state) must not reach the guest-visible frame.
    let desc = XstateDesc::new(SUPPORTED_XFEATURES, 2700, 0xffff).unwrap();
    let mut state = XsaveImage::initialized(desc);
    state[1100] = 0xAA;
    let img = XsaveImage::from_standard(&state, desc, false).unwrap();
    assert_eq!(
        img.bytes()[1100],
        0,
        "host bytes beyond presented components copied"
    );
}

#[test]
fn q15_frame_mxcsr_mask_is_the_presented_one() {
    let desc = XstateDesc::new(SUPPORTED_XFEATURES, 832, 0xffbf).unwrap();
    let mut state = XsaveImage::initialized(desc);
    state[28..32].copy_from_slice(&0xffffu32.to_le_bytes());
    let img = XsaveImage::from_standard(&state, desc, false).unwrap();
    assert_eq!(
        u32::from_le_bytes(img.bytes()[28..32].try_into().unwrap()),
        0xffbf
    );
}

#[test]
fn q16_kvm_shape_constant_accepts_daz() {
    let d = XstateDesc::X87_SSE_YMM;
    let mut state = XsaveImage::initialized(d);
    state[24..28].copy_from_slice(&0x1fc0u32.to_le_bytes());
    let img = XsaveImage::from_standard(&state, d, false).unwrap();
    assert!(XsaveImage::restore_from_signal(img.bytes(), d).is_ok());
}

#[test]
fn q17_ymm_extent_is_architectural_832() {
    let big = XstateDesc::new(SUPPORTED_XFEATURES, 2700, 0xffff).unwrap();
    let small = XstateDesc::new(SUPPORTED_XFEATURES, 832, 0xffff).unwrap();
    let img = XsaveImage::from_standard(&XsaveImage::initialized(small), small, false).unwrap();
    let mut bytes = img.bytes().to_vec();
    bytes[512] |= 1 << 2;
    assert!(XsaveImage::restore_from_signal(&bytes, big).is_ok());
}

#[test]
fn q18_release_goes_to_the_charged_uid() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.send_process(info(40, SI_QUEUE, 1), SendSource::SigQueue, &mut l);
    d.uid = 2000;
    d.next_delivery(tg(100), &env(), &mut l).unwrap();
    assert_eq!((l.used_by(1000), l.used_by(2000)), (0, 0));
}

#[test]
fn q19_invalid_desc_is_unrepresentable() {
    assert!(XstateDesc::new(SUPPORTED_XFEATURES, usize::MAX - 2, 0).is_err());
}

// ---- new probes for 170ae243 ----

#[test]
fn r20_ignored_sigcont_still_continues_and_flushes_stops() {
    // prepare_signal: the SIGCONT flush + wake-up runs BEFORE the
    // `return !sig_ignored(..)` (kernel/signal.c:871-940).
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.thread_mut(tg(100)).unwrap().blocked = 1 << 19; // block SIGTSTP
    d.send_process(info(20, 0, 0), SendSource::UserKill, &mut l);
    d.set_action(SIGCONT, ign(), tg(100), &mut l);
    let out = d.send_process(info(SIGCONT, 0, 0), SendSource::UserKill, &mut l);
    assert!(out.continued, "SIGCONT continues even when ignored");
    assert_eq!(
        d.shared.peek_mask() & (1 << 19),
        0,
        "pending SIGTSTP flushed"
    );
}

#[test]
fn r21_ignored_stop_signal_still_flushes_sigcont() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.thread_mut(tg(100)).unwrap().blocked = 1 << 17; // block SIGCONT
    d.send_process(info(SIGCONT, 0, 0), SendSource::UserKill, &mut l);
    d.set_action(20, ign(), tg(100), &mut l);
    d.send_process(info(20, 0, 0), SendSource::UserKill, &mut l);
    assert_eq!(
        d.shared.peek_mask() & (1 << 17),
        0,
        "pending SIGCONT flushed by the ignored SIGTSTP"
    );
}

#[test]
fn r22_default_sigcont_is_in_the_ignore_class() {
    // sig_kernel_ignore includes SIGCONT: SIG_DFL SIGCONT is discarded at
    // generation (after continuing) and do_sigaction(SIG_DFL) flushes it.
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    let out = d.send_process(info(SIGCONT, 0, 0), SendSource::UserKill, &mut l);
    assert!(out.continued);
    assert_eq!(
        out.queue,
        QueueResult::DiscardedIgnored,
        "unblocked SIG_DFL SIGCONT is not queued"
    );
    let mut e = ProcessDomain::new(tg(100), 1000, 64);
    e.thread_mut(tg(100)).unwrap().blocked = 1 << 17;
    e.send_process(info(SIGCONT, 0, 0), SendSource::UserKill, &mut l);
    e.set_action(SIGCONT, SigAction::default(), tg(100), &mut l);
    assert_eq!(e.shared.len(), 0, "SIG_DFL install flushes pending SIGCONT");
}

#[test]
fn r23_generation_ignore_tests_the_target_threads_mask() {
    // sig_ignored(t, ..) reads the TARGET task t (for kill(), the leader
    // found by pid); a sibling's mask does not keep the signal.
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.threads.push(ThreadState::new(tg(101)));
    d.thread_mut(tg(101)).unwrap().blocked = 1 << 16; // sibling blocks SIGCHLD
    let out = d.send_process(info(17, 0, 0), SendSource::UserKill, &mut l);
    assert_eq!(out.queue, QueueResult::DiscardedIgnored);
}

#[test]
fn r24_timer_record_and_later_queue_record_deliver_twice() {
    // Two distinct sigqueue records (preallocated timer + sigqueue): two
    // deliveries, the timer's siginfo intact.
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.thread_mut(tg(100)).unwrap().blocked = 1 << 39;
    d.send_process(info(40, SI_TIMER, 5), SendSource::TimerPrealloc(1), &mut l);
    d.send_process(info(40, SI_QUEUE, 9), SendSource::SigQueue, &mut l);
    d.thread_mut(tg(100)).unwrap().blocked = 0;
    let a = d.next_delivery(tg(100), &env(), &mut l).unwrap();
    let b = d.next_delivery(tg(100), &env(), &mut l);
    assert_eq!(
        (a.info.code, a.info.value),
        (SI_TIMER, 5),
        "timer record overwritten"
    );
    assert!(b.is_some(), "only one delivery");
}

#[test]
fn r25_fallback_bit_absorbed_by_a_timer_record() {
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 0);
    d.thread_mut(tg(100)).unwrap().blocked = 1 << 39;
    d.send_process(info(40, 0, 0), SendSource::UserKill, &mut l); // bit only
    d.send_process(info(40, SI_TIMER, 5), SendSource::TimerPrealloc(2), &mut l);
    d.thread_mut(tg(100)).unwrap().blocked = 0;
    let a = d.next_delivery(tg(100), &env(), &mut l).unwrap();
    assert_eq!(a.info.code, SI_TIMER);
    assert!(
        d.next_delivery(tg(100), &env(), &mut l).is_none(),
        "fallback and timer delivered twice"
    );
}

#[test]
fn r26_repeated_expiry_of_one_timer_keeps_one_record() {
    // posixtimer_send_sigqueue: an already-pending preallocated record is
    // not queued again; the overrun count grows instead.
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.thread_mut(tg(100)).unwrap().blocked = 1 << 39;
    d.send_process(info(40, SI_TIMER, 5), SendSource::TimerPrealloc(3), &mut l);
    d.send_process(info(40, SI_TIMER, 5), SendSource::TimerPrealloc(3), &mut l);
    assert_eq!(d.shared.len(), 1, "a second record for the same timer");
}

// ---- new probes for 479766ae ----

#[test]
fn s27_saved_premask_does_not_retain_an_ignored_signal() {
    // sig_ignored retains for t->blocked or t->real_blocked, where Linux's
    // real_blocked is set only by rt_sigtimedwait. During rt_sigsuspend the
    // pre-wait mask is saved_sigmask and does NOT retain: an ignored
    // SIGCHLD is dropped and does not end the sigsuspend.
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    {
        let t = d.thread_mut(tg(100)).unwrap();
        t.blocked = 0; // sigsuspend's temporary mask
        t.saved_sigmask = Some(1 << 16); // sigsuspend's saved pre-wait mask (split out at 8cdd08d2)
    }
    let out = d.send_process(info(17, 0, 0), SendSource::UserKill, &mut l);
    assert_eq!(
        out.queue,
        QueueResult::DiscardedIgnored,
        "retained via the saved pre-wait mask"
    );
}

#[test]
fn s32_amd_mm_bit_mask_is_presentable() {
    // MXCSR_MASK is a 32-bit FXSAVE field; CPUs with misaligned-SSE mode
    // report bit 17 (MM), and Linux's mxcsr_feature_mask then admits it.
    assert!(XstateDesc::new(SUPPORTED_XFEATURES, 832, 0x2ffff).is_ok());
}

#[test]
fn s33_restartblock_without_deadline_restarts_via_restart_syscall() {
    // Corrected premise: poll(-1) returns ERESTART_RESTARTBLOCK with
    // restart_block->poll.has_timeout = 0 (fs/select.c:1074-1087 v7.1.3);
    // with no handler it restarts through restart_syscall.
    assert_eq!(
        map_restart(RestartCode::RestartRestartBlock, false, false, None),
        WaitResolution::RestartSyscall
    );
    assert_eq!(
        map_restart(RestartCode::RestartRestartBlock, true, false, None),
        WaitResolution::Eintr
    );
}

#[test]
fn s27b_timedwait_real_blocked_retains_an_ignored_signal() {
    // sig_ignored: blocked in t->real_blocked (rt_sigtimedwait's original
    // mask) is never ignored, so the signal is queued.
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    {
        let t = d.thread_mut(tg(100)).unwrap();
        t.blocked = 0;
        t.real_blocked = Some(1 << 16);
    }
    let out = d.send_process(info(17, 0, 0), SendSource::UserKill, &mut l);
    assert!(matches!(out.queue, QueueResult::Queued(_)));
}

#[test]
fn t34_second_frame_at_one_return_saves_the_first_handlers_mask() {
    // signal_delivered -> clear_restore_sigmask(): once the first frame
    // stored saved_sigmask, a second frame built before any guest
    // instruction saves current->blocked (the first handler's mask).
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.actions[9] = handler(SA_RESTORER);
    d.actions[11] = handler(SA_RESTORER);
    {
        let t = d.thread_mut(tg(100)).unwrap();
        t.blocked = 0; // sigsuspend temporary mask
        t.saved_sigmask = Some(1 << 2); // pre-wait mask: SIGQUIT
    }
    d.send_thread(tg(100), info(10, 0, 0), SendSource::UserKill, &mut l);
    d.send_thread(tg(100), info(12, 0, 0), SendSource::UserKill, &mut l);
    let Delivery::EnterHandler { frame: f1 } =
        d.next_delivery(tg(100), &env(), &mut l).unwrap().delivery
    else {
        panic!()
    };
    assert_eq!(f1.restore_mask, 1 << 2);
    let Delivery::EnterHandler { frame: f2 } =
        d.next_delivery(tg(100), &env(), &mut l).unwrap().delivery
    else {
        panic!()
    };
    assert_eq!(
        f2.restore_mask,
        1 << 9,
        "second frame must save the first handler's mask, not the stale pre-wait mask"
    );
}

#[test]
fn t38_standard_timer_record_coexists_with_a_regular_copy() {
    // posixtimer_queue_sigqueue has no legacy_queue check: a standard
    // signal's timer record joins a pending regular copy; two deliveries.
    let mut l = Sigpending::new();
    let mut d = ProcessDomain::new(tg(100), 1000, 64);
    d.actions[13] = handler(SA_RESTORER);
    d.thread_mut(tg(100)).unwrap().blocked = 1 << 13;
    d.send_process(info(14, 0, 0), SendSource::UserKill, &mut l);
    d.send_process(info(14, SI_TIMER, 4), SendSource::TimerPrealloc(9), &mut l);
    d.thread_mut(tg(100)).unwrap().blocked = 0;
    let a = d.next_delivery(tg(100), &env(), &mut l).unwrap();
    d.thread_mut(tg(100)).unwrap().blocked = 0; // sigreturn
    let b = d.next_delivery(tg(100), &env(), &mut l);
    assert_eq!(a.info.signo, 14);
    assert!(b.is_some());
}
