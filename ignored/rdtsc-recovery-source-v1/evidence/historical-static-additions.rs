
const RDTSC_SENTINEL: u64 = 0x1122_3344_5566_7788;
const RDTSCP_SENTINEL: u64 = 0x99aa_bbcc_ddee_ff00;
const RDTSCP_AUX_SENTINEL: u32 = 0x1357_9bdf;

#[derive(Debug, Default)]
struct TimestampLog {
    calls: Mutex<Vec<(Pid, Rdtsc)>>,
}

impl TimestampLog {
    fn calls(&self) -> Vec<(Pid, Rdtsc)> {
        self.calls
            .lock()
            .expect("timestamp log lock poisoned")
            .clone()
    }
}

#[reverie::global_tool]
impl GlobalTool for TimestampLog {
    type Request = Rdtsc;
    type Response = ();
    type Config = bool;

    async fn receive_rpc(&self, from: Pid, request: Rdtsc) {
        self.calls
            .lock()
            .expect("timestamp log lock poisoned")
            .push((from, request));
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct TimestampTool;

#[reverie::tool]
impl Tool for TimestampTool {
    type GlobalState = TimestampLog;
    type ThreadState = ();

    fn subscriptions(enabled: &bool) -> Subscription {
        let mut subscriptions = Subscription::none();
        if *enabled {
            subscriptions.rdtsc();
        }
        subscriptions
    }

    async fn handle_rdtsc_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        request: Rdtsc,
    ) -> Result<RdtscResult, Errno> {
        guest.send_rpc(request).await;
        Ok(match request {
            Rdtsc::Tsc => RdtscResult {
                tsc: RDTSC_SENTINEL,
                aux: None,
            },
            Rdtsc::Tscp => RdtscResult {
                tsc: RDTSCP_SENTINEL,
                aux: Some(RDTSCP_AUX_SENTINEL),
            },
        })
    }
}

fn append_jne_failure(code: &mut Vec<u8>, patches: &mut Vec<usize>) {
    code.extend_from_slice(&[0x0f, 0x85]);
    patches.push(code.len());
    code.extend_from_slice(&0_i32.to_le_bytes());
}

fn timestamp_assertion_program() -> Vec<u8> {
    let mut code = Vec::new();
    let mut failure_patches = Vec::new();

    code.extend_from_slice(&[0x0f, 0x31]); // rdtsc
    code.push(0x3d); // cmp eax, imm32
    code.extend_from_slice(&(RDTSC_SENTINEL as u32).to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);
    code.extend_from_slice(&[0x81, 0xfa]); // cmp edx, imm32
    code.extend_from_slice(&((RDTSC_SENTINEL >> 32) as u32).to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);

    code.extend_from_slice(&[0x0f, 0x01, 0xf9]); // rdtscp
    code.push(0x3d); // cmp eax, imm32
    code.extend_from_slice(&(RDTSCP_SENTINEL as u32).to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);
    code.extend_from_slice(&[0x81, 0xfa]); // cmp edx, imm32
    code.extend_from_slice(&((RDTSCP_SENTINEL >> 32) as u32).to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);
    code.extend_from_slice(&[0x81, 0xf9]); // cmp ecx, imm32
    code.extend_from_slice(&RDTSCP_AUX_SENTINEL.to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);

    code.extend_from_slice(&[
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, SYS_exit
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
    ]);
    let failure = code.len();
    code.extend_from_slice(&[
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, SYS_exit
        0xbf, 0x01, 0x00, 0x00, 0x00, // mov edi, 1
        0x0f, 0x05, // syscall
    ]);

    for patch in failure_patches {
        let displacement = i32::try_from(failure).unwrap() - i32::try_from(patch + 4).unwrap();
        code[patch..patch + 4].copy_from_slice(&displacement.to_le_bytes());
    }
    code
}

fn run_timestamp_probe(image: &[u8], subscribed: bool) -> (TimestampLog, i32) {
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(image, "/bin/timestamp-probe")
        .unwrap();
    let (log, code, stdout, stderr) = futures::executor::block_on(
        backend.run_static_elf_with_tool::<TimestampTool>(subscribed, true),
    )
    .unwrap();
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    (log, code)
}

#[test]
fn static_elf_timestamp_reads_dispatch_exact_tool_results_repeatably() {
    if !kvm_available("KVM timestamp dispatch test") {
        return;
    }

    let image = static_elf(&timestamp_assertion_program());
    for _ in 0..2 {
        let (log, code) = run_timestamp_probe(&image, true);
        assert_eq!(code, 0);
        assert_eq!(
            log.calls(),
            vec![
                (Pid::from_raw(1), Rdtsc::Tsc),
                (Pid::from_raw(1), Rdtsc::Tscp)
            ]
        );
    }
}

#[test]
fn timestamp_dispatch_survives_thread_fork_and_exec_vcpu_lifecycles() {
    if !kvm_available("KVM timestamp lifecycle test") {
        return;
    }

    let directory = TestDirectory::new();
    let source = format!(
        r#"
#include <pthread.h>
#include <stdint.h>
#include <sys/wait.h>
#include <unistd.h>

static uint64_t read_tsc(void) {{
    unsigned int low;
    unsigned int high;
    __asm__ volatile("rdtsc" : "=a"(low), "=d"(high));
    return ((uint64_t)high << 32) | low;
}}

static void *thread_main(void *unused) {{
    (void)unused;
    return (void *)(uintptr_t)(read_tsc() != UINT64_C({RDTSC_SENTINEL}));
}}

int main(int argc, char **argv) {{
    if (read_tsc() != UINT64_C({RDTSC_SENTINEL})) return 10;
    if (argc > 1) return 0;

    pthread_t thread;
    if (pthread_create(&thread, NULL, thread_main, NULL) != 0) return 11;
    void *thread_result = NULL;
    if (pthread_join(thread, &thread_result) != 0 || thread_result != NULL) return 12;

    pid_t child = fork();
    if (child < 0) return 13;
    if (child == 0) {{
        if (read_tsc() != UINT64_C({RDTSC_SENTINEL})) _exit(14);
        execl(argv[0], argv[0], "post-exec", NULL);
        _exit(15);
    }}
    int status = 0;
    if (waitpid(child, &status, 0) != child) return 16;
    return WIFEXITED(status) ? WEXITSTATUS(status) : 17;
}}
"#
    );
    let executable = compile_c_program(&directory.0, "timestamp-lifecycle", &source);
    let image = std::fs::read(&executable).unwrap();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_with_context(
            &image,
            &[executable.to_str().unwrap()],
            &["PATH=/usr/bin:/bin"],
            &directory.0,
        )
        .unwrap();
    let (log, code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<TimestampTool>(true, true))
            .unwrap();
    assert_eq!(code, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let calls = log.calls();
    let mut senders = calls
        .iter()
        .map(|(sender, _)| sender.as_raw())
        .collect::<Vec<_>>();
    senders.sort_unstable();
    senders.dedup();
    assert!(
        senders.len() >= 3,
        "expected root, pthread, and fork-child timestamp senders; calls={calls:?}"
    );
    let child = *senders.iter().max().unwrap();
    assert!(
        calls
            .iter()
            .filter(|(sender, _)| sender.as_raw() == child)
            .count()
            >= 2,
        "fork child must dispatch timestamps before and after exec; calls={calls:?}"
    );
}

#[test]
fn static_elf_unsubscribed_rdtsc_runs_without_tool_dispatch() {
    if !kvm_available("KVM unsubscribed RDTSC test") {
        return;
    }

    let image = static_elf(&[
        0x0f, 0x31, // rdtsc
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, SYS_exit
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
    ]);
    let (log, code) = run_timestamp_probe(&image, false);
    assert_eq!(code, 0);
    assert!(log.calls().is_empty());
}

#[test]
fn subscribed_timestamp_dispatch_refuses_unrelated_exceptions() {
    if !kvm_available("KVM timestamp exception refusal test") {
        return;
    }

    let mut invalid_opcode_backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    invalid_opcode_backend
        .install_static_elf(&static_elf(&[0x0f, 0x0b]), "/bin/fault")
        .unwrap();
    let error = futures::executor::block_on(
        invalid_opcode_backend.run_static_elf_with_tool::<TimestampTool>(true, true),
    )
    .unwrap_err();
    assert_invalid_opcode(error);

    let mut general_protection_backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    general_protection_backend
        .install_static_elf(&static_elf(&[0xed]), "/bin/fault")
        .unwrap();
    let error = futures::executor::block_on(
        general_protection_backend.run_static_elf_with_tool::<TimestampTool>(true, true),
    )
    .unwrap_err();
    assert_general_protection(error);
}

#[derive(Clone, Copy, Debug, Default)]
struct EvolvingTimestampTool;

#[reverie::tool]
impl Tool for EvolvingTimestampTool {
    type GlobalState = TimestampLog;
    type ThreadState = u64;

    fn subscriptions(enabled: &bool) -> Subscription {
        let mut subscriptions = Subscription::none();
        if *enabled {
            subscriptions.rdtsc();
        }
        subscriptions
    }

    async fn handle_rdtsc_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        request: Rdtsc,
    ) -> Result<RdtscResult, Errno> {
        let ordinal = *guest.thread_state();
        *guest.thread_state_mut() += 1;
        guest.send_rpc(request).await;
        Ok(RdtscResult {
            tsc: RDTSC_SENTINEL + ordinal,
            aux: (request == Rdtsc::Tscp).then_some(RDTSCP_AUX_SENTINEL + ordinal as u32),
        })
    }
}

fn evolving_timestamp_assertion_program() -> Vec<u8> {
    let mut code = Vec::new();
    let mut failure_patches = Vec::new();

    for expected in [RDTSC_SENTINEL, RDTSC_SENTINEL + 1] {
        code.extend_from_slice(&[0x0f, 0x31]); // rdtsc
        code.push(0x3d); // cmp eax, imm32
        code.extend_from_slice(&(expected as u32).to_le_bytes());
        append_jne_failure(&mut code, &mut failure_patches);
        code.extend_from_slice(&[0x81, 0xfa]); // cmp edx, imm32
        code.extend_from_slice(&((expected >> 32) as u32).to_le_bytes());
        append_jne_failure(&mut code, &mut failure_patches);
    }

    let rdtscp_expected = RDTSC_SENTINEL + 2;
    code.extend_from_slice(&[0x0f, 0x01, 0xf9]); // rdtscp
    code.push(0x3d); // cmp eax, imm32
    code.extend_from_slice(&(rdtscp_expected as u32).to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);
    code.extend_from_slice(&[0x81, 0xfa]); // cmp edx, imm32
    code.extend_from_slice(&((rdtscp_expected >> 32) as u32).to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);
    code.extend_from_slice(&[0x81, 0xf9]); // cmp ecx, imm32
    code.extend_from_slice(&(RDTSCP_AUX_SENTINEL + 2).to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);

    code.extend_from_slice(&[
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, SYS_exit
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
    ]);
    let failure = code.len();
    code.extend_from_slice(&[
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, SYS_exit
        0xbf, 0x01, 0x00, 0x00, 0x00, // mov edi, 1
        0x0f, 0x05, // syscall
    ]);

    for patch in failure_patches {
        let displacement = i32::try_from(failure).unwrap() - i32::try_from(patch + 4).unwrap();
        code[patch..patch + 4].copy_from_slice(&displacement.to_le_bytes());
    }
    code
}

#[test]
fn repeated_timestamp_reads_evolve_once_per_instruction() {
    if !kvm_available("KVM evolving timestamp test") {
        return;
    }

    let image = static_elf(&evolving_timestamp_assertion_program());
    for _ in 0..2 {
        let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
        backend
            .install_static_elf(&image, "/bin/evolving-timestamp-probe")
            .unwrap();
        let (log, code, stdout, stderr) = futures::executor::block_on(
            backend.run_static_elf_with_tool::<EvolvingTimestampTool>(true, true),
        )
        .unwrap();
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(
            log.calls(),
            vec![
                (Pid::from_raw(1), Rdtsc::Tsc),
                (Pid::from_raw(1), Rdtsc::Tsc),
                (Pid::from_raw(1), Rdtsc::Tscp),
            ]
        );
    }
}

#[test]
fn static_elf_unsubscribed_rdtscp_remains_guest_exception() {
    if !kvm_available("KVM unsubscribed RDTSCP test") {
        return;
    }

    let image = static_elf(&[
        0x0f, 0x01, 0xf9, // rdtscp
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, SYS_exit
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
    ]);
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&image, "/bin/unsubscribed-rdtscp")
        .unwrap();
    let error =
        futures::executor::block_on(backend.run_static_elf_with_tool::<TimestampTool>(false, true))
            .unwrap_err();
    assert_invalid_opcode(error);
}
