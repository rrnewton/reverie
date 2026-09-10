//! Finite real SUD qualification, first unarmed, then armed cancellation.

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Guest;
use reverie::Subscription;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;

use super::Floating;
use super::mask;
use super::native_controls;
use super::raw_syscall6;

static ARMED: AtomicBool = AtomicBool::new(false);
static WORK: AtomicU64 = AtomicU64::new(0);
static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static INJECTIONS: AtomicU64 = AtomicU64::new(0);
static CLOCKS: [AtomicU64; 5] = [const { AtomicU64::new(u64::MAX) }; 5];
pub(super) static FD: AtomicU64 = AtomicU64::new(0);
pub(super) static BUFFER: AtomicU64 = AtomicU64::new(0);
pub(super) static RESULTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

#[repr(C)]
pub(super) struct Probe {
    path: PathBuf,
    evidence: PathBuf,
    evidence_file: std::fs::File,
    mask: u64,
    controls: [u64; 7],
    inventory: Option<reverie_liteinst::PosixTimerInventory>,
    text: Vec<u8>,
    descriptors: [i32; 2],
    pid: i64,
    buffer: [u8; 55],
    pub(super) registers: [u64; 18],
    pub(super) before: Floating,
    pub(super) after: Floating,
}

unsafe extern "C" {
    fn owned_syscall_entry(probe: *mut libc::c_void) -> !;
    fn owned_syscall_first();
    fn owned_syscall_first_return();
    fn owned_syscall_one();
    fn owned_syscall_read();
    fn owned_syscall_read_return();
    fn owned_syscall_interrupt();
    fn owned_syscall_interrupt_return();
    fn owned_syscall_two();
    fn owned_syscall_end();
}

fn sites() -> [u64; 8] {
    [
        owned_syscall_first as *const () as u64,
        owned_syscall_first_return as *const () as u64,
        owned_syscall_one as *const () as u64,
        owned_syscall_read as *const () as u64,
        owned_syscall_read_return as *const () as u64,
        owned_syscall_interrupt as *const () as u64,
        owned_syscall_interrupt_return as *const () as u64,
        owned_syscall_two as *const () as u64,
    ]
}

fn work() {
    for iteration in 0..WORK.load(Ordering::Relaxed) {
        std::hint::black_box(iteration);
    }
}

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        work();
    }
}

#[derive(Default)]
struct SyscallTool;

struct TerminalOutput(i32);

impl std::io::Write for TerminalOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let result = unsafe {
            raw_syscall6(
                libc::SYS_write,
                [
                    self.0 as u64,
                    bytes.as_ptr() as u64,
                    bytes.len() as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        if result < 0 {
            Err(std::io::Error::from_raw_os_error((-result) as i32))
        } else {
            Ok(result as usize)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn diagnostic_failure(message: &'static [u8]) -> ! {
    unsafe {
        raw_syscall6(
            libc::SYS_write,
            [2, message.as_ptr() as u64, message.len() as u64, 0, 0, 0],
        );
        raw_syscall6(libc::SYS_exit_group, [126, 0, 0, 0, 0, 0]);
        core::arch::asm!("ud2", options(noreturn));
    }
}

fn capture_identity() -> (u64, i64, u64) {
    reverie_liteinst::__owned_syscall_capture().unwrap_or_else(|| {
        diagnostic_failure(b"owned-syscall: missing authentic owned capture before Tool syscall\n")
    })
}

#[reverie::tool]
impl Tool for SyscallTool {
    type GlobalState = super::super::CounterGlobal;
    type ThreadState = usize;

    fn subscriptions(_: &()) -> Subscription {
        [Sysno::getpid, Sysno::read].into_iter().collect()
    }

    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        if capture_identity() != (1, libc::SYS_getpid, sites()[0]) {
            diagnostic_failure(
                b"owned-syscall: first ThreadStart has wrong captured syscall identity\n",
            );
        }
        assert_eq!(guest.read_clock()?, 0);
        if ARMED.load(Ordering::Relaxed) {
            guest.set_timer_precise(TimerSchedule::RcbsAndInstructions(2, 3))?;
        }
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        let armed = ARMED.load(Ordering::Relaxed);
        let number = syscall.number();
        let captured = capture_identity();
        let index = sample(guest, false).await;
        let ordinal = INJECTIONS.fetch_add(1, Ordering::Relaxed);
        assert!(ordinal < 3);
        assert_eq!(
            number,
            if ordinal == 1 {
                Sysno::read
            } else {
                Sysno::getpid
            }
        );
        let registers = guest.regs().await;
        let site = sites()[[0, 3, 5][ordinal as usize]];
        assert_eq!(
            captured,
            (
                if armed {
                    [1, 10, 12][ordinal as usize]
                } else {
                    ordinal + 1
                },
                number as i64,
                site
            )
        );
        assert_eq!(registers.rip, site);
        assert_eq!(registers.rcx, site + 2);
        assert_eq!(registers.r11 & 0x100, 0);
        assert_eq!(registers.r11, registers.eflags);
        assert_eq!(registers.rdi, FD.load(Ordering::Relaxed));
        assert_eq!(registers.rsi, BUFFER.load(Ordering::Relaxed));
        assert_eq!(registers.rdx, 39);
        assert_eq!(registers.rax, number as u64);
        let before = output_state();
        let result = guest.inject(syscall).await?;
        assert_eq!(
            output_state(),
            before,
            "guest injection must restore protected runtime mask"
        );
        assert_eq!(
            guest.regs().await,
            registers,
            "injection is not a register editor"
        );
        if ordinal == 1 {
            assert_eq!(result, 39);
            if armed {
                guest.set_timer_precise(TimerSchedule::RcbsAndInstructions(0, 3))?;
            }
        } else if ordinal == 2 && armed {
            guest.set_timer_precise(TimerSchedule::RcbsAndInstructions(0, 1))?;
        }
        println!(
            "syscall-result: event={index} ordinal={ordinal} nr={} result={result} pc={site:#x} rcx={:#x} r11={:#x} capture_generation={}",
            number as u64, registers.rcx, registers.r11, captured.0
        );
        assert_eq!(
            output_state(),
            before,
            "nested Tool output retains protected mask/TF"
        );
        Ok(result)
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        let index = sample(guest, true).await;
        let position = reverie_liteinst::__owned_timer_position().unwrap();
        let (generation, sequence, target, suffix, pc) = match index {
            1 => (1, 8, 2, 3, sites()[2]),
            4 => (3, 1, 3, 1, sites()[7]),
            _ => panic!("unexpected TimerEvent {index}"),
        };
        assert_eq!(
            (
                position.generation,
                position.sequence,
                position.target,
                position.suffix,
                position.rip
            ),
            (generation, sequence, target, suffix, pc)
        );
        assert_eq!(position.clock, 3);
        let before = output_state();
        println!(
            "syscall-timer-position: event={index} generation={generation} sequence={sequence} offset={} clock=3 target={target} suffix={suffix}",
            pc - sites()[0]
        );
        assert_eq!(output_state(), before);
    }
}

fn output_state() -> (u64, [u64; 7]) {
    let flags: u64;
    unsafe {
        core::arch::asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags));
    }
    assert_eq!(flags & 0x100, 0);
    let mask = mask();
    assert_eq!(
        mask,
        reverie_preload::signal::runtime_ordinary_mask()
            & !((1 << (libc::SIGKILL - 1)) | (1 << (libc::SIGSTOP - 1)))
    );
    assert_ne!(mask & (1 << (libc::SIGTRAP - 1)), 0);
    (mask, native_controls())
}

async fn sample<G: Guest<SyscallTool>>(guest: &mut G, timer: bool) -> usize {
    let _cleanup = Cleanup;
    let index = *guest.thread_state();
    assert!(index < if ARMED.load(Ordering::Relaxed) { 5 } else { 3 });
    assert_eq!(CALLBACKS.fetch_add(1, Ordering::Relaxed), index as u64);
    *guest.thread_state_mut() += 1;
    assert_eq!(
        timer,
        ARMED.load(Ordering::Relaxed) && [1, 4].contains(&index)
    );
    let registers = guest.regs().await;
    assert_eq!(registers.eflags & 0x100, 0);
    let local = 0u64;
    assert!(registers.rsp.abs_diff((&raw const local) as u64) > 64 * 1024);
    let clock = guest.read_clock().unwrap();
    assert_eq!(clock, if index == 0 { 0 } else { 3 });
    CLOCKS[index].store(clock, Ordering::Relaxed);
    assert_eq!(guest.send_rpc(1).await, (index as u64 + 1, 1));
    let controls = native_controls();
    assert_eq!((controls[3], controls[4]), (1, libc::PR_TSC_ENABLE as u64));
    let _native = core::arch::x86_64::__cpuid(0);
    let _tsc = reverie::RdtscResult::new(reverie::Rdtsc::Tsc);
    let _tscp = reverie::RdtscResult::new(reverie::Rdtsc::Tscp);
    work();
    let mut clobbered = Floating([0xff; 2560]);
    unsafe {
        *libc::__errno_location() = libc::EDOM;
        core::arch::asm!("fninit", "vzeroall",
            ".irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31",
            r"vpxord zmm\register, zmm\register, zmm\register",
            r"vmovdqu64 [{image} + (\register - 16) * 64], zmm\register", ".endr",
            image = in(reg) clobbered.0.as_mut_ptr(), clobber_abi("C"), options(nostack));
    }
    assert!(clobbered.0[..1024].iter().all(|byte| *byte == 0));
    assert_eq!(native_controls(), controls);
    assert_eq!(guest.read_clock().unwrap(), clock);
    index
}

pub(super) fn run(path: &Path, armed: bool, work: u64) -> ! {
    ARMED.store(armed, Ordering::Relaxed);
    WORK.store(work, Ordering::Relaxed);
    let mut descriptors = [-1; 2];
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    for byte in [0x35u8, 0xa6] {
        let block = [byte; 39];
        assert_eq!(
            unsafe { libc::write(descriptors[1], block.as_ptr().cast(), block.len()) },
            39
        );
    }
    let text = unsafe {
        std::slice::from_raw_parts(
            sites()[0] as *const u8,
            owned_syscall_end as *const () as usize - sites()[0] as usize,
        )
    }
    .to_vec();
    let evidence: PathBuf = std::env::var_os("REVERIE_OWNED_SYSCALL_EVIDENCE")
        .expect("private evidence path")
        .into();
    let evidence_file = std::fs::File::create_new(&evidence).unwrap();
    let mut probe = Probe {
        path: path.to_owned(),
        evidence,
        evidence_file,
        mask: mask(),
        controls: native_controls(),
        inventory: None,
        text,
        descriptors,
        pid: unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) },
        buffer: [0xcc; 55],
        registers: [0; 18],
        before: Floating([0; 2560]),
        after: Floating([0; 2560]),
    };
    unsafe { owned_syscall_entry((&raw mut probe).cast()) }
}

pub(super) unsafe extern "C" fn initialize(pointer: *mut libc::c_void) -> i32 {
    let probe = unsafe { &mut *pointer.cast::<Probe>() };
    FD.store(probe.descriptors[0] as u64, Ordering::Relaxed);
    BUFFER.store(
        unsafe { probe.buffer.as_mut_ptr().add(8) } as u64,
        Ordering::Relaxed,
    );
    match unsafe {
        reverie_liteinst::__install_owned_syscall_timer_fixture::<SyscallTool>(&probe.path)
    } {
        Ok(inventory) => probe.inventory = Some(inventory),
        Err(error) => {
            eprintln!("owned-syscall: installer failed; probe not entered: {error}");
            return 42;
        }
    }
    assert_eq!(native_controls(), probe.controls);
    assert_eq!(mask(), probe.mask);
    unsafe { *libc::__errno_location() = libc::E2BIG };
    1
}

pub(super) unsafe extern "C" fn verify(pointer: *const libc::c_void) -> ! {
    use std::io::Write;
    use std::os::fd::AsRawFd;

    let probe = unsafe { &*pointer.cast::<Probe>() };
    let saved_errno = unsafe { *libc::__errno_location() };
    let evidence = unsafe { reverie_liteinst::__owned_syscall_evidence() }.unwrap_or_else(|_| {
        diagnostic_failure(b"owned-syscall: terminal clock/runtime ownership unavailable\n")
    });
    let bytes = evidence.bytes.as_ref().unwrap_or_else(|_| {
        diagnostic_failure(
            b"owned-syscall: terminal owned-return validation failed; no success evidence\n",
        )
    });
    if TerminalOutput(probe.evidence_file.as_raw_fd())
        .write_all(bytes)
        .is_err()
    {
        diagnostic_failure(b"owned-syscall: terminal evidence output failed\n");
    }
    assert_eq!(saved_errno, libc::E2BIG);
    let armed = ARMED.load(Ordering::Relaxed);
    let count = if armed { 5 } else { 3 };
    assert_eq!(CALLBACKS.load(Ordering::Relaxed), count);
    assert_eq!(INJECTIONS.load(Ordering::Relaxed), 3);
    if armed {
        assert_eq!(
            unsafe { reverie_liteinst::__owned_step_observation() }.unwrap(),
            (10, sites()[7], 3)
        );
    }
    let mut clocks = CLOCKS[..count as usize]
        .iter()
        .map(|sample| sample.load(Ordering::Relaxed))
        .collect::<Vec<_>>();
    clocks.push(unsafe { reverie_liteinst::__read_owned_clock_after_return() }.unwrap());
    assert_eq!(
        clocks,
        if armed {
            vec![0, 3, 3, 3, 3, 6]
        } else {
            vec![0, 3, 3, 6]
        }
    );
    assert_eq!(RESULTS[0].load(Ordering::Relaxed), probe.pid as u64);
    assert_eq!(RESULTS[1].load(Ordering::Relaxed), probe.pid as u64);
    assert_eq!(
        RESULTS[2].load(Ordering::Relaxed),
        FD.load(Ordering::Relaxed)
    );
    assert_eq!(
        RESULTS[3].load(Ordering::Relaxed),
        BUFFER.load(Ordering::Relaxed)
    );
    assert_eq!(RESULTS[4].load(Ordering::Relaxed), 39);
    assert_eq!(&probe.buffer[..8], &[0xcc; 8]);
    assert_eq!(&probe.buffer[8..47], &[0x35; 39]);
    assert_eq!(&probe.buffer[47..], &[0xcc; 8]);
    let mut second: [u8; 39] = [0xcc; 39];
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_read,
                [
                    FD.load(Ordering::Relaxed),
                    second.as_mut_ptr() as u64,
                    39,
                    0,
                    0,
                    0,
                ],
            )
        },
        39
    );
    assert_eq!(
        second, [0xa6; 39],
        "second pipe block proves first read was not executed twice"
    );
    assert_eq!(
        &probe.registers[..4],
        &[probe.pid as u64, 0x2222222222222222, sites()[6], 39]
    );
    assert_eq!(probe.registers[4], probe.before.0.as_ptr() as u64);
    assert_eq!(probe.registers[5], probe.registers.as_ptr() as u64);
    assert_eq!(
        &probe.registers[6..15],
        &[
            0x7272727272727272,
            0x5555555555555555,
            0x6666666666666666,
            0x4444444444444444,
            0xed7,
            0x7171717171717171,
            0x7373737373737373,
            0x7474747474747474,
            0x7575757575757575
        ]
    );
    assert_eq!(probe.registers[15], probe.registers[16]);
    assert_eq!(probe.registers[17], 0xed7);
    assert!(probe.before.0[1408..2432].iter().all(|byte| *byte == 0xff));
    assert_ne!(probe.before.0[512] & 0x80, 0);
    assert_eq!(
        &probe.before.0[..2440],
        &probe.after.0[..2440],
        "owned-syscall full seeded XSAVE"
    );
    assert_eq!(mask(), probe.mask);
    assert_eq!(native_controls(), probe.controls);
    assert_eq!(
        unsafe { std::slice::from_raw_parts(sites()[0] as *const u8, probe.text.len()) },
        probe.text
    );
    let stats = reverie_liteinst::syscall_mode_stats();
    assert_eq!(
        (
            stats.planning_attempts,
            stats.patch_attempts,
            stats.installed_patches,
            stats.vdso_rewrite_attempts
        ),
        (0, 0, 0, 0)
    );
    if writeln!(TerminalOutput(libc::STDOUT_FILENO),
        "owned-syscall: armed={armed} callbacks={count} rpc={count} injections=3 timers={} completed={} clocks={clocks:?} work={} pipe=first-and-second-preserved state=preserved patches=0 posix_inventory={:?} fp_profile=seeded-hi16-zmm evidence={:?}",
        if armed { 2 } else { 0 },
        if armed { 10 } else { 0 },
        WORK.load(Ordering::Relaxed),
        probe.inventory.unwrap(),
        probe.evidence
    ).is_err() {
        diagnostic_failure(b"owned-syscall: terminal stdout failed\n");
    }
    unsafe {
        raw_syscall6(libc::SYS_exit_group, [0; 6]);
    }
    std::process::abort()
}

#[cfg(test)]
mod terminal_output_tests {
    use std::io::Read;
    use std::io::Seek;
    use std::io::Write;
    use std::os::fd::AsRawFd;

    use super::TerminalOutput;

    #[test]
    fn trusted_write_preserves_evidence_and_descriptor_ownership() {
        let mut file = tempfile::tempfile().unwrap();
        TerminalOutput(file.as_raw_fd())
            .write_all(b"retained frame evidence")
            .unwrap();
        file.rewind().unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"retained frame evidence");
    }

    #[test]
    fn invalid_and_full_descriptors_refuse_output() {
        assert_eq!(
            TerminalOutput(-1)
                .write_all(b"must fail")
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EBADF)
        );
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        assert_eq!(
            TerminalOutput(full.as_raw_fd())
                .write_all(b"must fail")
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOSPC)
        );
    }
}
