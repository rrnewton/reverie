use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tid;
use reverie::Tool;
use reverie::process::Command;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::LiteinstBackend;

#[derive(Default)]
struct EventCounter {
    delivered: AtomicU64,
    last_getpid_rip: AtomicU64,
    last_getpid_r12: AtomicU64,
    helper_mprotect_callbacks: AtomicU64,
}

#[reverie::global_tool]
impl GlobalTool for EventCounter {
    type Request = u64;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Tid, increment: u64) {
        if increment & (1_u64 << 63) != 0 {
            self.last_getpid_rip
                .store(increment & ((1_u64 << 62) - 1), Ordering::SeqCst);
            self.delivered.fetch_add(1, Ordering::SeqCst);
        } else if increment & (1_u64 << 62) != 0 {
            self.last_getpid_r12
                .store(increment & ((1_u64 << 62) - 1), Ordering::SeqCst);
        } else if increment & (1_u64 << 61) != 0 {
            self.helper_mprotect_callbacks
                .fetch_add(1, Ordering::SeqCst);
        } else {
            self.delivered.fetch_add(increment, Ordering::SeqCst);
        }
    }
}

#[derive(Default)]
struct CountSyscalls;

#[reverie::tool]
impl Tool for CountSyscalls {
    type GlobalState = EventCounter;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getrandom, Sysno::getpid, Sysno::mprotect]
            .into_iter()
            .collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert!(matches!(
            syscall.number(),
            Sysno::getrandom | Sysno::getpid | Sysno::mprotect
        ));
        if syscall.number() == Sysno::getpid {
            let regs = guest.regs().await;
            guest.send_rpc((1_u64 << 62) | regs.r12).await;
            guest.send_rpc((1_u64 << 63) | regs.rip).await;
        } else if syscall.number() == Sysno::mprotect {
            guest.send_rpc(1_u64 << 61).await;
        } else {
            guest.send_rpc(1).await;
        }
        Ok(guest.inject(syscall).await?)
    }
}

fn preload_path() -> PathBuf {
    let launcher = PathBuf::from(env!("CARGO_BIN_EXE_reverie-liteinst-strace"));
    let target = launcher.parent().unwrap();
    [
        target.join("libreverie_liteinst.so"),
        target.join("deps/libreverie_liteinst.so"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .expect("cargo did not build the LiteInst preload cdylib")
}

fn compile_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let output = directory.path().join(name.trim_end_matches(".c"));
    let compiler = std::env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
    let result = ProcessCommand::new(compiler)
        .args(["-std=gnu11", "-O0", "-fno-pie", "-no-pie"])
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "failed to compile {}:\n{}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    (directory, output)
}

fn symbol_address(binary: &std::path::Path, symbol: &str) -> u64 {
    let output = ProcessCommand::new("nm").arg(binary).output().unwrap();
    assert!(output.status.success(), "nm failed: {output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let address = fields.next()?;
            let _kind = fields.next()?;
            (fields.next()? == symbol).then(|| u64::from_str_radix(address, 16).unwrap())
        })
        .unwrap_or_else(|| panic!("missing symbol {symbol}"))
}

#[tokio::test(flavor = "current_thread")]
async fn host_lifecycle_observes_allocator_and_explicit_getrandom() {
    let (_directory, guest) = compile_fixture("allocator_getrandom.c");
    let (output, global) = LiteinstBackend::run_host_with_output_and_preload::<CountSyscalls>(
        Command::new(guest),
        (),
        preload_path(),
    )
    .await
    .unwrap();

    assert_eq!(
        global.delivered.load(Ordering::SeqCst),
        3,
        "host lifecycle missed allocator/pre-constructor entropy: {output:?}"
    );
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn first_site_is_installed_once_and_hot_calls_use_liteinst() {
    let (_baseline_directory, baseline_guest) = compile_fixture("allocator_getrandom.c");
    let (baseline_output, baseline_global) = LiteinstBackend::run_host_with_output_and_preload::<
        CountSyscalls,
    >(Command::new(baseline_guest), (), preload_path())
    .await
    .unwrap();
    assert!(baseline_output.status.success(), "{baseline_output:?}");

    let (_directory, guest) = compile_fixture("hybrid_hot_site.c");
    let site = symbol_address(&guest, "reverie_liteinst_hybrid_getpid_site");
    let (output, global) = LiteinstBackend::run_host_with_output_and_preload::<CountSyscalls>(
        Command::new(guest),
        (),
        preload_path(),
    )
    .await
    .unwrap();

    assert_eq!(
        output.stdout, b"calls=32 traps=1 hooks=31 ac=0 spoofs=2\n",
        "{output:?}"
    );
    assert_eq!(global.delivered.load(Ordering::SeqCst), 33, "{output:?}");
    assert_eq!(
        global.last_getpid_rip.load(Ordering::SeqCst),
        site + 2,
        "the host Tool must see the original logical post-syscall RIP"
    );
    assert_eq!(
        global.last_getpid_r12.load(Ordering::SeqCst),
        0x0012_3456_789a_bcde,
        "logical guest R12 must remain distinct from the controller HookContext base"
    );
    assert_eq!(
        global.helper_mprotect_callbacks.load(Ordering::SeqCst)
            - baseline_global
                .helper_mprotect_callbacks
                .load(Ordering::SeqCst),
        0,
        "the patch helper must add zero mprotect Tool callbacks above the loader baseline"
    );
    assert!(output.status.success(), "{output:?}");
}
