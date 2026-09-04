//! Pure, allocation-free verification of the finite public instruction fixture.
//!
//! The rev19 native attempt showed why this cannot be ordinary `assert_eq!` over
//! `Vec`: while SUD is armed, a failing assertion unwound, the panic machinery
//! reported `failed to initiate panic`, a normal Tool injection reentered, an
//! allocation failed, and the guest never terminated until the deadline killed it.
//!
//! So verification here is a pure function over fixed arrays with no allocator
//! traffic, and reporting is a bounded sink written through the existing trusted
//! raw gate. Every original check is kept, in the same order, with the same strict
//! values. Only the mechanism changed: a failure is terminal and names itself
//! instead of unwinding.
//!
//! The module is shared with the host tests through the crate's existing `#[path]`
//! pattern, so the goldens and the rejection controls exercise this exact code.

use core::fmt::Write;

use reverie_preload::trap::raw_syscall6;

pub const BRANCHES: u32 = 4;
pub const READ_BYTES: usize = 16;
pub const EVENTS_EXPECTED: u64 = 6;
pub const RDTSC_ECX_SEED: u64 = 0x3333_3333;

pub const KIND_GETPID: u64 = 1;
pub const KIND_CPUID: u64 = 2;
pub const KIND_RDTSC: u64 = 3;
pub const KIND_RDTSCP: u64 = 4;
pub const KIND_READ: u64 = 5;

/// Exit status of a terminal verification failure. Distinct from the installer's
/// `42 -> 127` refusal and from the runtime's own `125`/`126` statuses.
pub const FAILURE_STATUS: u64 = 70;

/// Exit status when verification passed but its success record could not be
/// written. An unwritten record is not a success, and it gets its own identity
/// rather than being folded into a check failure.
pub const OUTPUT_FAILURE_STATUS: u64 = 71;

/// Capacity of the detail rendering. Sized so the widest report — twelve `u64`
/// values twice — cannot truncate; a host control proves that bound.
pub const DETAIL_CAPACITY: usize = 448;

/// Capacity of a whole emitted record line.
pub const LINE_CAPACITY: usize = 512;

/// The expected typed instruction results, `RESULTS[1..13]`.
///
/// Index 6 is the RCX value observed after `RDTSC`. `RDTSC` does not write ECX, so
/// this is the sentinel the guest seeds immediately before it; it stays **nonzero**
/// and is not relaxed to accept the zero the unseeded body produced.
pub const EXPECTED_TYPED: [u64; 12] = [
    0xfeed ^ 0xbaad,
    0x2222_2222,
    0x3333_3333,
    0x4444_4444,
    3,
    0x2222_2222,
    RDTSC_ECX_SEED,
    0xfedc_ba98,
    4,
    0x2222_2222,
    0xaaaa_5555,
    0xfedc_ba98,
];

pub const EXPECTED_KINDS: [u64; 6] = [
    KIND_GETPID,
    KIND_CPUID,
    KIND_RDTSC,
    KIND_RDTSCP,
    KIND_READ,
    KIND_GETPID,
];

/// A fixed-capacity `core::fmt::Write` sink. Formatting integers and slices of
/// integers does not allocate, so nothing here reaches the allocator.
pub struct Bounded<const N: usize> {
    bytes: [u8; N],
    used: usize,
    truncated: bool,
}

impl<const N: usize> Default for Bounded<N> {
    fn default() -> Self {
        Self {
            bytes: [0; N],
            used: 0,
            truncated: false,
        }
    }
}

impl<const N: usize> Bounded<N> {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.used]
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

impl<const N: usize> Write for Bounded<N> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let room = N - self.used;
        let take = text.len().min(room);
        self.bytes[self.used..self.used + take].copy_from_slice(&text.as_bytes()[..take]);
        self.used += take;
        if take < text.len() {
            self.truncated = true;
        }
        Ok(())
    }
}

/// Everything the verifier reads, captured once as plain data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Observation {
    pub starts: u64,
    pub events: u64,
    pub injections: u64,
    pub kinds: [u64; 8],
    pub clocks: [u64; 8],
    pub results: [u64; 15],
    pub buffer: [u8; READ_BYTES],
    pub stats: [u64; 4],
    pub inventory: u64,
    pub pid: u64,
}

/// The rendered actual-versus-expected detail of a rejecting check.
pub type Detail = Bounded<DETAIL_CAPACITY>;

/// A whole emitted record.
pub type Line = Bounded<LINE_CAPACITY>;

/// The name of the check that rejected.
///
/// The detail is rendered into caller-owned storage rather than carried in the
/// `Err` variant, so the result stays small and nothing is boxed. Boxing is
/// refused deliberately: it would allocate on the failure path, which is exactly
/// what the rev19 native attempt showed must not happen while SUD is armed.
pub type Rejection = &'static str;

/// Every original check, in the original order, with the original strict values.
///
/// The detail of a rejection is rendered into `detail`, so the error value stays a
/// borrowed name and nothing is allocated or boxed on the failure path.
pub fn check(observation: &Observation, detail: &mut Detail) -> Result<(), Rejection> {
    let count = observation.events;
    if observation.starts != 1 {
        let _ = write!(detail, "actual={} expected=1", observation.starts);
        return Err("thread-start-count");
    }
    if count != EVENTS_EXPECTED {
        let _ = write!(detail, "actual={count} expected={EVENTS_EXPECTED}");
        return Err("event-count");
    }
    if observation.injections != 3 {
        let _ = write!(detail, "actual={} expected=3", observation.injections);
        return Err("injection-count");
    }
    let kinds = &observation.kinds[..count as usize];
    if kinds != EXPECTED_KINDS {
        let _ = write!(detail, "actual={kinds:?} expected={EXPECTED_KINDS:?}");
        return Err("event-kinds");
    }
    let clocks = &observation.clocks[..count as usize];
    let branches = u64::from(BRANCHES);
    let mut expected_clocks = [0u64; 8];
    for (index, slot) in expected_clocks.iter_mut().enumerate().take(count as usize) {
        *slot = index as u64 * branches;
    }
    let expected_clocks = &expected_clocks[..count as usize];
    if clocks != expected_clocks {
        let _ = write!(detail, "actual={clocks:?} expected={expected_clocks:?}");
        return Err("full-owned-clock-sequence");
    }
    if observation.results[0] != observation.pid {
        let _ = write!(
            detail,
            "actual={} expected={}",
            observation.results[0], observation.pid
        );
        return Err("first-getpid-result");
    }
    if observation.results[14] != observation.pid {
        let _ = write!(
            detail,
            "actual={} expected={}",
            observation.results[14], observation.pid
        );
        return Err("second-getpid-result");
    }
    if observation.results[13] != READ_BYTES as u64 {
        let _ = write!(
            detail,
            "actual={} expected={}",
            observation.results[13], READ_BYTES
        );
        return Err("read-return-length");
    }
    let typed = &observation.results[1..13];
    if typed != EXPECTED_TYPED {
        let _ = write!(detail, "actual={typed:?} expected={EXPECTED_TYPED:?}");
        return Err("typed-instruction-results");
    }
    if observation.buffer != [0x5a; READ_BYTES] {
        let _ = write!(
            detail,
            "actual={:?} expected=[0x5a; {READ_BYTES}]",
            observation.buffer
        );
        return Err("read-buffer-bytes");
    }
    if observation.stats != [0, 0, 0, 0] {
        let _ = write!(
            detail,
            "actual={:?} expected=[0, 0, 0, 0]",
            observation.stats
        );
        return Err("patch-and-rewrite-stats");
    }
    Ok(())
}

/// The success record. Its text and fields are unchanged from the original.
pub fn success_line(observation: &Observation) -> Line {
    let mut out = Line::default();
    let kinds = &observation.kinds[..observation.events as usize];
    let clocks = &observation.clocks[..observation.events as usize];
    let typed = &observation.results[1..13];
    let _ = writeln!(
        out,
        "owned-public: events={} injections=3 inventory={} kinds={kinds:?} clocks={clocks:?} instructions={typed:x?}",
        observation.events, observation.inventory
    );
    out
}

/// The terminal failure record, naming the check with its actual and expected.
pub fn failure_line(check: Rejection, detail: &Detail) -> Line {
    let mut out = Line::default();
    let _ = writeln!(
        out,
        "owned-public FAILED: check={check} {}",
        core::str::from_utf8(detail.as_bytes()).unwrap_or("<non-utf8>")
    );
    out
}

/// Write through the existing trusted raw gate, bounded in attempts.
///
/// Returns whether every byte reached the descriptor, so a failed diagnostic is
/// reported rather than silently treated as written.
pub fn raw_write(fd: u64, bytes: &[u8]) -> bool {
    let mut offset = 0;
    let mut attempts = 0;
    while offset < bytes.len() {
        if attempts >= 64 {
            return false;
        }
        attempts += 1;
        let written = unsafe {
            raw_syscall6(
                libc::SYS_write,
                [
                    fd,
                    bytes.as_ptr() as u64 + offset as u64,
                    (bytes.len() - offset) as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        if written <= 0 {
            return false;
        }
        offset += written as usize;
    }
    true
}

/// The status the whole terminal chain would exit with, and the records it emits.
///
/// Split out from [`terminate_on`] only so a host control can observe the decision
/// without ending its own process; the guest path runs the same function.
pub fn decide(observation: &Observation, out_fd: u64, err_fd: u64) -> u64 {
    let mut detail = Detail::default();
    match check(observation, &mut detail) {
        Ok(()) => {
            let line = success_line(observation);
            if !line.truncated() && raw_write(out_fd, line.as_bytes()) {
                return 0;
            }
            let mut note = Line::default();
            let _ = writeln!(
                note,
                "owned-public FAILED: check=success-record-output actual=unwritten expected=written"
            );
            let _unreportable = raw_write(err_fd, note.as_bytes());
            OUTPUT_FAILURE_STATUS
        }
        Err(rejection) => {
            let line = failure_line(rejection, &detail);
            let _unreportable = raw_write(err_fd, line.as_bytes());
            FAILURE_STATUS
        }
    }
}

/// Check, emit the bounded record through the raw gate, and exit. Never returns,
/// never panics, never unwinds, and never issues an ordinary syscall.
///
/// This is the whole terminal chain, and it is the only one. The guest calls it
/// with standard output and standard error; the host controls call the same
/// function on a controlled observation, so the exercised code is this function
/// and not a copy of it.
pub fn terminate_on(observation: &Observation, out_fd: u64, err_fd: u64) -> ! {
    let status = decide(observation, out_fd, err_fd);
    unsafe {
        raw_syscall6(libc::SYS_exit_group, [status, 0, 0, 0, 0, 0]);
        core::arch::asm!("ud2", options(noreturn));
    }
}
