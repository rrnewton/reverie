// Reviewer probe suite committed as regression tests: astra-seat-sigdomain
// counterexamples (22). A failure names a semantic deviation from Linux.

use reverie_signal::frame::{XsaveImage, XstateDesc};
use reverie_signal::*;

fn task(n: i32) -> TaskGen {
    TaskGen {
        tid: n,
        generation: 1,
    }
}
fn bit(s: i32) -> u64 {
    1u64 << (s - 1)
}
fn info(s: i32, code: i32, value: i64) -> SigInfo {
    let mut i = SigInfo::user(s, 999, 777);
    i.code = code;
    i.value = value;
    i
}
fn env(sp: u64) -> FrameEnv {
    FrameEnv {
        sp_now: sp,
        xstate: XstateDesc::X87_SSE_YMM,
        stack_lower_bound: 0,
    }
}
fn handler(flags: u32) -> SigAction {
    SigAction {
        kind: DispositionKind::Handler,
        handler: 0x401000,
        mask: 0,
        flags: flags | SA_RESTORER,
        restorer: 0x402000,
    }
}
fn ignored() -> SigAction {
    SigAction {
        kind: DispositionKind::Ignore,
        ..SigAction::default()
    }
}
fn fact(t: TaskGen) -> SchedFact {
    SchedFact {
        task: t,
        running: false,
        exiting: false,
        stopped: false,
    }
}

#[test]
fn failed_and_blocked_private_send_allow_fresh_shared_receiver() {
    for blocked in [false, true] {
        let mut d = ProcessDomain::new(task(100), 1000, if blocked { 8 } else { 0 });
        let mut l = Sigpending::new();
        if blocked {
            d.thread_mut(task(100)).unwrap().blocked = bit(40);
        }
        let q = d
            .send_thread(task(100), info(40, -1, 1), SendSource::SigQueue, &mut l)
            .unwrap();
        if !blocked {
            assert_eq!(q.queue, QueueResult::Overflow);
        }
        assert!(!d.thread(task(100)).unwrap().sigpending);
        d.send_process(info(10, 0, 0), SendSource::UserKill, &mut l);
        assert_eq!(
            d.choose_receiver(10, task(100), &[fact(task(100))]),
            Some(task(100))
        );
    }
}
#[test]
fn ignored_cont_still_cancels_stops_and_continues() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.send_process(info(20, 0, 0), SendSource::UserKill, &mut l);
    d.set_action(18, ignored(), task(100), &mut l);
    let q = d.send_process(info(18, 0, 0), SendSource::UserKill, &mut l);
    assert!(q.continued);
    assert_eq!(q.queue, QueueResult::DiscardedIgnored);
    assert_eq!(d.shared.peek_mask() & bit(20), 0);
    assert_eq!(l.used_by(1000), 0);
}
#[test]
fn ignored_stop_still_cancels_cont() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.thread_mut(task(100)).unwrap().blocked = bit(18);
    d.send_process(info(18, 0, 0), SendSource::UserKill, &mut l);
    assert_ne!(d.shared.peek_mask() & bit(18), 0);
    d.set_action(20, ignored(), task(100), &mut l);
    let q = d.send_process(info(20, 0, 0), SendSource::UserKill, &mut l);
    assert_eq!(q.queue, QueueResult::DiscardedIgnored);
    assert_eq!(d.shared.peek_mask() & bit(18), 0);
}
#[test]
fn setting_default_cont_discards_private_and_shared_pending() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.thread_mut(task(100)).unwrap().blocked = bit(18);
    d.actions[17] = handler(0);
    d.send_process(info(18, 0, 0), SendSource::UserKill, &mut l);
    d.send_thread(task(100), info(18, 0, 0), SendSource::UserKill, &mut l);
    assert_eq!(l.used_by(1000), 2);
    d.set_action(18, SigAction::default(), task(100), &mut l);
    assert!(
        d.shared.is_empty(),
        "SIG_DFL/SIGCONT must flush shared pending"
    );
    assert!(d.thread(task(100)).unwrap().private.is_empty());
    assert_eq!(l.used_by(1000), 0);
}
#[test]
fn ignored_signal_saved_mask_retains_it() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.set_action(10, ignored(), task(100), &mut l);
    d.thread_mut(task(100)).unwrap().real_blocked = Some(bit(10));
    let q = d
        .send_thread(task(100), info(10, 0, 0), SendSource::UserKill, &mut l)
        .unwrap();
    assert!(matches!(q.queue, QueueResult::Queued(_)));
}
#[test]
fn process_ignore_retention_uses_leader_not_blocked_sibling() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.threads.push(ThreadState::new(task(101)));
    d.thread_mut(task(101)).unwrap().blocked = bit(10);
    d.set_action(10, ignored(), task(100), &mut l);
    assert_eq!(
        d.send_process(info(10, 0, 0), SendSource::UserKill, &mut l)
            .queue,
        QueueResult::DiscardedIgnored
    );
}
#[test]
fn timer_record_survives_later_ordinary_allocation() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.send_process(info(40, -2, 7), SendSource::TimerPrealloc(5), &mut l);
    d.send_process(info(40, -1, 8), SendSource::SigQueue, &mut l);
    assert_eq!(
        d.next_delivery(task(100), &env(0x8000), &mut l)
            .unwrap()
            .info
            .value,
        7
    );
    assert_eq!(
        d.next_delivery(task(100), &env(0x8000), &mut l)
            .unwrap()
            .info
            .value,
        8
    );
}
#[test]
fn positive_private_faults_keep_generation_order() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.send_thread(
        task(100),
        info(11, 1, 0),
        SendSource::KernelOrdinary,
        &mut l,
    );
    d.send_thread(task(100), info(4, 1, 0), SendSource::KernelOrdinary, &mut l);
    assert_eq!(
        d.next_delivery(task(100), &env(0x8000), &mut l)
            .unwrap()
            .info
            .signo,
        11
    );
}
#[test]
fn shared_faults_use_numeric_order() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.send_process(info(4, 0, 0), SendSource::UserKill, &mut l);
    d.send_process(info(11, 1, 0), SendSource::KernelOrdinary, &mut l);
    assert_eq!(
        d.next_delivery(task(100), &env(0x8000), &mut l)
            .unwrap()
            .info
            .signo,
        4
    );
}
#[test]
fn autodisarm_restarts_at_altstack_top() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.thread_mut(task(100)).unwrap().altstack = Some(AltStack {
        base: 0x10000,
        size: 2048,
        disabled: false,
        autodisarm: true,
    });
    d.actions[9] = handler(SA_ONSTACK);
    d.send_thread(task(100), info(10, 0, 0), SendSource::UserKill, &mut l);
    assert!(
        matches!(d.next_delivery(task(100),&env(0x10590),&mut l).unwrap().delivery, Delivery::EnterHandler{frame} if !frame.on_altstack_now)
    );
}
#[test]
fn switching_uses_redzone_adjusted_sp() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.thread_mut(task(100)).unwrap().altstack = Some(AltStack {
        base: 0x10000,
        size: 2048,
        disabled: false,
        autodisarm: false,
    });
    d.actions[9] = handler(SA_ONSTACK);
    d.send_thread(task(100), info(10, 0, 0), SendSource::UserKill, &mut l);
    assert!(matches!(
        d.next_delivery(task(100), &env(0x10080), &mut l)
            .unwrap()
            .delivery,
        Delivery::EnterHandler { .. }
    ));
}
#[test]
fn descriptor_rejects_impossible_mxcsr_masks() {
    assert!(XstateDesc::new(7, 832, 0xffff).is_ok());
    assert!(XstateDesc::new(7, 832, 1).is_err());
    assert!(XstateDesc::new(7, 832, 0xffff_ffff).is_err());
}
#[test]
fn live_ymm_presence_and_payload_survive() {
    let desc = XstateDesc::X87_SSE_YMM;
    let mut state = XsaveImage::initialized(desc);
    state[512..520].copy_from_slice(&7u64.to_le_bytes());
    state[600..608].copy_from_slice(&0x12345678u64.to_le_bytes());
    let image = XsaveImage::from_standard(&state, desc, false).unwrap();
    assert_ne!(
        u64::from_le_bytes(image.bytes()[512..520].try_into().unwrap()) & 4,
        0
    );
    let restored = XsaveImage::restore_from_signal(image.bytes(), desc).unwrap();
    assert_ne!(
        u64::from_le_bytes(restored[512..520].try_into().unwrap()) & 4,
        0
    );
    assert_eq!(&restored[600..608], &state[600..608]);
}
#[test]
fn standard_threadkill_overflow_uses_uncharged_zero_info() {
    let mut d = ProcessDomain::new(task(100), 1000, 0);
    let mut l = Sigpending::new();
    let q = d
        .send_thread(task(100), info(10, -6, 88), SendSource::ThreadKill, &mut l)
        .unwrap();
    assert!(matches!(q.queue, QueueResult::Queued(_)));
    assert_eq!(l.used_by(1000), 0);
    let p = d.next_delivery(task(100), &env(0x8000), &mut l).unwrap();
    assert_eq!(
        (p.info.code, p.info.pid, p.info.uid, p.info.value),
        (0, 0, 0, 0)
    );
}
#[test]
fn fallback_bit_absorbed_by_later_ordinary_record() {
    let mut d = ProcessDomain::new(task(100), 1000, 0);
    let mut l = Sigpending::new();
    d.send_process(info(40, 0, 0), SendSource::UserKill, &mut l);
    d.sigpending_limit = 1;
    d.send_process(info(40, -1, 8), SendSource::SigQueue, &mut l);
    assert_eq!(
        d.next_delivery(task(100), &env(0x8000), &mut l)
            .unwrap()
            .info
            .value,
        8
    );
    assert!(d.next_delivery(task(100), &env(0x8000), &mut l).is_none());
}
#[test]
fn fallback_bit_absorbed_by_later_timer_record() {
    let mut d = ProcessDomain::new(task(100), 1000, 0);
    let mut l = Sigpending::new();
    d.send_process(info(40, 0, 0), SendSource::UserKill, &mut l);
    d.send_process(info(40, -2, 7), SendSource::TimerPrealloc(5), &mut l);
    assert_eq!(
        d.next_delivery(task(100), &env(0x8000), &mut l)
            .unwrap()
            .info
            .value,
        7
    );
    assert!(d.next_delivery(task(100), &env(0x8000), &mut l).is_none());
}
#[test]
fn retained_default_cont_is_discarded_at_delivery() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.thread_mut(task(100)).unwrap().blocked = bit(18);
    assert!(
        d.send_process(info(18, 0, 0), SendSource::UserKill, &mut l)
            .continued
    );
    d.thread_mut(task(100)).unwrap().blocked = 0;
    assert_eq!(
        d.next_delivery(task(100), &env(0x8000), &mut l)
            .unwrap()
            .delivery,
        Delivery::Discard
    );
}
#[test]
fn distinct_standard_signal_timers_keep_distinct_records() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.thread_mut(task(100)).unwrap().blocked = bit(10);
    d.send_process(info(10, -2, 3), SendSource::TimerPrealloc(3), &mut l);
    d.send_process(info(10, -2, 4), SendSource::TimerPrealloc(4), &mut l);
    d.thread_mut(task(100)).unwrap().blocked = 0;
    assert_eq!(
        d.next_delivery(task(100), &env(0x8000), &mut l)
            .unwrap()
            .info
            .value,
        3
    );
    assert_eq!(
        d.next_delivery(task(100), &env(0x8000), &mut l)
            .map(|p| p.info.value),
        Some(4)
    );
}
#[test]
fn repeated_standard_timer_expiry_updates_overrun() {
    let mut d = ProcessDomain::new(task(100), 1000, 8);
    let mut l = Sigpending::new();
    d.thread_mut(task(100)).unwrap().blocked = bit(10);
    d.send_process(info(10, -2, 3), SendSource::TimerPrealloc(3), &mut l);
    d.send_process(info(10, -2, 3), SendSource::TimerPrealloc(3), &mut l);
    d.thread_mut(task(100)).unwrap().blocked = 0;
    assert_eq!(
        d.next_delivery(task(100), &env(0x8000), &mut l)
            .unwrap()
            .info
            .overrun,
        1
    );
}
#[test]
fn no_handler_restartblock_without_deadline_does_not_invent_eintr() {
    assert_ne!(
        map_restart(RestartCode::RestartRestartBlock, false, false, None),
        WaitResolution::Eintr
    );
}

#[test]
fn absent_ymm_component_is_canonical_zero() {
    let desc = XstateDesc::new(3, 832, 0xffbf).unwrap();
    let mut a = XsaveImage::initialized(desc);
    a[512..520].copy_from_slice(&7u64.to_le_bytes());
    let mut b = a.clone();
    b[600] = 0xaa;
    let ia = XsaveImage::from_standard(&a, desc, true).unwrap();
    let ib = XsaveImage::from_standard(&b, desc, true).unwrap();
    assert_eq!(
        ia.bytes(),
        ib.bytes(),
        "nonpresented YMM bytes cannot change the frame"
    );
}
#[test]
fn padding_beyond_presented_components_is_canonical_zero() {
    let desc = XstateDesc::new(7, 2700, 0xffbf).unwrap();
    let mut a = XsaveImage::initialized(desc);
    a[512..520].copy_from_slice(&7u64.to_le_bytes());
    let mut b = a.clone();
    b[900] = 0xaa;
    let ia = XsaveImage::from_standard(&a, desc, false).unwrap();
    let ib = XsaveImage::from_standard(&b, desc, false).unwrap();
    assert_eq!(
        ia.bytes(),
        ib.bytes(),
        "padding outside the presented components cannot change the frame"
    );
}
