//! Compiler-generated integer workload for the private owned-step qualification.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::CpuIdResult;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Rdtsc;
use reverie::RdtscResult;
use reverie::Subscription;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_preload::trap::raw_syscall6;

const WORDS: usize = 32;
const SEED: u64 = 0x1122_3344_5566_7788;
const EVIDENCE_BYTES: usize = 8 * 1024 * 1024;
static WORK: AtomicU64 = AtomicU64::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);
static EVENTS: AtomicU64 = AtomicU64::new(0);
static TIMERS: AtomicU64 = AtomicU64::new(0);
static INJECTIONS: AtomicU64 = AtomicU64::new(0);
static CLOCKS: [AtomicU64; 128] = [const { AtomicU64::new(u64::MAX) }; 128];
static PIPE_FD: AtomicU64 = AtomicU64::new(0);
static INIT_PHASE: AtomicU64 = AtomicU64::new(0);
static mut DATA: [u64; WORDS] = [0; WORDS];
static mut BUFFER: [u8; 55] = [0xcc; 55];
static mut RESULTS: [u64; 25] = [0; 25];

#[repr(C, align(64))]
struct Floating([u8; 2560]);
static mut BEFORE: Floating = Floating([0; 2560]);
static mut AFTER: Floating = Floating([0; 2560]);

struct Probe {
    socket: PathBuf,
    evidence: PathBuf,
    evidence_files: [std::fs::File; 3],
    budget: usize,
    expected: [u64; WORDS],
    result: u64,
    pid: i64,
    mask: u64,
    controls: [u64; 7],
}

unsafe extern "C" {
    fn compiled_entry(probe: *mut libc::c_void) -> !;
    fn compiled_first();
    fn reverie_liteinst_domain_depth() -> u64;
    fn reverie_liteinst_owned_capture_diagnostic(output: *mut u64);
}

core::arch::global_asm!(include_str!("owned_compiled_step/boundary.s"),
    clock_begin = sym reverie_liteinst::__clock_constructor_begin,
    clock_finish = sym reverie_liteinst::__clock_constructor_finish,
    initialize = sym initialize, verify = sym verify,
    workload = sym compiled_integer_work,
    data = sym DATA, before = sym BEFORE, after = sym AFTER,
    results = sym RESULTS, buffer = sym BUFFER, pipe_fd = sym PIPE_FD,
    seed = const SEED, words = const WORDS,
    init_phase = sym INIT_PHASE,
);

struct EarlyObservation {
    bytes: [u8; 1024],
    used: usize,
}

struct InitializerOutput(i32);

fn install_guest_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;
        let _ = writeln!(InitializerOutput(libc::STDERR_FILENO), "{info}");
        unsafe {
            raw_syscall6(libc::SYS_exit_group, [126, 0, 0, 0, 0, 0]);
            core::arch::asm!("ud2", options(noreturn));
        }
    }));
}

fn initializer_write_result(result: i64) -> std::io::Result<usize> {
    if result < 0 {
        Err(std::io::Error::from_raw_os_error((-result) as i32))
    } else {
        Ok(result as usize)
    }
}

impl std::io::Write for InitializerOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        initializer_write_result(unsafe {
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
        })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn print_installation(
    output: &mut impl std::io::Write,
    inventory: reverie_liteinst::PosixTimerInventory,
    budget: usize,
) -> std::io::Result<()> {
    writeln!(output, "compiled-install: {inventory:?} budget={budget}")
}

fn print_installation_failure(
    output: &mut impl std::io::Write,
    error: &std::io::Error,
) -> std::io::Result<()> {
    writeln!(output, "compiled-install-failed: {error}")
}

#[cfg(test)]
mod initializer_output_tests {
    use std::io;
    use std::io::Write;

    use reverie_liteinst::PosixTimerInventory;

    use super::*;

    struct PartialOutput {
        bytes: Vec<u8>,
        interrupt: bool,
        stop: Option<io::Result<usize>>,
    }

    impl Write for PartialOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if std::mem::take(&mut self.interrupt) {
                return Err(io::Error::from_raw_os_error(libc::EINTR));
            }
            if !self.bytes.is_empty()
                && let Some(result) = self.stop.take()
            {
                return result;
            }
            let length = bytes.len().min(3);
            self.bytes.extend_from_slice(&bytes[..length]);
            Ok(length)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn installation_line_preserves_inventory_budget_and_newline() {
        for (inventory, name) in [
            (PosixTimerInventory::Empty, "Empty"),
            (PosixTimerInventory::Unavailable, "Unavailable"),
        ] {
            for budget in [64, EVIDENCE_BYTES] {
                let mut output = Vec::new();
                print_installation(&mut output, inventory, budget).unwrap();
                assert_eq!(
                    output,
                    format!("compiled-install: {name} budget={budget}\n").as_bytes()
                );
            }
        }
    }

    #[test]
    fn partial_and_interrupted_writes_keep_the_complete_line_once() {
        let mut output = PartialOutput {
            bytes: Vec::new(),
            interrupt: true,
            stop: None,
        };
        print_installation(&mut output, PosixTimerInventory::Empty, EVIDENCE_BYTES).unwrap();
        assert_eq!(output.bytes, b"compiled-install: Empty budget=8388608\n");
    }

    #[test]
    fn output_failure_or_zero_write_is_not_success() {
        for (stop, kind) in [
            (Ok(0), io::ErrorKind::WriteZero),
            (
                Err(io::Error::from_raw_os_error(libc::EPIPE)),
                io::ErrorKind::BrokenPipe,
            ),
        ] {
            let mut output = PartialOutput {
                bytes: Vec::new(),
                interrupt: false,
                stop: Some(stop),
            };
            let error =
                print_installation(&mut output, PosixTimerInventory::Empty, 64).unwrap_err();
            assert_eq!(error.kind(), kind);
            assert_eq!(output.bytes, b"com");
        }
    }

    #[test]
    fn installation_failure_keeps_the_actual_diagnostic() {
        let mut output = Vec::new();
        print_installation_failure(&mut output, &io::Error::other("original failure")).unwrap();
        assert_eq!(output, b"compiled-install-failed: original failure\n");
    }

    #[test]
    fn raw_write_results_preserve_count_and_errno() {
        for count in [0, 1, 1024] {
            assert_eq!(initializer_write_result(count).unwrap(), count as usize);
        }
        for errno in [libc::EINTR, libc::EPIPE, libc::EBADF] {
            assert_eq!(
                initializer_write_result(-i64::from(errno))
                    .unwrap_err()
                    .raw_os_error(),
                Some(errno)
            );
        }
    }

    #[test]
    fn trusted_output_writes_the_real_line_without_owning_the_descriptor() {
        use std::io::Read;
        use std::io::Seek;
        use std::os::fd::AsRawFd;

        let mut file = tempfile::tempfile().unwrap();
        {
            let mut output = InitializerOutput(file.as_raw_fd());
            print_installation(&mut output, PosixTimerInventory::Unavailable, 64).unwrap();
            output.flush().unwrap();
        }
        file.rewind().unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"compiled-install: Unavailable budget=64\n");
        assert_eq!(
            InitializerOutput(-1)
                .write(b"must fail")
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EBADF)
        );
    }

    #[test]
    fn guest_panic_hook_refuses_output_and_assertion_failure() {
        use std::os::fd::AsRawFd;

        const CHILD: &str = "REVERIE_COMPILED_TERMINAL_FAILURE";
        if let Ok(mode) = std::env::var(CHILD) {
            install_guest_panic_hook();
            if mode == "output" {
                let full = std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/full")
                    .unwrap();
                InitializerOutput(full.as_raw_fd())
                    .write_all(b"must fail")
                    .unwrap();
            } else {
                assert_eq!(1, 2, "terminal assertion failure");
            }
            panic!("terminal failure returned");
        }
        for mode in ["output", "assertion"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "initializer_output_tests::guest_panic_hook_refuses_output_and_assertion_failure", "--nocapture"])
                .env(CHILD, mode)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(126), "{output:?}");
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(
                stderr.contains(if mode == "output" {
                    "No space left on device"
                } else {
                    "terminal assertion failure"
                }),
                "{stderr}"
            );
            assert!(!stderr.contains("terminal failure returned"), "{stderr}");
        }
    }
}

#[cfg(test)]
mod early_observation_tests {
    use core::fmt::Write;

    use super::*;

    #[test]
    fn maximum_fields_fit_without_truncation() {
        let output = early_observation([u64::MAX; 15]);
        let text = core::str::from_utf8(&output.bytes[..output.used]).unwrap();
        assert_eq!(text.matches("=ffffffffffffffff ").count(), 15);
        assert!(text.starts_with("init_phase=ffffffffffffffff "));
        assert!(text.ends_with("expected_site=ffffffffffffffff \n"));
        assert!(output.used < output.bytes.len());
    }

    #[test]
    fn overflow_refuses_without_overwriting_the_retained_prefix() {
        let mut output = early_observation([0; 15]);
        let prefix = output.bytes;
        let used = output.used;
        assert!(output.write_str(&"x".repeat(1025)).is_err());
        assert_eq!(output.used, used);
        assert_eq!(output.bytes, prefix);
    }

    #[test]
    fn physical_phase_is_published_after_finalizer_before_original_syscall() {
        let boundary = include_str!("owned_compiled_step/boundary.s");
        let finish = boundary.find("call {clock_finish}").unwrap();
        let phase = boundary
            .find("mov qword ptr [rip + {init_phase}], 7")
            .unwrap();
        let first = boundary.find("compiled_first:\n    syscall").unwrap();
        assert!(finish < phase && phase < first);
        assert_eq!(boundary.matches("{init_phase}").count(), 1);
        assert!(boundary.contains("mov eax, 39\n.global compiled_first"));
    }
}

impl core::fmt::Write for EarlyObservation {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let end = self.used.checked_add(text.len()).ok_or(core::fmt::Error)?;
        let destination = self.bytes.get_mut(self.used..end).ok_or(core::fmt::Error)?;
        destination.copy_from_slice(text.as_bytes());
        self.used = end;
        Ok(())
    }
}

fn early_observation(values: [u64; 15]) -> EarlyObservation {
    use core::fmt::Write;
    let mut output = EarlyObservation {
        bytes: [0; 1024],
        used: 0,
    };
    for (name, value) in [
        "init_phase",
        "domain_depth",
        "owned_phase",
        "callback",
        "capture_present",
        "generation",
        "capture_number",
        "capture_site",
        "rip",
        "rax",
        "rcx",
        "r11",
        "rsp",
        "flags",
        "expected_site",
    ]
    .into_iter()
    .zip(values)
    {
        write!(&mut output, "{name}={value:016x} ").expect("fixed diagnostic bound");
    }
    output.write_str("\n").expect("fixed diagnostic bound");
    output
}

fn write_early_observation(output: &EarlyObservation) {
    let mut offset = 0;
    for _attempt in 0..4 {
        if offset == output.used {
            break;
        }
        let written = unsafe {
            raw_syscall6(
                libc::SYS_write,
                [
                    2,
                    output.bytes.as_ptr() as u64 + offset as u64,
                    (output.used - offset) as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        if written == -i64::from(libc::EINTR) {
            continue;
        }
        if written <= 0 {
            break;
        }
        offset += written as usize;
    }
}

#[derive(Default)]
struct Coordinator(AtomicU64);

#[reverie::global_tool]
impl GlobalTool for Coordinator {
    type Request = u64;
    type Response = u64;
    type Config = ();
    async fn receive_rpc(&self, _: reverie::Tid, request: u64) -> u64 {
        assert_eq!(self.0.fetch_add(1, Ordering::Relaxed) + 1, request);
        request
    }
}

#[derive(Default)]
struct CompiledTool;

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

fn mask() -> u64 {
    let mut mask = 0;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [0, 0, (&raw mut mask) as u64, 8, 0, 0],
            )
        },
        0
    );
    mask
}

fn controls() -> [u64; 7] {
    let mut values = [0; 7];
    for (operation, output) in [0x1003, 0x1004, 0x1022].into_iter().zip(&mut values[..3]) {
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_arch_prctl,
                    [operation, output as *mut u64 as u64, 0, 0, 0, 0],
                )
            },
            0
        );
    }
    values[3] = unsafe { raw_syscall6(libc::SYS_arch_prctl, [0x1011, 0, 0, 0, 0, 0]) } as u64;
    let mut tsc = 0i32;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [libc::PR_GET_TSC as u64, (&raw mut tsc) as u64, 0, 0, 0, 0],
            )
        },
        0
    );
    values[4] = tsc as u64;
    values[5] = unsafe { core::arch::x86_64::_xgetbv(0) };
    let pkru: u32;
    unsafe {
        core::arch::asm!("rdpkru", in("ecx") 0u32, out("eax") pkru, out("edx") _, options(nostack));
    }
    values[6] = u64::from(pkru);
    values
}

fn runtime_state() -> (u64, [u64; 7]) {
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
    (mask, controls())
}

async fn sample<G: Guest<CompiledTool>>(guest: &mut G, kind: &str) -> u64 {
    let _cleanup = Cleanup;
    let ordinal = EVENTS.fetch_add(1, Ordering::Relaxed);
    assert!(ordinal < CLOCKS.len() as u64);
    assert_eq!(*guest.thread_state(), ordinal);
    *guest.thread_state_mut() += 1;
    let clock = guest.read_clock().unwrap();
    CLOCKS[ordinal as usize].store(clock, Ordering::Relaxed);
    let registers = guest.regs().await;
    assert_eq!(registers.eflags & 0x100, 0);
    let local = 0u64;
    assert!(registers.rsp.abs_diff((&raw const local) as u64) > 64 * 1024);
    let before = runtime_state();
    assert_eq!((before.1[3], before.1[4]), (1, libc::PR_TSC_ENABLE as u64));
    let total = guest.send_rpc(ordinal + 1).await;
    assert_eq!(total, ordinal + 1);
    let _native = core::arch::x86_64::__cpuid(0);
    let _tsc = RdtscResult::new(Rdtsc::Tsc);
    let _tscp = RdtscResult::new(Rdtsc::Tscp);
    work();
    println!(
        "compiled-event: ordinal={} kind={kind} clock={clock} rpc={total}",
        ordinal + 1
    );
    assert_eq!(runtime_state(), before);
    assert_eq!(guest.read_clock().unwrap(), clock);
    assert_eq!(guest.regs().await, registers);
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
    total
}

fn schedule<G: Guest<CompiledTool>>(guest: &mut G) -> Result<(), reverie::Error> {
    if ARMED.load(Ordering::Relaxed) {
        guest.set_timer_precise(TimerSchedule::RcbsAndInstructions(2, 3))?;
    }
    Ok(())
}

#[reverie::tool]
impl Tool for CompiledTool {
    type GlobalState = Coordinator;
    type ThreadState = u64;

    fn subscriptions(_: &()) -> Subscription {
        let mut subscriptions: Subscription = [Sysno::getpid, Sysno::read].into_iter().collect();
        subscriptions.cpuid();
        subscriptions.rdtsc();
        subscriptions
    }

    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        if reverie_liteinst::__owned_syscall_capture()
            != Some((1, libc::SYS_getpid, compiled_first as *const () as u64))
        {
            let registers = guest.regs().await;
            let mut capture = [0; 6];
            unsafe { reverie_liteinst_owned_capture_diagnostic(capture.as_mut_ptr()) };
            let output = early_observation([
                INIT_PHASE.load(Ordering::Relaxed),
                unsafe { reverie_liteinst_domain_depth() },
                capture[0],
                capture[1],
                capture[2],
                capture[3],
                capture[4],
                capture[5],
                registers.rip,
                registers.rax,
                registers.rcx,
                registers.r11,
                registers.rsp,
                registers.eflags,
                compiled_first as *const () as u64,
            ]);
            write_early_observation(&output);
        }
        assert_eq!(
            reverie_liteinst::__owned_syscall_capture(),
            Some((1, libc::SYS_getpid, compiled_first as *const () as u64))
        );
        assert_eq!(guest.read_clock()?, 0);
        schedule(guest)
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        let ordinal = INJECTIONS.fetch_add(1, Ordering::Relaxed);
        assert!(ordinal < 3);
        let expected = if ordinal == 1 {
            Sysno::read
        } else {
            Sysno::getpid
        };
        assert_eq!(syscall.number(), expected);
        let capture = reverie_liteinst::__owned_syscall_capture().unwrap();
        assert_eq!(capture.1, expected as i64);
        let registers = guest.regs().await;
        assert_eq!(registers.rip, capture.2);
        assert_eq!(registers.rcx, capture.2 + 2);
        assert_eq!(registers.r11, registers.eflags);
        assert_eq!(registers.eflags & 0x100, 0);
        sample(guest, if ordinal == 1 { "read" } else { "getpid" }).await;
        let before = runtime_state();
        let result = guest.inject(syscall).await?;
        assert_eq!(runtime_state(), before);
        assert_eq!(guest.regs().await, registers);
        if ordinal == 1 {
            assert_eq!(result, 39);
            if ARMED.load(Ordering::Relaxed) {
                guest.set_timer_precise(TimerSchedule::RcbsAndInstructions(0, 3))?;
            }
        }
        Ok(result)
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        let timer = TIMERS.fetch_add(1, Ordering::Relaxed);
        sample(guest, "timer").await;
        let position = reverie_liteinst::__owned_timer_position().unwrap();
        assert!(position.sequence > 0);
        assert_eq!(position.clock, guest.read_clock().unwrap());
        assert_eq!(
            (
                position.generation,
                position.sequence,
                position.clock,
                position.target,
                position.suffix
            ),
            (
                timer + 1,
                if timer == 0 { 41 } else { 31 },
                2 * (timer + 1),
                2 * (timer + 1),
                3
            )
        );
        assert_eq!(
            position.rip,
            compiled_integer_work as *const () as u64 + 0x3b
        );
        let before = runtime_state();
        println!(
            "compiled-position: generation={} sequence={} pc={:#x} clock={} target={} suffix={}",
            position.generation,
            position.sequence,
            position.rip,
            position.clock,
            position.target,
            position.suffix
        );
        assert_eq!(runtime_state(), before);
        schedule(guest).unwrap();
    }

    async fn handle_cpuid_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        eax: u32,
        ecx: u32,
    ) -> Result<CpuIdResult, reverie::Errno> {
        assert_eq!((eax, ecx), (0xfeed, 0xbaad));
        sample(guest, "cpuid").await;
        schedule(guest).unwrap();
        Ok(CpuIdResult {
            eax: eax ^ ecx,
            ebx: 0x2222_2222,
            ecx: 0x3333_3333,
            edx: 0x4444_4444,
        })
    }

    async fn handle_rdtsc_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        request: Rdtsc,
    ) -> Result<RdtscResult, reverie::Errno> {
        sample(
            guest,
            if request == Rdtsc::Tsc {
                "rdtsc"
            } else {
                "rdtscp"
            },
        )
        .await;
        schedule(guest).unwrap();
        Ok(RdtscResult {
            tsc: 0xfedc_ba98_0000_0000 | if request == Rdtsc::Tsc { 3 } else { 4 },
            aux: (request == Rdtsc::Tscp).then_some(0xaaaa_5555),
        })
    }
}

#[inline(never)]
fn mix_left(value: u64, salt: u64) -> u64 {
    value.rotate_left(7).wrapping_add(salt) ^ 0x1234_5678_9abc_def0
}

#[inline(never)]
fn mix_right(value: u64, salt: u64) -> u64 {
    value.rotate_right(11).wrapping_mul(3).wrapping_sub(salt)
}

#[inline(never)]
unsafe extern "C" fn compiled_integer_work(data: *mut u64, count: usize, seed: u64) -> u64 {
    let mut accumulator = seed;
    for offset in 0..count {
        let value = unsafe { data.add(offset).read_volatile() };
        let operation: fn(u64, u64) -> u64 = if value & 1 == 0 { mix_left } else { mix_right };
        accumulator = std::hint::black_box(operation)(value, accumulator);
        unsafe { data.add(offset).write_volatile(accumulator) };
    }
    accumulator
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(args.len() >= 3);
    if args[1] == "server" {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        runtime.block_on(async {
            let server = reverie_rpc_transport::RpcServer::bind(
                std::path::Path::new(&args[2]),
                Arc::new(Coordinator::default()),
                (),
            )
            .unwrap();
            server.serve().await.unwrap();
        });
        return;
    }
    assert_eq!(args[1], "guest");
    assert_eq!(args.len(), 7);
    install_guest_panic_hook();
    assert!(
        std::env::vars_os()
            .all(|(key, _)| !key.as_encoded_bytes().starts_with(b"LD_") && key != "GLIBC_TUNABLES")
    );
    assert_eq!(
        std::fs::symlink_metadata("/etc/ld.so.preload")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    WORK.store(args[4].parse().unwrap(), Ordering::Relaxed);
    ARMED.store(args[5] == "armed", Ordering::Relaxed);
    assert!(matches!(args[5].as_str(), "armed" | "unarmed"));
    for signal in [libc::SIGSEGV, libc::SIGBUS] {
        assert_ne!(
            unsafe { libc::signal(signal, libc::SIG_DFL) },
            libc::SIG_ERR
        );
    }
    let mut descriptors = [-1; 2];
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    for byte in [0x35u8, 0xa6] {
        let block = [byte; 39];
        assert_eq!(
            unsafe { libc::write(descriptors[1], block.as_ptr().cast(), block.len()) },
            39
        );
    }
    PIPE_FD.store(descriptors[0] as u64, Ordering::Relaxed);
    let input = core::array::from_fn(|index| (index as u64).wrapping_mul(0x1234_5678_9abc_def1));
    unsafe {
        DATA = input;
    }
    let mut expected = input;
    let mut result = SEED;
    for value in &mut expected {
        result = if *value & 1 == 0 {
            mix_left(*value, result)
        } else {
            mix_right(*value, result)
        };
        *value = result;
    }
    let evidence = PathBuf::from(&args[3]);
    let evidence_files = [
        evidence.clone(),
        evidence.with_extension("before.xsave"),
        evidence.with_extension("after.xsave"),
    ]
    .map(|path| std::fs::File::create_new(path).unwrap());
    let mut probe = Probe {
        socket: (&args[2]).into(),
        evidence,
        evidence_files,
        budget: if args[6] == "full" {
            EVIDENCE_BYTES
        } else {
            assert_eq!(args[6], "small");
            64
        },
        expected,
        result,
        pid: unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) },
        mask: mask(),
        controls: controls(),
    };
    {
        use std::io::Read;
        let mut mappings = Vec::new();
        std::fs::File::open("/proc/self/maps")
            .unwrap()
            .take(64 * 1024 + 1)
            .read_to_end(&mut mappings)
            .unwrap();
        assert!(
            mappings.len() <= 64 * 1024,
            "startup mapping evidence limit"
        );
        std::fs::write(probe.evidence.with_extension("startup-maps"), mappings).unwrap();
    }
    unsafe { compiled_entry((&raw mut probe).cast()) }
}

unsafe extern "C" fn initialize(probe: *mut Probe) -> i32 {
    INIT_PHASE.store(1, Ordering::Relaxed);
    let probe = unsafe { &mut *probe };
    let high = core::arch::x86_64::__cpuid_count(0xd, 7);
    assert_eq!((high.eax, high.ebx), (1024, 1408));
    INIT_PHASE.store(2, Ordering::Relaxed);
    match unsafe {
        reverie_liteinst::__install_owned_compiled_step_fixture::<CompiledTool>(
            &probe.socket,
            probe.budget,
        )
    } {
        Ok(inventory) => {
            INIT_PHASE.store(3, Ordering::Relaxed);
            print_installation(
                &mut InitializerOutput(libc::STDOUT_FILENO),
                inventory,
                probe.budget,
            )
            .expect("installation output failed");
            INIT_PHASE.store(4, Ordering::Relaxed);
        }
        Err(error) => {
            print_installation_failure(&mut InitializerOutput(libc::STDERR_FILENO), &error)
                .expect("installation error output failed");
            return 42;
        }
    }
    assert_eq!(mask(), probe.mask);
    INIT_PHASE.store(5, Ordering::Relaxed);
    let mut controls = probe.controls;
    controls[3] = 0;
    controls[4] = libc::PR_TSC_SIGSEGV as u64;
    probe.controls = controls;
    assert_eq!(self::controls(), controls);
    unsafe {
        *libc::__errno_location() = libc::E2BIG;
    }
    INIT_PHASE.store(6, Ordering::Relaxed);
    1
}

unsafe extern "C" fn verify(probe: *const Probe) -> ! {
    use std::io::Write;
    use std::os::fd::AsRawFd;

    let probe = unsafe { &*probe };
    let errno = unsafe { *libc::__errno_location() };
    let evidence =
        unsafe { reverie_liteinst::__owned_syscall_evidence() }.unwrap_or_else(|_| unsafe {
            let message = b"compiled-terminal-runtime-unavailable\n";
            raw_syscall6(
                libc::SYS_write,
                [2, message.as_ptr() as u64, message.len() as u64, 0, 0, 0],
            );
            raw_syscall6(libc::SYS_exit_group, [126, 0, 0, 0, 0, 0]);
            core::arch::asm!("ud2", options(noreturn));
        });
    let bytes = evidence.bytes.as_ref().unwrap();
    InitializerOutput(probe.evidence_files[0].as_raw_fd())
        .write_all(bytes)
        .unwrap();
    let before = unsafe { (&raw const BEFORE).read() };
    let after = unsafe { (&raw const AFTER).read() };
    InitializerOutput(probe.evidence_files[1].as_raw_fd())
        .write_all(&before.0[..2440])
        .unwrap();
    InitializerOutput(probe.evidence_files[2].as_raw_fd())
        .write_all(&after.0[..2440])
        .unwrap();
    let completed = verify_history(bytes);
    let count = EVENTS.load(Ordering::Relaxed) as usize;
    let mut trajectory: Vec<_> = CLOCKS[..count]
        .iter()
        .map(|sample| sample.load(Ordering::Relaxed))
        .collect();
    trajectory.push(unsafe { reverie_liteinst::__read_owned_clock_after_return() }.unwrap());
    assert_eq!(errno, libc::E2BIG);
    assert_eq!(mask(), probe.mask);
    assert_eq!(controls(), probe.controls);
    assert_eq!(unsafe { DATA }, probe.expected);
    let results = unsafe { RESULTS };
    assert_eq!(results[0], probe.pid as u64);
    assert_eq!(results[1], probe.result);
    assert_eq!(results[14], 39);
    assert_eq!(results[15], probe.pid as u64);
    assert_eq!(results[16], results[17]);
    assert_eq!(
        &results[18..24],
        &[0x7171, 0x7272, 0x7373, 0x7474, 0x7575, 0x7676]
    );
    assert_eq!(results[24], 0x7979);
    assert_eq!(
        &results[2..14],
        &[
            0xfeed ^ 0xbaad,
            0x2222_2222,
            0x3333_3333,
            0x4444_4444,
            3,
            0x2222_2222,
            0x3333_3333,
            0xfedc_ba98,
            4,
            0x2222_2222,
            0xaaaa_5555,
            0xfedc_ba98,
        ]
    );
    assert_eq!(
        unsafe { BUFFER },
        [vec![0xcc; 8], vec![0x35; 39], vec![0xcc; 8]]
            .concat()
            .as_slice()
    );
    let mut second = [0u8; 39];
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_read,
                [
                    PIPE_FD.load(Ordering::Relaxed),
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
    assert_eq!(second, [0xa6; 39]);
    assert_ne!(before.0[512] & 0x80, 0);
    assert!(before.0[1408..2432].iter().all(|byte| *byte == 0xff));
    assert_eq!(before.0[..2440], after.0[..2440]);
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
    assert_eq!(INJECTIONS.load(Ordering::Relaxed), 3);
    let expected: Vec<u64> = if ARMED.load(Ordering::Relaxed) {
        std::iter::once(0)
            .chain((2..=32).step_by(2))
            .chain([33; 6])
            .collect()
    } else {
        vec![0, 33, 33, 33, 33, 33, 33]
    };
    assert_eq!(trajectory, expected);
    if ARMED.load(Ordering::Relaxed) {
        assert_eq!(completed, 553);
        assert!(bytes.len() > 133120);
        assert!(TIMERS.load(Ordering::Relaxed) > 1);
    }
    writeln!(
        InitializerOutput(libc::STDOUT_FILENO),
        "compiled-instructions: {:?}",
        &results[2..14]
    )
    .unwrap();
    writeln!(InitializerOutput(libc::STDOUT_FILENO),
        "compiled-terminal: events={count} timers={} injections=3 completed={completed} bytes={} clocks={trajectory:?}",
        TIMERS.load(Ordering::Relaxed),
        bytes.len()
    ).unwrap();
    unsafe {
        raw_syscall6(libc::SYS_exit_group, [0, 0, 0, 0, 0, 0]);
        core::arch::asm!("ud2", options(noreturn));
    }
}

fn verify_history(mut bytes: &[u8]) -> u64 {
    let mut frames = 0;
    let mut completed = 0;
    let mut trace = None;
    let mut previous = None;
    while !bytes.is_empty() {
        let header: [u64; 8] = core::array::from_fn(|field| {
            u64::from_le_bytes(bytes[field * 8..field * 8 + 8].try_into().unwrap())
        });
        bytes = &bytes[64..];
        match header[0] {
            0x5355444652414d45 => {
                assert!(trace.is_none());
                assert_eq!(header[7], frames);
                frames += 1;
                assert!(header[6] <= 4096);
                let image = &bytes[..header[6] as usize];
                if header[1] == libc::SIGTRAP as u64 {
                    assert_eq!(header[2], libc::TRAP_TRACE as u64);
                    let rip = u64::from_le_bytes(image[176..184].try_into().unwrap());
                    let flags = u64::from_le_bytes(image[184..192].try_into().unwrap());
                    assert_eq!(flags & 0x10100, 0x100);
                    trace = Some((frames, rip));
                } else {
                    assert!(matches!(header[1] as i32, libc::SIGSEGV | libc::SIGSYS));
                }
                bytes = &bytes[header[6] as usize..];
            }
            0x4e41544956455354 => {
                assert_eq!(trace.take(), Some((header[5], header[4])));
                completed += 1;
                assert_eq!(header[6], completed);
                if let Some((generation, sequence, clock)) = previous {
                    assert!(header[3] == clock || header[3] == clock + 1);
                    if generation == header[1] {
                        assert_eq!(header[2], sequence + 1);
                    } else {
                        assert!(header[1] > generation);
                        assert_eq!(header[2], 1);
                    }
                } else {
                    assert_eq!((header[1], header[2], header[3]), (1, 1, 0));
                }
                previous = Some((header[1], header[2], header[3]));
            }
            magic => panic!("unknown raw evidence header {magic:#x}"),
        }
    }
    assert!(trace.is_none());
    assert_eq!(frames, completed + 6);
    completed
}
