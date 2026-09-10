use std::io;
pub(crate) mod provider;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use liteinst2::trampoline::HookContext;

#[repr(C)]
struct InitialRecord {
    capture: *const u8,
    loader_entry: u64,
    fault_site: u64,
}

unsafe extern "C" {
    #[linkage = "extern_weak"]
    static pl_take_initial: Option<unsafe extern "C" fn() -> *const InitialRecord>;
}

static REQUESTED: AtomicBool = AtomicBool::new(false);
static INITIAL_MASK: OnceLock<u64> = OnceLock::new();

/// Prepare the installed root Tool's one-use private-CRT fault transition.
///
/// # Safety
/// Only the retained private CRT owner may call this, after the real shared
/// Tool constructor and before any guest instruction. The compiled lifecycle
/// component must supply `pl_take_initial`; no caller supplies a readiness bit,
/// synthetic frame or register image through this API. This does not complete
/// ThreadStart/post-exec or authorize an unobserved transfer.
pub unsafe fn prepare() -> io::Result<()> {
    if !crate::owned_context::tls::available() || unsafe { pl_take_initial }.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private CRT provider unavailable",
        ));
    }
    let mask = crate::owned_context::prepare_initial()?;
    crate::tool_host::prepare_initial()?;
    INITIAL_MASK
        .set(mask)
        .map_err(|_| io::Error::other("initial guest mask already retained"))?;
    REQUESTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| io::Error::other("initial Tool transition already requested"))?;
    Ok(())
}

fn word(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn matches_registers(capture: &[u8], registers: &HookContext) -> bool {
    if capture.len() < 320
        || word(capture, 0) != 1
        || word(capture, 8) != 4416
        || word(capture, 16) != 31
        || word(capture, 24) != 0
    {
        return false;
    }
    let actual = [
        registers.rax,
        registers.rbx,
        registers.rcx,
        registers.rdx,
        registers.rsi,
        registers.rdi,
        registers.rbp,
        registers.stack_pointer,
        registers.r8,
        registers.r9,
        registers.r10,
        registers.r11,
        registers.r12,
        registers.r13,
        registers.r14,
        registers.r15,
    ];
    actual
        .iter()
        .enumerate()
        .all(|(index, value)| *value == word(capture, 40 + index * 8))
        && word(capture, 168) & (0x100 | 0x10000 | 0x20000) == 0
        && registers.rflags == word(capture, 168) | 0x10000
}

pub(crate) unsafe fn capture(
    signal: i32,
    registers: &HookContext,
    fp: &[u8],
    guest_mask: u64,
) -> Option<u64> {
    if !REQUESTED.swap(false, Ordering::AcqRel) {
        return None;
    }
    if !initial_mask_matches(INITIAL_MASK.get().copied(), guest_mask) {
        crate::owned_context::refuse();
    }
    let take_initial = unsafe { pl_take_initial }.unwrap_or_else(|| crate::owned_context::refuse());
    let record =
        unsafe { take_initial().as_ref() }.unwrap_or_else(|| crate::owned_context::refuse());
    if record.capture.is_null()
        || signal != libc::SIGSEGV
        || registers.instruction_pointer != record.fault_site
        || record.loader_entry == 0
        || record.loader_entry >= 1 << 47
    {
        crate::owned_context::refuse();
    }
    let capture = unsafe { std::slice::from_raw_parts(record.capture, 4416) };
    if !matches_registers(capture, registers) || !matches_fp(capture, fp) {
        if let Some(mismatch) = first_mismatch(capture, registers, fp) {
            mismatch.report_with(|field, detail, value| {
                reverie_preload::trap::report_terminal126(field, detail, Some(value));
            });
        }
        crate::owned_context::refuse();
    }
    Some(record.loader_entry)
}

fn initial_mask_matches(retained: Option<u64>, observed: u64) -> bool {
    retained == Some(observed)
}

fn matches_fp(capture: &[u8], fp: &[u8]) -> bool {
    first_fp_mismatch(capture, fp).is_none()
}

#[derive(Debug, PartialEq, Eq)]
struct InitialMismatch {
    field: &'static str,
    index: usize,
    expected: u64,
    observed: u64,
    raw_flags: Option<u64>,
}

impl InitialMismatch {
    fn new(field: &'static str, index: usize, expected: u64, observed: u64) -> Self {
        Self {
            field,
            index,
            expected,
            observed,
            raw_flags: None,
        }
    }

    fn report_with(&self, mut report: impl FnMut(&'static str, &'static str, i64)) {
        report(self.field, "index", self.index as i64);
        report(self.field, "expected-high32", (self.expected >> 32) as i64);
        report(
            self.field,
            "expected-low32",
            i64::from(self.expected as u32),
        );
        report(self.field, "observed-high32", (self.observed >> 32) as i64);
        report(
            self.field,
            "observed-low32",
            i64::from(self.observed as u32),
        );
        if let Some(flags) = self.raw_flags {
            report(self.field, "raw-flags-high32", (flags >> 32) as i64);
            report(self.field, "raw-flags-low32", i64::from(flags as u32));
        }
    }
}

fn first_mismatch(capture: &[u8], registers: &HookContext, fp: &[u8]) -> Option<InitialMismatch> {
    if capture.len() < 320 {
        return Some(InitialMismatch::new(
            "initial/capture-min-length",
            0,
            320,
            capture.len() as u64,
        ));
    }
    for (field, offset, expected) in [
        ("initial/version", 0, 1),
        ("initial/record-size", 8, 4416),
        ("initial/valid", 16, 31),
        ("initial/failure", 24, 0),
    ] {
        let observed = word(capture, offset);
        if observed != expected {
            return Some(InitialMismatch::new(field, offset, expected, observed));
        }
    }
    for (index, (field, observed)) in [
        ("initial/rax", registers.rax),
        ("initial/rbx", registers.rbx),
        ("initial/rcx", registers.rcx),
        ("initial/rdx", registers.rdx),
        ("initial/rsi", registers.rsi),
        ("initial/rdi", registers.rdi),
        ("initial/rbp", registers.rbp),
        ("initial/rsp", registers.stack_pointer),
        ("initial/r8", registers.r8),
        ("initial/r9", registers.r9),
        ("initial/r10", registers.r10),
        ("initial/r11", registers.r11),
        ("initial/r12", registers.r12),
        ("initial/r13", registers.r13),
        ("initial/r14", registers.r14),
        ("initial/r15", registers.r15),
    ]
    .into_iter()
    .enumerate()
    {
        let offset = 40 + index * 8;
        let expected = word(capture, offset);
        if observed != expected {
            return Some(InitialMismatch::new(field, offset, expected, observed));
        }
    }
    let flags = word(capture, 168);
    let forbidden = flags & (0x100 | 0x10000 | 0x20000);
    if forbidden != 0 {
        return Some(InitialMismatch {
            raw_flags: Some(flags),
            ..InitialMismatch::new("initial/captured-rflags-forbidden-bits", 168, 0, forbidden)
        });
    }
    if registers.rflags != flags | 0x10000 {
        return Some(InitialMismatch::new(
            "initial/signal-rflags",
            168,
            flags | 0x10000,
            registers.rflags,
        ));
    }
    first_fp_mismatch(capture, fp)
}

fn first_fp_mismatch(capture: &[u8], fp: &[u8]) -> Option<InitialMismatch> {
    if capture.len() < 320 + 2440 {
        return Some(InitialMismatch::new(
            "initial/capture-fp-min-length",
            320,
            320 + 2440,
            capture.len() as u64,
        ));
    }
    if fp.len() != 2444 {
        return Some(InitialMismatch::new(
            "initial/signal-fp-length",
            0,
            2444,
            fp.len() as u64,
        ));
    }
    for (field, offset, expected) in [
        ("initial/xcr0", 216, 0x2e7),
        ("initial/xstate-size", 224, 2440),
    ] {
        let observed = word(capture, offset);
        if observed != expected {
            return Some(InitialMismatch::new(field, offset, expected, observed));
        }
    }
    for (offset, expected) in [
        (464, 0x46505853),
        (468, 2444),
        (480, 2440),
        (2440, 0x46505845),
    ] {
        let observed = u32::from_le_bytes(fp[offset..offset + 4].try_into().unwrap());
        if observed != expected {
            return Some(InitialMismatch::new(
                "initial/signal-xsave-metadata",
                offset,
                u64::from(expected),
                u64::from(observed),
            ));
        }
    }
    if word(fp, 472) != 0x2e7 {
        return Some(InitialMismatch::new(
            "initial/signal-xfeatures",
            472,
            0x2e7,
            word(fp, 472),
        ));
    }
    let captured_bv = word(capture, 320 + 512);
    if captured_bv & !0x2e7 != 0 {
        return Some(InitialMismatch::new(
            "initial/captured-xstate-bv-allowed",
            512,
            0x2e7,
            captured_bv,
        ));
    }
    for offset in 520..576 {
        if capture[320 + offset] != 0 {
            return Some(InitialMismatch::new(
                "initial/captured-xsave-header-byte",
                offset,
                0,
                u64::from(capture[320 + offset]),
            ));
        }
    }
    for offset in (484..512).chain(520..576) {
        if fp[offset] != 0 {
            return Some(InitialMismatch::new(
                "initial/signal-xsave-reserved-byte",
                offset,
                0,
                u64::from(fp[offset]),
            ));
        }
    }
    let expected_bv = captured_bv | 3;
    let observed_bv = word(fp, 512);
    if observed_bv != expected_bv {
        return Some(InitialMismatch::new(
            "initial/signal-xstate-bv",
            512,
            expected_bv,
            observed_bv,
        ));
    }
    for offset in (0..464).chain(576..2440) {
        let expected = capture[320 + offset];
        let observed = fp[offset];
        if observed != expected {
            return Some(InitialMismatch::new(
                "initial/fp-byte",
                offset,
                u64::from(expected),
                u64::from(observed),
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn unavailable_initial_record() -> *const InitialRecord {
        std::ptr::null()
    }

    std::arch::global_asm!(
        ".weak pl_take_initial",
        ".set pl_take_initial, {take_initial}",
        take_initial = sym unavailable_initial_record,
    );

    #[test]
    fn unit_support_never_supplies_an_initial_record() {
        for _ in 0..2 {
            assert!(unsafe { pl_take_initial.unwrap()() }.is_null());
        }
    }

    fn matching_capture() -> (Vec<u8>, HookContext, Vec<u8>) {
        let mut capture = vec![0u8; 4416];
        for (offset, value) in [
            (0, 1),
            (8, 4416),
            (16, 31),
            (168, 0x202),
            (216, 0x2e7),
            (224, 2440),
        ] {
            put(&mut capture, offset, value);
        }
        let registers = HookContext {
            rflags: 0x10202,
            ..unsafe { std::mem::zeroed() }
        };
        let mut fp = vec![0; 2444];
        for (offset, value) in [
            (464, 0x46505853),
            (468, 2444),
            (480, 2440),
            (2440, 0x46505845),
        ] {
            fp[offset..offset + 4].copy_from_slice(&u32::to_le_bytes(value));
        }
        put(&mut fp, 472, 0x2e7);
        put(&mut fp, 512, 3);
        (capture, registers, fp)
    }

    fn checked_mismatch(
        capture: &[u8],
        registers: &HookContext,
        fp: &[u8],
    ) -> Option<InitialMismatch> {
        let mismatch = first_mismatch(capture, registers, fp);
        assert_eq!(
            mismatch.is_none(),
            matches_registers(capture, registers) && matches_fp(capture, fp)
        );
        mismatch
    }

    #[test]
    fn diagnostic_matches_admitted_contract_and_header_categories() {
        let (capture, registers, fp) = matching_capture();
        assert!(checked_mismatch(&capture, &registers, &fp).is_none());
        for (field, offset, expected) in [
            ("initial/version", 0, 1),
            ("initial/record-size", 8, 4416),
            ("initial/valid", 16, 31),
            ("initial/failure", 24, 0),
            ("initial/xcr0", 216, 0x2e7),
            ("initial/xstate-size", 224, 2440),
        ] {
            let mut changed = capture.clone();
            put(&mut changed, offset, expected ^ 1);
            assert_eq!(
                checked_mismatch(&changed, &registers, &fp),
                Some(InitialMismatch::new(field, offset, expected, expected ^ 1))
            );
        }
        for (length, field, index, minimum) in [
            (319, "initial/capture-min-length", 0, 320),
            (2759, "initial/capture-fp-min-length", 320, 2760),
        ] {
            assert_eq!(
                checked_mismatch(&capture[..length], &registers, &fp),
                Some(InitialMismatch::new(field, index, minimum, length as u64))
            );
        }
        assert_eq!(
            checked_mismatch(&capture, &registers, &fp[..2439]),
            Some(InitialMismatch::new(
                "initial/signal-fp-length",
                0,
                2444,
                2439
            ))
        );
    }

    #[test]
    fn diagnostic_names_each_gpr_and_preserves_full_flags() {
        let (capture, mut registers, fp) = matching_capture();
        for (index, field) in [
            "initial/rax",
            "initial/rbx",
            "initial/rcx",
            "initial/rdx",
            "initial/rsi",
            "initial/rdi",
            "initial/rbp",
            "initial/rsp",
            "initial/r8",
            "initial/r9",
            "initial/r10",
            "initial/r11",
            "initial/r12",
            "initial/r13",
            "initial/r14",
            "initial/r15",
        ]
        .into_iter()
        .enumerate()
        {
            let mut changed = capture.clone();
            let offset = 40 + index * 8;
            put(&mut changed, offset, u64::MAX - index as u64);
            assert_eq!(
                checked_mismatch(&changed, &registers, &fp),
                Some(InitialMismatch::new(
                    field,
                    offset,
                    u64::MAX - index as u64,
                    0
                ))
            );
        }
        let distinct = HookContext {
            rax: 1,
            rbx: 2,
            rcx: 3,
            rdx: 4,
            rsi: 5,
            rdi: 6,
            rbp: 7,
            stack_pointer: 8,
            r8: 9,
            r9: 10,
            r10: 11,
            r11: 12,
            r12: 13,
            r13: 14,
            r14: 15,
            r15: 16,
            rflags: 0x10202,
            ..unsafe { std::mem::zeroed() }
        };
        let mut changed = capture.clone();
        for index in 0..16 {
            let offset = 40 + index * 8;
            let mismatch = checked_mismatch(&changed, &distinct, &fp).unwrap();
            assert_eq!(
                (mismatch.index, mismatch.expected, mismatch.observed),
                (offset, 0, index as u64 + 1)
            );
            put(&mut changed, offset, index as u64 + 1);
        }
        assert!(checked_mismatch(&changed, &distinct, &fp).is_none());
        for forbidden in [0x100, 0x10000, 0x20000, 0x30100] {
            let mut changed = capture.clone();
            put(&mut changed, 168, 0x202 | forbidden);
            assert_eq!(
                checked_mismatch(&changed, &registers, &fp),
                Some(InitialMismatch {
                    raw_flags: Some(0x202 | forbidden),
                    ..InitialMismatch::new(
                        "initial/captured-rflags-forbidden-bits",
                        168,
                        0,
                        forbidden
                    )
                })
            );
        }
        registers.rflags = 0x202;
        assert_eq!(
            checked_mismatch(&capture, &registers, &fp),
            Some(InitialMismatch::new(
                "initial/signal-rflags",
                168,
                0x10202,
                0x202
            ))
        );
    }

    #[test]
    fn diagnostic_checks_every_payload_byte_and_register_priority() {
        let (mut capture, registers, mut fp) = matching_capture();
        for index in (0..464).chain(576..2440) {
            fp[index] = 0x80;
            assert_eq!(
                checked_mismatch(&capture, &registers, &fp),
                Some(InitialMismatch::new("initial/fp-byte", index, 0, 0x80))
            );
            fp[index] = 0;
        }
        assert!(checked_mismatch(&capture, &registers, &fp).is_none());
        fp[512] = 1;
        put(&mut capture, 40, 7);
        assert_eq!(
            checked_mismatch(&capture, &registers, &fp),
            Some(InitialMismatch::new("initial/rax", 40, 7, 0))
        );
        put(&mut capture, 40, 0);
        fp[512] = 3;
        fp[463] = 2;
        assert_eq!(
            checked_mismatch(&capture, &registers, &fp),
            Some(InitialMismatch::new("initial/fp-byte", 463, 0, 2))
        );
    }

    #[test]
    fn linux_materializes_exactly_the_legacy_bits_without_editing_buffers() {
        for bitmap in (0..=0x2e7).filter(|bitmap| bitmap & !0x2e7 == 0) {
            let (mut capture, registers, mut fp) = matching_capture();
            put(&mut capture, 320 + 512, bitmap);
            put(&mut fp, 512, bitmap | 3);
            let original_capture = capture.clone();
            let original_fp = fp.clone();
            assert!(checked_mismatch(&capture, &registers, &fp).is_none());
            assert_eq!(capture, original_capture);
            assert_eq!(fp, original_fp);
            if bitmap & 3 != 3 {
                put(&mut fp, 512, bitmap);
                assert_eq!(
                    checked_mismatch(&capture, &registers, &fp),
                    Some(InitialMismatch::new(
                        "initial/signal-xstate-bv",
                        512,
                        bitmap | 3,
                        bitmap
                    ))
                );
            }
            for bit in 0..64 {
                let changed = (bitmap | 3) ^ (1 << bit);
                put(&mut fp, 512, changed);
                assert_eq!(
                    checked_mismatch(&capture, &registers, &fp),
                    Some(InitialMismatch::new(
                        "initial/signal-xstate-bv",
                        512,
                        bitmap | 3,
                        changed
                    ))
                );
            }
        }
    }

    #[test]
    fn malformed_and_unsupported_xsave_layouts_do_not_become_equivalent() {
        let (capture, registers, fp) = matching_capture();
        for bit in (0..64).filter(|bit| (1u64 << bit) & 0x2e7 == 0) {
            let mut changed_capture = capture.clone();
            let mut changed_fp = fp.clone();
            put(&mut changed_capture, 320 + 512, 1 << bit);
            put(&mut changed_fp, 512, (1 << bit) | 3);
            assert_eq!(
                checked_mismatch(&changed_capture, &registers, &changed_fp),
                Some(InitialMismatch::new(
                    "initial/captured-xstate-bv-allowed",
                    512,
                    0x2e7,
                    1 << bit
                ))
            );
        }
        for index in 520..576 {
            let mut changed_capture = capture.clone();
            let mut changed_fp = fp.clone();
            changed_capture[320 + index] = 0x80;
            changed_fp[index] = 0x80;
            assert_eq!(
                checked_mismatch(&changed_capture, &registers, &changed_fp),
                Some(InitialMismatch::new(
                    "initial/captured-xsave-header-byte",
                    index,
                    0,
                    0x80
                ))
            );
            assert_eq!(
                checked_mismatch(&capture, &registers, &changed_fp),
                Some(InitialMismatch::new(
                    "initial/signal-xsave-reserved-byte",
                    index,
                    0,
                    0x80
                ))
            );
        }
        for index in (464..512).chain(2440..2444) {
            let mut changed = fp.clone();
            changed[index] ^= 0x80;
            assert!(
                checked_mismatch(&capture, &registers, &changed).is_some(),
                "metadata byte {index}"
            );
        }
        for length in [0, 512, 2440, 2443, 2445] {
            let mut changed = fp.clone();
            changed.resize(length, 0);
            assert_eq!(
                checked_mismatch(&capture, &registers, &changed),
                Some(InitialMismatch::new(
                    "initial/signal-fp-length",
                    0,
                    2444,
                    length as u64
                ))
            );
        }
    }

    #[test]
    fn legacy_materialization_never_exempts_payload_or_mxcsr_bytes() {
        for legacy in 0..4 {
            let (mut capture, registers, mut fp) = matching_capture();
            put(&mut capture, 320 + 512, legacy);
            for index in (0..464).chain(576..2440) {
                fp[index] ^= 1;
                assert_eq!(
                    checked_mismatch(&capture, &registers, &fp),
                    Some(InitialMismatch::new("initial/fp-byte", index, 0, 1))
                );
                fp[index] ^= 1;
                capture[320 + index] ^= 1;
                assert_eq!(
                    checked_mismatch(&capture, &registers, &fp),
                    Some(InitialMismatch::new("initial/fp-byte", index, 1, 0))
                );
                capture[320 + index] ^= 1;
            }
            assert!(checked_mismatch(&capture, &registers, &fp).is_none());
        }
    }

    #[test]
    fn bitmap_diagnostic_reports_the_full_linux_expected_value() {
        let (mut capture, registers, mut fp) = matching_capture();
        put(&mut capture, 320 + 512, 0x2e4);
        put(&mut fp, 512, 0x8000_0000_0000_02e7);
        let mismatch = checked_mismatch(&capture, &registers, &fp).unwrap();
        let mut records = Vec::new();
        mismatch.report_with(|field, detail, value| records.push((field, detail, value)));
        assert_eq!(
            records,
            [
                ("initial/signal-xstate-bv", "index", 512),
                ("initial/signal-xstate-bv", "expected-high32", 0),
                ("initial/signal-xstate-bv", "expected-low32", 0x2e7),
                ("initial/signal-xstate-bv", "observed-high32", 0x8000_0000),
                ("initial/signal-xstate-bv", "observed-low32", 0x2e7),
            ]
        );
    }

    #[test]
    fn diagnostic_reporter_fields_and_values_are_exact_without_signed_loss() {
        let mismatch = InitialMismatch {
            raw_flags: Some(0x302),
            ..InitialMismatch::new(
                "initial/test",
                168,
                0xfedc_ba98_7654_3210,
                0x8123_4567_89ab_cdef,
            )
        };
        let mut records = Vec::new();
        mismatch.report_with(|field, detail, value| records.push((field, detail, value)));
        assert_eq!(
            records,
            [
                ("initial/test", "index", 168),
                ("initial/test", "expected-high32", 0xfedc_ba98),
                ("initial/test", "expected-low32", 0x7654_3210),
                ("initial/test", "observed-high32", 0x8123_4567),
                ("initial/test", "observed-low32", 0x89ab_cdef),
                ("initial/test", "raw-flags-high32", 0),
                ("initial/test", "raw-flags-low32", 0x302),
            ]
        );
        let (capture, registers, fp) = matching_capture();
        records.clear();
        if let Some(mismatch) = checked_mismatch(&capture, &registers, &fp) {
            mismatch.report_with(|field, detail, value| records.push((field, detail, value)));
        }
        assert!(records.is_empty());
    }

    #[test]
    fn initial_fault_requires_exact_retained_guest_mask() {
        for mask in [0, 1 << (libc::SIGUSR1 - 1)] {
            assert!(initial_mask_matches(Some(mask), mask));
            assert!(!initial_mask_matches(
                Some(mask),
                mask ^ (1 << (libc::SIGUSR1 - 1))
            ));
            assert!(!initial_mask_matches(
                Some(mask),
                mask | (1 << (libc::SIGSEGV - 1))
            ));
            assert!(!initial_mask_matches(None, mask));
        }
    }

    fn put(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn initial_register_validation_checks_every_retained_gpr() {
        let mut capture = [0u8; 320];
        for (offset, value) in [(0, 1), (8, 4416), (16, 31), (168, 0x202)] {
            put(&mut capture, offset, value);
        }
        for index in 0..16 {
            put(&mut capture, 40 + index * 8, index as u64 + 1);
        }
        let registers = HookContext {
            rax: 1,
            rbx: 2,
            rcx: 3,
            rdx: 4,
            rsi: 5,
            rdi: 6,
            rbp: 7,
            stack_pointer: 8,
            r8: 9,
            r9: 10,
            r10: 11,
            r11: 12,
            r12: 13,
            r13: 14,
            r14: 15,
            r15: 16,
            rflags: 0x10202,
            ..unsafe { std::mem::zeroed() }
        };
        assert!(matches_registers(&capture, &registers));
        for index in 0..16 {
            capture[40 + index * 8] ^= 0x80;
            assert!(!matches_registers(&capture, &registers));
            capture[40 + index * 8] ^= 0x80;
        }
        for flags in [0x302, 0x10202, 0x20202, 0x203] {
            put(&mut capture, 168, flags);
            assert!(!matches_registers(&capture, &registers));
        }
        assert!(!matches_registers(&capture[..319], &registers));
    }

    #[test]
    fn initial_fp_validation_retains_xstate_bv_and_every_component_byte() {
        let mut capture = [0u8; 4416];
        put(&mut capture, 216, 0x2e7);
        put(&mut capture, 224, 2440);
        for index in (0..464).chain(576..2440) {
            capture[320 + index] = (index % 251) as u8;
        }
        let (_, _, mut frame) = matching_capture();
        for index in (0..464).chain(576..2440) {
            frame[index] = capture[320 + index];
        }
        assert!(matches_fp(&capture, &frame));
        for index in (0..464).chain(512..2440) {
            frame[index] ^= 0x80;
            assert!(!matches_fp(&capture, &frame), "lost byte {index}");
            frame[index] ^= 0x80;
        }
        frame[512] ^= 0x80;
        assert!(!matches_fp(&capture, &frame));
        frame[512] ^= 0x80;
        frame[464..512].fill(0xa5);
        assert!(!matches_fp(&capture, &frame));
        frame[464..512].copy_from_slice(&matching_capture().2[464..512]);
        assert!(matches_fp(&capture, &frame));
        assert!(!matches_fp(&capture, &frame[..2439]));
        assert!(!matches_fp(&capture[..319], &frame));
        put(&mut capture, 216, 7);
        assert!(!matches_fp(&capture, &frame));
    }
}
