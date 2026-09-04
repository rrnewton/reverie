use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie_rpc_transport::guest_log::HostProducer;
use reverie_rpc_transport::guest_log::LogHandle;
use reverie_rpc_transport::guest_log::PublishError;
use reverie_rpc_transport::guest_log::fixture::Control;

pub mod records;

static SELECTED: AtomicBool = AtomicBool::new(false);
static HOST: OnceLock<Host> = OnceLock::new();
pub static DROPS: AtomicU64 = AtomicU64::new(0);
pub static RPC_ENTRIES: AtomicU64 = AtomicU64::new(0);
pub type HostRecord = (Vec<u8>, Result<u64, PublishError>);

pub struct Host {
    pub producer: HostProducer,
    pub handle: LogHandle,
    pub control: Arc<Control>,
    pub records: Mutex<Vec<HostRecord>>,
}

pub fn configure(producer: HostProducer, handle: LogHandle, control: Arc<Control>) {
    assert!(handle.capture_snapshot().unwrap().publication.ready);
    assert!(
        HOST.set(Host {
            producer,
            handle,
            control,
            records: Mutex::new(Vec::new())
        })
        .is_ok()
    );
    assert_eq!(host_record(records::READY).unwrap(), 1);
}

pub fn host() -> &'static Host {
    HOST.get().expect("V4 host configured before GlobalTool")
}

pub fn host_record(bytes: &[u8]) -> Result<u64, PublishError> {
    let host = host();
    let result = host.producer.write_record(bytes).map(|commit| commit.order);
    host.records.lock().unwrap().push((bytes.to_vec(), result));
    result
}

pub fn global_init() {
    if let Some(host) = HOST.get() {
        assert!(host.handle.capture_snapshot().unwrap().publication.ready);
        assert_eq!(host_record(records::INIT).unwrap(), 2);
    }
}

pub fn rpc_entry(request: u64) {
    if let Some(host) = HOST.get() {
        let index = RPC_ENTRIES.fetch_add(1, Ordering::Relaxed) as usize;
        let order = host.control.observations().guest_orders[index].load(Ordering::Acquire);
        if request == u64::MAX {
            assert_eq!(index, 1);
            assert_eq!(order, 0);
            assert_eq!(host_record(&records::rpc(index, request)).unwrap(), 5);
        } else {
            assert_eq!(request, records::request(super::clocked(), index));
            assert_eq!(order, 3 + 2 * index as u64);
            assert_eq!(
                host_record(&records::rpc(index, request)).unwrap(),
                order + 1
            );
        }
    }
}

impl Drop for crate::ClockGlobal {
    fn drop(&mut self) {
        if HOST.get().is_some() {
            let _ = host_record(records::DROP);
            DROPS.fetch_add(1, Ordering::Release);
        }
    }
}

pub fn selected() -> bool {
    SELECTED.load(Ordering::Relaxed)
}

pub fn select_guest(data: &[u8]) -> std::io::Result<()> {
    if &data[..8] == b"LOGTEST4" {
        if ![1, 2].contains(&data[10]) {
            return Err(std::io::Error::other("V4 pressure profile"));
        }
        SELECTED.store(true, Ordering::Relaxed);
    }
    Ok(())
}

pub fn emit(index: usize, writer: &mut reverie_liteinst::GuestLogWriter) {
    let case = super::CASE.load(Ordering::Relaxed);
    if index == 1 {
        if case == 2 {
            return;
        }
        if case == 3 {
            reverie_liteinst::guest_log_fixture::await_prefix();
        }
        if case == 4 {
            writer.record_failed().unwrap();
            reverie_liteinst::guest_log_fixture::record_failed();
            unsafe {
                reverie_preload::trap::raw_syscall6(libc::SYS_exit_group, [127, 0, 0, 0, 0, 0]);
            }
            loop {
                core::hint::spin_loop();
            }
        }
    }
    let suffix = [0, index as u8, 0xff];
    let bytes = match index {
        0 => super::PREFIX,
        1 => &super::BURST,
        _ => &suffix,
    };
    let commit = writer.write_record(bytes).unwrap();
    assert_eq!(commit.order, 3 + index as u64 * 2);
    reverie_liteinst::guest_log_fixture::record_commit(index, commit.order);
}
