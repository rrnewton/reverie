#![deny(warnings)]
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use core::ffi::c_void;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::ffi::CString;
use std::hint::black_box;
use std::process::Command;
use std::ptr;
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use liteinst2::patcher::JumpPatchPlan;
use liteinst2::patcher::StalenessBudget;
use liteinst2::rapid::RapidProbe;
use liteinst2::rapid::RapidTogglePlan;
use liteinst2::scanner::InstructionScanner;
use liteinst2::scanner::ScanResult;
use liteinst2::trampoline::HookContext;
use liteinst2::trampoline::HookSite;
use liteinst2::trampoline::InstalledHook;
use liteinst2::trampoline::TrampolinePlan;
use liteinst2::trampoline::translate_program_counter;

const PAGE_BYTES: usize = 4096;
const CACHE_LINE_BYTES: usize = 64;
const SITE_OFFSET: usize = 60;
const TARGET_OFFSET: usize = 128;
const EXECUTOR_THREADS: usize = 4;
const TOGGLER_THREADS: usize = 4;

static HOOK_CALLS: AtomicU64 = AtomicU64::new(0);
static EXECUTED_CALLS: AtomicU64 = AtomicU64::new(0);
static TRAP_SIGNALS: AtomicU64 = AtomicU64::new(0);
static USER_SIGNALS: AtomicU64 = AtomicU64::new(0);
static FAULT_ORIGINAL_PC: AtomicU64 = AtomicU64::new(0);
static FAULT_GENERATED_PC: AtomicU64 = AtomicU64::new(0);

const ISOLATED_STRESS_ENV: &str = "LITEINST_ISOLATED_STRESS_TEST";

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(#10): Review the subprocess boundary for global signal state.
fn run_isolated(test_name: &str, body: fn()) {
    if std::env::var(ISOLATED_STRESS_ENV).as_deref() == Ok(test_name) {
        body();
        return;
    }

    let executable = std::env::current_exe().expect("failed to locate stress test executable");
    let status = Command::new(executable)
        .args(["--exact", test_name, "--ignored", "--nocapture"])
        .env(ISOLATED_STRESS_ENV, test_name)
        .status()
        .expect("failed to launch isolated stress test");
    assert!(
        status.success(),
        "isolated stress test {test_name} failed: {status}"
    );
}

unsafe extern "C" fn record_hook(_context: *mut HookContext) {
    HOOK_CALLS.fetch_add(1, Ordering::Relaxed);
}

extern "C" fn count_signal(signal: libc::c_int) {
    match signal {
        libc::SIGTRAP => {
            TRAP_SIGNALS.fetch_add(1, Ordering::Relaxed);
        }
        libc::SIGUSR1 => {
            USER_SIGNALS.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
}

fn install_counting_handler(signal: libc::c_int) {
    // SAFETY: the action is fully initialized before sigaction publishes it.
    unsafe {
        let mut action: libc::sigaction = core::mem::zeroed();
        action.sa_sigaction = count_signal as *const () as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(libc::sigaction(signal, &action, ptr::null_mut()), 0);
    }
}

extern "C" fn translate_fault_pc(
    signal: libc::c_int,
    _info: *mut libc::siginfo_t,
    context: *mut c_void,
) {
    if signal != libc::SIGSEGV || context.is_null() {
        // SAFETY: _exit is async-signal-safe and terminates this isolated child.
        unsafe { libc::_exit(100) };
    }
    // SAFETY: Linux supplies a live ucontext_t to an SA_SIGINFO handler.
    let context = unsafe { &*context.cast::<libc::ucontext_t>() };
    let generated = context.uc_mcontext.gregs[libc::REG_RIP as usize] as u64;
    let translated = translate_program_counter(generated);
    let matches = generated == FAULT_GENERATED_PC.load(Ordering::Relaxed)
        && translated == Some(FAULT_ORIGINAL_PC.load(Ordering::Relaxed));
    // SAFETY: _exit is async-signal-safe and terminates this isolated child.
    unsafe { libc::_exit(if matches { 0 } else { 101 }) };
}

fn install_fault_handler() {
    // SAFETY: the action is fully initialized before sigaction publishes it.
    unsafe {
        let mut action: libc::sigaction = core::mem::zeroed();
        action.sa_sigaction = translate_fault_pc as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(libc::sigaction(libc::SIGSEGV, &action, ptr::null_mut()), 0);
    }
}

struct DualMapping {
    writable: *mut u8,
    executable: *mut u8,
}

impl DualMapping {
    fn new(byte_len: usize) -> Self {
        let len = byte_len.div_ceil(PAGE_BYTES) * PAGE_BYTES;
        let name = b"liteinst2-m5-stress\0";
        // SAFETY: valid NUL-terminated name and flags.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                name.as_ptr().cast::<libc::c_char>(),
                libc::MFD_CLOEXEC,
            ) as libc::c_int
        };
        assert!(fd >= 0, "memfd_create failed");
        // SAFETY: fd is valid and len is representable.
        assert_eq!(unsafe { libc::ftruncate(fd, len as libc::off_t) }, 0);
        // SAFETY: maps a shared writable, non-executable alias.
        let writable = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(writable, libc::MAP_FAILED);
        // SAFETY: maps the same bytes through a read-execute alias.
        let executable = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(executable, libc::MAP_FAILED);
        // SAFETY: both VMAs retain the backing object.
        assert_eq!(unsafe { libc::close(fd) }, 0);
        Self {
            writable: writable.cast(),
            executable: executable.cast(),
        }
    }

    fn writable_address(&self, offset: usize) -> usize {
        // SAFETY: fixtures keep offsets within the mapping.
        unsafe { self.writable.add(offset) as usize }
    }

    fn executable_address(&self, offset: usize) -> u64 {
        // SAFETY: fixtures keep offsets within the mapping.
        unsafe { self.executable.add(offset) as usize as u64 }
    }

    fn write(&self, bytes: &[u8]) {
        // SAFETY: fixture mappings are sized for the complete code image.
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), self.writable, bytes.len());
        }
    }

    fn write_at(&self, offset: usize, bytes: &[u8]) {
        // SAFETY: fixtures keep the write within the mapping.
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), self.writable.add(offset), bytes.len());
        }
    }
}

struct Placeholder {
    address: usize,
}

impl Placeholder {
    fn reserve_near(next_ip: u64, seed: usize) -> Self {
        let page_base = next_ip as usize & !(PAGE_BYTES - 1);
        for offset in 0..2047_usize {
            let step = 1 + (seed + offset) % 2047;
            let distance = step * 1024 * 1024;
            for candidate in [
                page_base.checked_add(distance),
                page_base.checked_sub(distance),
            ]
            .into_iter()
            .flatten()
            {
                if candidate < PAGE_BYTES {
                    continue;
                }
                let target = candidate + TARGET_OFFSET;
                let displacement = target as i128 - i128::from(next_ip);
                if i32::try_from(displacement).is_err() {
                    continue;
                }
                // SAFETY: MAP_FIXED_NOREPLACE never replaces a live VMA.
                let mapping = unsafe {
                    libc::mmap(
                        candidate as *mut c_void,
                        PAGE_BYTES,
                        libc::PROT_NONE,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
                        -1,
                        0,
                    )
                };
                if mapping == libc::MAP_FAILED {
                    continue;
                }
                if mapping as usize == candidate {
                    return Self { address: candidate };
                }
                // SAFETY: mapping is the VMA just returned by mmap.
                assert_eq!(unsafe { libc::munmap(mapping, PAGE_BYTES) }, 0);
            }
        }
        panic!("no rel32 placeholder page available");
    }

    fn target_address(&self) -> u64 {
        (self.address + TARGET_OFFSET) as u64
    }

    fn release(self) {
        // SAFETY: this object exclusively owns the placeholder VMA.
        assert_eq!(
            unsafe { libc::munmap(self.address as *mut c_void, PAGE_BYTES) },
            0
        );
    }
}

struct RapidSpec {
    plan: RapidTogglePlan,
    placeholder: Placeholder,
    writable_address: usize,
    expected: u32,
}

fn function_offset(index: usize) -> usize {
    index * CACHE_LINE_BYTES + SITE_OFFSET
}

fn image_len(functions: usize) -> usize {
    functions * CACHE_LINE_BYTES + 8
}

fn build_rapid_image(
    scanner: &InstructionScanner,
    functions: usize,
) -> (DualMapping, Vec<RapidSpec>, Vec<u32>) {
    assert!(functions > 0);
    let mut code = vec![0_u8; image_len(functions)];
    let mapping = DualMapping::new(code.len());
    let region_base = mapping.executable_address(0);
    let mut placeholders = Vec::with_capacity(functions);
    let mut expected = Vec::with_capacity(functions);

    for index in 0..functions {
        let offset = function_offset(index);
        let execute_address = mapping.executable_address(offset);
        let placeholder = Placeholder::reserve_near(execute_address + 5, index);
        let displacement = i32::try_from(
            i128::from(placeholder.target_address()) - i128::from(execute_address + 5),
        )
        .unwrap();
        code[offset] = 0xB8;
        code[offset + 1..offset + 5].copy_from_slice(&displacement.to_le_bytes());
        code[offset + 5] = 0xC3;
        expected.push(displacement as u32);
        placeholders.push(placeholder);
    }
    mapping.write(&code);
    let scan = scanner.scan(&code, region_base).unwrap();
    assert_eq!(scan.crossing_sites().count(), functions);

    let specs = placeholders
        .into_iter()
        .enumerate()
        .map(|(index, placeholder)| {
            let offset = function_offset(index);
            let execute_address = mapping.executable_address(offset);
            let plan = RapidTogglePlan::from_scan(
                scanner,
                &scan,
                &code,
                region_base,
                execute_address,
                record_hook,
            )
            .unwrap();
            assert_eq!(plan.target_address(), placeholder.target_address());
            RapidSpec {
                plan,
                placeholder,
                writable_address: mapping.writable_address(offset),
                expected: expected[index],
            }
        })
        .collect();
    (mapping, specs, expected)
}

fn build_general_image(
    scanner: &InstructionScanner,
    functions: usize,
) -> (DualMapping, Vec<u8>, ScanResult, Vec<u32>) {
    assert!(functions > 0);
    let mut code = vec![0_u8; image_len(functions)];
    let mapping = DualMapping::new(code.len());
    let mut expected = Vec::with_capacity(functions);
    for index in 0..functions {
        let offset = function_offset(index);
        let value = 0xA500_0000_u32 | u32::try_from(index).unwrap();
        code[offset] = 0xB8;
        code[offset + 1..offset + 5].copy_from_slice(&value.to_le_bytes());
        code[offset + 5] = 0xC3;
        expected.push(value);
    }
    mapping.write(&code);
    let scan = scanner.scan(&code, mapping.executable_address(0)).unwrap();
    assert_eq!(scan.crossing_sites().count(), functions);
    (mapping, code, scan, expected)
}

fn relocated_fault_pc_translation_body() {
    const FAULT_OFFSET: usize = 128;
    let mapping = DualMapping::new(PAGE_BYTES);
    let code = [0x48, 0xA1, 0, 0, 0, 0, 0, 0, 0, 0, 0xC3];
    mapping.write_at(FAULT_OFFSET, &code);

    let scanner = InstructionScanner::default();
    let execute_address = mapping.executable_address(FAULT_OFFSET);
    let scan = scanner.scan(&code, execute_address).unwrap();
    // SAFETY: both aliases and the callback remain valid until the isolated
    // child exits from its SIGSEGV handler.
    let hook = unsafe {
        InstalledHook::install(
            HookSite::new(
                &scanner,
                &scan,
                &code,
                execute_address,
                execute_address,
                mapping.writable.add(FAULT_OFFSET),
            ),
            record_hook,
            StalenessBudget::new(10_000).unwrap(),
        )
        .unwrap()
    };
    FAULT_ORIGINAL_PC.store(execute_address, Ordering::Relaxed);
    FAULT_GENERATED_PC.store(
        hook.trampoline().relocated_tail_address(),
        Ordering::Relaxed,
    );
    install_fault_handler();
    hook.activate().unwrap();

    // SAFETY: the fixture is a valid no-argument function whose relocated load
    // intentionally faults before returning.
    let function: extern "C" fn() =
        unsafe { core::mem::transmute(mapping.executable_address(FAULT_OFFSET) as usize) };
    function();
    panic!("faulting relocated instruction unexpectedly returned");
}

#[test]
#[ignore = "isolated intentional SIGSEGV regression test"]
fn relocated_fault_pc_translation() {
    run_isolated(
        "relocated_fault_pc_translation",
        relocated_fault_pc_translation_body,
    );
}

fn next_random(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

unsafe fn call_function(base: usize, index: usize) -> u32 {
    let address = base + function_offset(index);
    // SAFETY: synthetic images contain mov eax, imm32; ret at every function offset.
    let function: unsafe extern "C" fn() -> u32 = unsafe { core::mem::transmute(address) };
    // SAFETY: upheld by this helper's fixture contract.
    unsafe { function() }
}

fn spawn_executors(
    executable_base: usize,
    expected: Arc<Vec<u32>>,
    stop: Arc<AtomicBool>,
    start: Arc<Barrier>,
) -> Vec<thread::JoinHandle<()>> {
    (0..EXECUTOR_THREADS)
        .map(|worker| {
            let expected = Arc::clone(&expected);
            let stop = Arc::clone(&stop);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                let mut random = 0x9E37_79B9_7F4A_7C15_u64 ^ worker as u64;
                start.wait();
                while !stop.load(Ordering::Acquire) {
                    let index = next_random(&mut random) as usize % expected.len();
                    // SAFETY: mappings and functions live for the process.
                    let observed = unsafe { call_function(executable_base, index) };
                    assert_eq!(observed, expected[index]);
                    EXECUTED_CALLS.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect()
}

fn join_threads(threads: Vec<thread::JoinHandle<()>>) {
    for thread in threads {
        thread.join().unwrap();
    }
}

fn spawn_signal_flood(rounds: usize) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for iteration in 0..rounds {
            // SAFETY: both signals have process-lifetime counting dispositions.
            assert_eq!(unsafe { libc::raise(libc::SIGTRAP) }, 0);
            if iteration % 8 == 0 {
                // SAFETY: getpid returns this process and SIGUSR1 is handled.
                assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGUSR1) }, 0);
            }
        }
    })
}

fn env_count(name: &str, default: usize, maximum: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
        .clamp(1, maximum)
}

#[test]
fn random_rips_and_real_libc_windows_never_panic() {
    let scanner = InstructionScanner::default();
    let mut random = 0xD1B5_4A32_D192_ED03_u64;
    for case in 0..50_000_usize {
        let len = 1 + next_random(&mut random) as usize % 64;
        let mut bytes = vec![0_u8; len];
        for byte in &mut bytes {
            *byte = next_random(&mut random) as u8;
        }
        let base = 0x10_0000_u64
            + ((case as u64) << 7)
            + (next_random(&mut random) & (CACHE_LINE_BYTES as u64 - 1));
        let decoded = std::panic::catch_unwind(|| scanner.scan(&bytes, base));
        assert!(decoded.is_ok(), "scanner panicked for random case {case}");
        if let Ok(scan) = decoded.unwrap() {
            assert_eq!(
                scan.instructions()
                    .iter()
                    .map(|item| item.len())
                    .sum::<usize>(),
                bytes.len()
            );
            for site in scan.crossing_sites() {
                assert!(scanner.cache_line_size().crosses(
                    usize::try_from(site.address()).unwrap(),
                    site.instruction_len()
                ));
                let _ = JumpPatchPlan::from_scan(
                    &scanner,
                    &scan,
                    &bytes,
                    base,
                    site.address(),
                    site.address(),
                );
                let _ = RapidTogglePlan::from_scan(
                    &scanner,
                    &scan,
                    &bytes,
                    base,
                    site.address(),
                    record_hook,
                );
            }
        }
    }

    let tiny_base = 0x20_003E_u64;
    let tiny = [0x48, 0x89, 0xF8, 0xC3, 0x90, 0x90, 0x90, 0x90];
    let tiny_scan = scanner.scan(&tiny, tiny_base).unwrap();
    let tiny_plan = TrampolinePlan::from_scan(&tiny_scan, tiny_base, record_hook).unwrap();
    assert_eq!(tiny_plan.displaced_len(), 5);
    assert_eq!(tiny_plan.return_address(), tiny_base + 5);
    assert!(tiny_scan.site(tiny_base + 1).is_none());

    let symbols = [
        "malloc",
        "free",
        "memcpy",
        "memmove",
        "memset",
        "strlen",
        "strcmp",
        "strchr",
        "getpid",
        "clock_gettime",
        "pthread_create",
        "pthread_join",
    ];
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let mut resolved = 0_usize;
    for symbol in symbols {
        let name = CString::new(symbol).unwrap();
        // SAFETY: dlsym accepts this NUL-terminated symbol name.
        let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) } as usize;
        if address == 0 {
            continue;
        }
        let Some(end) = executable_mapping_end(&maps, address) else {
            continue;
        };
        let len = (end - address).min(512);
        // SAFETY: /proc/self/maps proves this complete window is mapped executable.
        let bytes = unsafe { core::slice::from_raw_parts(address as *const u8, len) };
        let result = std::panic::catch_unwind(|| scanner.scan(bytes, address as u64));
        assert!(result.is_ok(), "scanner panicked for libc symbol {symbol}");
        resolved += 1;
    }
    assert!(resolved >= 8, "only resolved {resolved} libc entry windows");
}

fn executable_mapping_end(maps: &str, address: usize) -> Option<usize> {
    maps.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let range = fields.next()?;
        let permissions = fields.next()?;
        if !permissions.contains('x') {
            return None;
        }
        let (start, end) = range.split_once('-')?;
        let start = usize::from_str_radix(start, 16).ok()?;
        let end = usize::from_str_radix(end, 16).ok()?;
        (start <= address && address < end).then_some(end)
    })
}

#[test]
#[ignore = "long-running M5 live stress matrix"]
fn live_probe_stress_matrix() {
    run_isolated("live_probe_stress_matrix", live_probe_stress_matrix_body);
}

fn live_probe_stress_matrix_body() {
    install_counting_handler(libc::SIGTRAP);
    install_counting_handler(libc::SIGUSR1);
    HOOK_CALLS.store(0, Ordering::Relaxed);
    EXECUTED_CALLS.store(0, Ordering::Relaxed);
    TRAP_SIGNALS.store(0, Ordering::Relaxed);
    USER_SIGNALS.store(0, Ordering::Relaxed);

    let rapid_functions = env_count("LITEINST_STRESS_RAPID_FUNCTIONS", 128, 1024);
    let rapid_iterations = env_count("LITEINST_STRESS_RAPID_ITERATIONS", 250_000, 10_000_000);
    let general_functions = env_count("LITEINST_STRESS_GENERAL_FUNCTIONS", 16, 128);
    let general_cycles = env_count("LITEINST_STRESS_GENERAL_CYCLES", 10_000, 1_000_000);
    let signal_rounds = env_count("LITEINST_STRESS_SIGNAL_ROUNDS", 10_000, 1_000_000);
    let scanner = InstructionScanner::default();

    let (rapid_mapping, rapid_specs, rapid_expected) = build_rapid_image(&scanner, rapid_functions);
    let rapid_base = rapid_mapping.executable_address(0) as usize;
    let rapid_expected = Arc::new(rapid_expected);
    let install_stop = Arc::new(AtomicBool::new(false));
    let install_start = Arc::new(Barrier::new(EXECUTOR_THREADS + 1));
    let install_workers = spawn_executors(
        rapid_base,
        Arc::clone(&rapid_expected),
        Arc::clone(&install_stop),
        Arc::clone(&install_start),
    );
    install_start.wait();
    let deadline = Instant::now() + Duration::from_secs(2);
    while EXECUTED_CALLS.load(Ordering::Relaxed) < 1_000 && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(EXECUTED_CALLS.load(Ordering::Relaxed) >= 1_000);

    let install_signals = spawn_signal_flood(signal_rounds);
    let probes = rapid_specs
        .into_iter()
        .map(|spec| {
            spec.placeholder.release();
            // SAFETY: synthetic dual mappings and hooks live for the process.
            let probe = unsafe { RapidProbe::install(spec.plan, spec.writable_address as *mut u8) }
                .unwrap();
            assert_eq!(
                spec.expected,
                rapid_expected[probe_index(&probe, rapid_base)]
            );
            probe
        })
        .collect::<Vec<_>>();
    install_signals.join().unwrap();
    install_stop.store(true, Ordering::Release);
    join_threads(install_workers);

    let before_trap = TRAP_SIGNALS.load(Ordering::Relaxed);
    // SAFETY: the rapid registry must delegate this unrelated software signal.
    assert_eq!(unsafe { libc::raise(libc::SIGTRAP) }, 0);
    assert_eq!(
        TRAP_SIGNALS.load(Ordering::Relaxed),
        before_trap + 1,
        "unknown SIGTRAP was not delegated"
    );
    assert!(USER_SIGNALS.load(Ordering::Relaxed) > 0);

    let probes = Arc::new(probes);
    probes[0].enable();
    // SAFETY: fixture contains the first synthetic function.
    assert_eq!(unsafe { call_function(rapid_base, 0) }, rapid_expected[0]);
    probes[0].disable();
    assert!(HOOK_CALLS.load(Ordering::Relaxed) > 0);

    let toggle_stop = Arc::new(AtomicBool::new(false));
    let execute_start = Arc::new(Barrier::new(EXECUTOR_THREADS + 1));
    let execute_workers = spawn_executors(
        rapid_base,
        Arc::clone(&rapid_expected),
        Arc::clone(&toggle_stop),
        Arc::clone(&execute_start),
    );
    execute_start.wait();
    let toggle_start = Arc::new(Barrier::new(TOGGLER_THREADS + 1));
    let mut togglers = Vec::new();
    for worker in 0..TOGGLER_THREADS {
        let probes = Arc::clone(&probes);
        let toggle_start = Arc::clone(&toggle_start);
        togglers.push(thread::spawn(move || {
            let mut random = 0xA076_1D64_78BD_642F_u64 ^ worker as u64;
            toggle_start.wait();
            for iteration in 0..rapid_iterations {
                let index = next_random(&mut random) as usize % probes.len();
                probes[index].set_enabled((iteration + worker) & 1 == 0);
            }
        }));
    }
    let toggle_signals = spawn_signal_flood(signal_rounds);
    toggle_start.wait();
    join_threads(togglers);
    toggle_signals.join().unwrap();
    for probe in probes.iter() {
        probe.disable();
        assert!(!probe.is_enabled());
    }
    toggle_stop.store(true, Ordering::Release);
    join_threads(execute_workers);
    for (index, expected) in rapid_expected.iter().copied().enumerate() {
        // SAFETY: every rapid function is restored and remains mapped.
        assert_eq!(unsafe { call_function(rapid_base, index) }, expected);
    }

    let (general_mapping, general_code, general_scan, general_expected) =
        build_general_image(&scanner, general_functions);
    let general_base = general_mapping.executable_address(0);
    let general_expected = Arc::new(general_expected);
    let general_install_stop = Arc::new(AtomicBool::new(false));
    let general_install_start = Arc::new(Barrier::new(EXECUTOR_THREADS + 1));
    let general_install_workers = spawn_executors(
        general_base as usize,
        Arc::clone(&general_expected),
        Arc::clone(&general_install_stop),
        Arc::clone(&general_install_start),
    );
    general_install_start.wait();

    let budget = StalenessBudget::new(3_000).unwrap();
    let hooks = (0..general_functions)
        .map(|index| {
            let offset = function_offset(index);
            let site = HookSite::new(
                &scanner,
                &general_scan,
                &general_code,
                general_base,
                general_mapping.executable_address(offset),
                general_mapping.writable_address(offset) as *mut u8,
            );
            // SAFETY: the process-lifetime dual mapping satisfies HookSite.
            unsafe { InstalledHook::install(site, record_hook, budget) }.unwrap()
        })
        .collect::<Vec<_>>();
    general_install_stop.store(true, Ordering::Release);
    join_threads(general_install_workers);

    let general_stop = Arc::new(AtomicBool::new(false));
    let general_start = Arc::new(Barrier::new(EXECUTOR_THREADS + 1));
    let general_workers = spawn_executors(
        general_base as usize,
        Arc::clone(&general_expected),
        Arc::clone(&general_stop),
        Arc::clone(&general_start),
    );
    general_start.wait();
    let general_signals = spawn_signal_flood(signal_rounds);
    let hook_count_before = HOOK_CALLS.load(Ordering::Relaxed);
    for cycle in 0..general_cycles {
        let index = cycle % hooks.len();
        hooks[index].activate().unwrap();
        // SAFETY: the active trampoline preserves the original result.
        assert_eq!(
            unsafe { call_function(general_base as usize, index) },
            general_expected[index]
        );
        hooks[index].deactivate().unwrap();
    }
    general_signals.join().unwrap();
    general_stop.store(true, Ordering::Release);
    join_threads(general_workers);
    assert!(HOOK_CALLS.load(Ordering::Relaxed) > hook_count_before);
    for hook in &hooks {
        assert!(!hook.is_active());
    }

    eprintln!(
        "M5 stress: rapid_functions={rapid_functions} rapid_stores={} general_functions={general_functions} general_cycles={general_cycles} signals={} executed={} callbacks={}",
        rapid_iterations * TOGGLER_THREADS,
        signal_rounds * 3,
        EXECUTED_CALLS.load(Ordering::Relaxed),
        HOOK_CALLS.load(Ordering::Relaxed),
    );
}

fn probe_index(probe: &RapidProbe, executable_base: usize) -> usize {
    (probe.plan().execute_address() as usize - executable_base - SITE_OFFSET) / CACHE_LINE_BYTES
}

#[test]
#[ignore = "release-mode M5 overhead benchmark"]
fn probe_overhead_benchmark() {
    run_isolated("probe_overhead_benchmark", probe_overhead_benchmark_body);
}

fn probe_overhead_benchmark_body() {
    HOOK_CALLS.store(0, Ordering::Relaxed);
    let scanner = InstructionScanner::default();
    let (mapping, mut specs, expected) = build_rapid_image(&scanner, 1);
    let base = mapping.executable_address(0) as usize;
    let iterations = env_count("LITEINST_BENCH_ITERATIONS", 10_000_000, 100_000_000);

    let baseline = time_calls(base, expected[0], iterations);
    let spec = specs.pop().unwrap();
    spec.placeholder.release();
    // SAFETY: benchmark mappings and callback live for the process.
    let probe =
        unsafe { RapidProbe::install(spec.plan, spec.writable_address as *mut u8) }.unwrap();
    let inactive = time_calls(base, expected[0], iterations);
    probe.enable();
    let active = time_calls(base, expected[0], iterations);
    probe.disable();

    let per_call = |duration: Duration| duration.as_nanos() as f64 / iterations as f64;
    eprintln!(
        "M5 overhead: calls={iterations} baseline={:.3} ns inactive={:.3} ns active={:.3} ns callbacks={}",
        per_call(baseline),
        per_call(inactive),
        per_call(active),
        HOOK_CALLS.load(Ordering::Relaxed),
    );
    assert_eq!(HOOK_CALLS.load(Ordering::Relaxed), iterations as u64);
}

fn time_calls(base: usize, expected: u32, iterations: usize) -> Duration {
    let start = Instant::now();
    let mut checksum = 0_u64;
    for _ in 0..iterations {
        // SAFETY: benchmark image contains one process-lifetime function.
        let value = unsafe { call_function(base, 0) };
        assert_eq!(value, expected);
        checksum = checksum.wrapping_add(value as u64);
    }
    black_box(checksum);
    start.elapsed()
}
