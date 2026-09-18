use super::*;

fn image() -> ImageIdentity {
    ImageIdentity {
        tid: 42,
        start_ticks: 73,
        generation: 1,
        executable_device: 11,
        executable_inode: 19,
        at_entry: 0x400000,
        at_phdr: 0x400040,
    }
}
fn guard() -> EntryGuard {
    EntryGuard::prepare(image(), [0x48, 0x31, 0xed, 0x49, 0x89, 0xd1, 0x5e, 0x48]).unwrap()
}
fn entry() -> TrapObservation {
    TrapObservation {
        identity: image(),
        signal: 5,
        si_code: 128,
        rip: image().at_entry + 1,
        rsp: 0x7fff0000,
        r10: 0,
    }
}
fn calls() -> Calls {
    let guard = guard();
    let word = guard.guarded_word();
    Calls::at_entry(guard, &entry(), word).unwrap()
}
fn code(guard: &EntryGuard) -> CallCode {
    CallCode::prepare(guard, 0x700000, 0x900000, 0x801000, 0x12345678).unwrap()
}
fn returned(code: &CallCode) -> TrapObservation {
    TrapObservation {
        identity: image(),
        signal: 5,
        si_code: 1,
        rip: code.return_rip,
        rsp: code.call_stack_top,
        r10: code.marker,
    }
}
fn loaded() -> (Calls, CallCode) {
    let mut calls = calls();
    let code = code(calls.guard());
    calls.start_dlopen().unwrap();
    calls
        .dlopen_returned(&code, &returned(&code), &code.bytes, 0x700140)
        .unwrap();
    (calls, code)
}
fn ready() -> (Calls, CallCode) {
    let (mut calls, code) = loaded();
    calls.start_initializer().unwrap();
    calls.begin().unwrap();
    calls.ready().unwrap();
    (calls, code)
}

#[test]
fn complete_authenticated_calls_preserve_reference_and_defer_ready_until_restored() {
    let mut first_calls = calls();
    let authenticated = first_calls.take_authenticated_entry().unwrap();
    assert_eq!(authenticated.identity(), image());
    assert_eq!(authenticated.original(), first_calls.guard().original());
    assert_eq!(authenticated.guarded(), first_calls.guard().guarded_word());
    assert_eq!(
        first_calls.take_authenticated_entry(),
        Err(Refusal::Order),
        "authenticated entry token was issued twice"
    );
    // A duplicate token request deliberately fails the call state; start from
    // a fresh exactly authenticated entry for the remaining lifecycle checks.
    let mut calls = calls();
    let code = code(calls.guard());
    calls
        .errno_returned(&code, &returned(&code), &code.bytes)
        .unwrap();
    assert_eq!(calls.phase(), Phase::EntryHeld);
    calls.start_dlopen().unwrap();
    calls
        .dlopen_returned(&code, &returned(&code), &code.bytes, 0x700140)
        .unwrap();
    assert_eq!(calls.handle, Some(0x700140));
    calls.start_initializer().unwrap();
    calls.begin().unwrap();
    calls.ready().unwrap();
    assert_eq!(calls.phase(), Phase::ReadyObserved);
    calls
        .initializer_returned(&code, &returned(&code), &code.bytes, 0)
        .unwrap();
    calls
        .errno_returned(&code, &returned(&code), &code.bytes)
        .unwrap();
    assert_eq!(calls.phase(), Phase::InitializerReturned);
    calls.restored().unwrap();
    assert_eq!(calls.phase(), Phase::Restored);
    assert_eq!(calls.handle, Some(0x700140));
    assert_eq!(&host_config()[..8], &1u64.to_le_bytes());
    assert_eq!(&host_config()[8..], &[0; 8]);
}

#[test]
fn entry_refuses_each_missing_bound_identity_field_and_existing_breakpoint() {
    for index in 0..7 {
        let mut id = image();
        match index {
            0 => id.tid = 0,
            1 => id.start_ticks = 0,
            2 => id.generation = 0,
            3 => id.executable_device = 0,
            4 => id.executable_inode = 0,
            5 => id.at_entry = 0,
            _ => id.at_phdr = 0,
        }
        assert!(EntryGuard::prepare(id, [0x90; 8]).is_err(), "field {index}");
    }
    assert!(EntryGuard::prepare(image(), [0xcc; 8]).is_err());
    assert!(
        EntryGuard::prepare(
            ImageIdentity {
                at_entry: u64::MAX - 7,
                ..image()
            },
            [0x90; 8]
        )
        .is_err()
    );
}

#[test]
fn entry_and_call_refuse_each_entry_image_swap() {
    let guard = guard();
    let code = code(&guard);
    for index in 0..7 {
        let mut id = image();
        match index {
            0 => id.tid += 1,
            1 => id.start_ticks += 1,
            2 => id.generation += 1,
            3 => id.executable_device += 1,
            4 => id.executable_inode += 1,
            5 => id.at_entry += 1,
            _ => id.at_phdr += 1,
        }
        assert_eq!(
            guard.authenticate(
                &TrapObservation {
                    identity: id,
                    ..entry()
                },
                guard.guarded_word()
            ),
            Err(Refusal::Identity)
        );
        assert_eq!(
            code.authenticate_return(
                &TrapObservation {
                    identity: id,
                    ..returned(&code)
                },
                &code.bytes
            ),
            Err(Refusal::Identity)
        );
    }
}

#[test]
fn call_return_rejects_rip_rsp_marker_each_code_byte_and_non_int3_siginfo() {
    let code = code(&guard());
    for index in 0..3 {
        let mut stop = returned(&code);
        match index {
            0 => stop.rip += 1,
            1 => stop.rsp -= 8,
            _ => stop.r10 ^= 1,
        }
        assert_eq!(
            code.authenticate_return(&stop, &code.bytes),
            Err(Refusal::Return)
        );
    }
    for index in 0..code.bytes.len() {
        let mut bytes = code.bytes;
        bytes[index] ^= 1;
        assert_eq!(
            code.authenticate_return(&returned(&code), &bytes),
            Err(Refusal::Instruction)
        );
    }
    assert!(
        code.authenticate_return(&returned(&code), &code.bytes[..29])
            .is_err()
    );
    for si_code in [-6, -1, 0, 2, 4] {
        assert_eq!(
            code.authenticate_return(
                &TrapObservation {
                    si_code,
                    ..returned(&code)
                },
                &code.bytes
            ),
            Err(Refusal::Signal)
        );
    }
    assert_eq!(
        code.authenticate_return(
            &TrapObservation {
                signal: 10,
                ..returned(&code)
            },
            &code.bytes
        ),
        Err(Refusal::Signal)
    );
}

#[test]
fn calls_refuse_a_valid_return_prepared_for_another_entry_image() {
    let mut calls = calls();
    calls.start_dlopen().unwrap();
    let other = EntryGuard::prepare(
        ImageIdentity {
            generation: 2,
            ..image()
        },
        [0x90; 8],
    )
    .unwrap();
    let code = code(&other);
    let stop = TrapObservation {
        identity: other.identity,
        ..returned(&code)
    };
    assert!(code.authenticate_return(&stop, &code.bytes).is_ok());
    assert_eq!(
        calls.dlopen_returned(&code, &stop, &code.bytes, 7),
        Err(Refusal::Identity)
    );
    assert_eq!(calls.phase(), Phase::Failed);
}

#[test]
fn handshake_order_duplicate_begin_ready_and_early_return_are_refused() {
    let mut calls = calls();
    assert_eq!(calls.begin(), Err(Refusal::Order));
    let (mut calls, _) = loaded();
    calls.start_initializer().unwrap();
    assert_eq!(calls.ready(), Err(Refusal::Order));
    let (mut calls, _) = loaded();
    calls.start_initializer().unwrap();
    calls.begin().unwrap();
    assert_eq!(calls.begin(), Err(Refusal::Order));
    let (mut calls, _) = ready();
    assert_eq!(calls.ready(), Err(Refusal::Order));
    let (mut calls, code) = loaded();
    calls.start_initializer().unwrap();
    assert_eq!(
        calls.initializer_returned(&code, &returned(&code), &code.bytes, 0),
        Err(Refusal::Order)
    );
    let (mut calls, _) = ready();
    assert_eq!(calls.restored(), Err(Refusal::Order));
}

#[test]
fn null_dlopen_and_nonzero_c_int_initializer_result_remain_failures() {
    let mut calls = calls();
    let code = code(calls.guard());
    calls.start_dlopen().unwrap();
    assert_eq!(
        calls.dlopen_returned(&code, &returned(&code), &code.bytes, 0),
        Err(Refusal::Return)
    );
    let (mut calls, code) = ready();
    assert_eq!(
        calls.initializer_returned(&code, &returned(&code), &code.bytes, 1),
        Err(Refusal::Return)
    );
    let (mut calls, code) = ready();
    let rax = 0xffff_ffff_0000_0000_u64;
    calls
        .initializer_returned(&code, &returned(&code), &code.bytes, rax as u32)
        .unwrap();
}

#[test]
fn entry_bytes_and_call_address_bounds_are_exact() {
    let guard = guard();
    for i in 0..8 {
        let mut word = guard.guarded_word();
        word[i] ^= 1;
        assert_eq!(
            guard.authenticate(&entry(), word),
            Err(Refusal::Instruction)
        );
    }
    assert!(CallCode::prepare(&guard, u64::MAX - 29, 0x900000, 1, 1).is_err());
    assert!(CallCode::prepare(&guard, 1, 0x900008, 1, 1).is_err());
    assert!(CallCode::prepare(&guard, 1, 0x900000, 0, 1).is_err());
    assert_eq!(
        RawClockInterval {
            before: 8,
            after: 19
        }
        .delta(),
        Ok(11)
    );
    assert_eq!(
        RawClockInterval {
            before: 19,
            after: 8
        }
        .delta(),
        Err(Refusal::CounterWentBackwards)
    );
}
