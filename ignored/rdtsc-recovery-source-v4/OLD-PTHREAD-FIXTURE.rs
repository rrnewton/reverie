#[test]
fn host_owned_timestamp_worker_keeps_native_execution() {
    if !kvm_available("KVM Host-owned timestamp worker test") {
        return;
    }
    let directory = TestDirectory::new();
    let source = format!(
        r#"
#include <pthread.h>
#include <stdint.h>
static uint64_t read_tsc(void) {{
    unsigned low, high;
    __asm__ volatile("rdtsc" : "=a"(low), "=d"(high));
    return ((uint64_t)high << 32) | low;
}}
static void *worker(void *argument) {{
    (void)read_tsc();
    return argument;
}}
int main(void) {{
    if (read_tsc() != UINT64_C({RDTSC_SENTINEL})) return 10;
    pthread_t thread;
    void *result = 0;
    if (pthread_create(&thread, 0, worker, (void *)(uintptr_t)0x1234)) return 11;
    if (pthread_join(thread, &result)) return 12;
    if (result != (void *)(uintptr_t)0x1234) return 13;
    return read_tsc() == UINT64_C({RDTSC_SENTINEL}) ? 0 : 14;
}}
"#
    );
    let executable = compile_c_program(&directory.0, "timestamp-host-worker", &source);
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend.set_thread_ownership(ThreadOwnership::Host);
    backend
        .install_static_elf_with_context(
            &std::fs::read(&executable).unwrap(),
            &[executable.to_str().unwrap()],
            &["PATH=/usr/bin:/bin"],
            &directory.0,
        )
        .unwrap();
    let (log, status, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<TimestampTool>(true, true))
            .unwrap();
    assert_eq!(status, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        log.calls(),
        vec![
            (Pid::from_raw(1), Rdtsc::Tsc),
            (Pid::from_raw(1), Rdtsc::Tsc)
        ]
    );
}
