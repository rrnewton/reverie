//! Exercise the real stopped-guest vDSO patch with initialized kernel state.
#![cfg(target_arch = "x86_64")]

use std::sync::Mutex;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Syscall;
use reverie::syscalls::Sysno;

type VdsoGetrandom =
    unsafe extern "C" fn(*mut libc::c_void, usize, libc::c_uint, *mut libc::c_void, usize) -> isize;

#[repr(C)]
#[derive(Default)]
struct AllocationParameters {
    state_size: u32,
    mmap_prot: u32,
    mmap_flags: u32,
    reserved: [u32; 13],
}

struct InitializedVdso {
    function: VdsoGetrandom,
    library: *mut libc::c_void,
    state: *mut libc::c_void,
    state_size: usize,
}

impl InitializedVdso {
    fn new() -> Self {
        unsafe {
            let library = libc::dlopen(
                c"linux-vdso.so.1".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            );
            assert!(!library.is_null(), "open actual host vDSO");
            let symbol = libc::dlsym(library, c"__vdso_getrandom".as_ptr());
            assert!(
                !symbol.is_null(),
                "this regression needs the actual getrandom vDSO"
            );
            let function: VdsoGetrandom = std::mem::transmute(symbol);
            let mut params = AllocationParameters::default();
            assert_eq!(std::mem::size_of_val(&params), 64);
            assert_eq!(
                function(
                    std::ptr::null_mut(),
                    0,
                    0,
                    (&mut params as *mut AllocationParameters).cast(),
                    usize::MAX
                ),
                0
            );
            assert!(params.state_size > 0);
            let state_size = params.state_size as usize;
            // Use the kernel's actual protection and MAP_DROPPABLE flags,
            // rather than substituting ordinary private anonymous memory.
            let state = libc::mmap(
                std::ptr::null_mut(),
                state_size,
                params.mmap_prot as i32,
                params.mmap_flags as i32,
                -1,
                0,
            );
            assert_ne!(state, libc::MAP_FAILED);
            let mut bytes = [0_u8; 16];
            assert_eq!(
                function(bytes.as_mut_ptr().cast(), bytes.len(), 0, state, state_size),
                16
            );
            Self {
                function,
                library,
                state,
                state_size,
            }
        }
    }
}

impl Drop for InitializedVdso {
    fn drop(&mut self) {
        unsafe {
            assert_eq!(libc::munmap(self.state, self.state_size), 0);
            assert_eq!(libc::dlclose(self.library), 0);
        }
    }
}

#[derive(Default)]
struct RandomCalls(Mutex<Vec<(usize, usize)>>);

#[reverie::global_tool]
impl GlobalTool for RandomCalls {
    type Request = (usize, usize);
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, call: Self::Request) {
        self.0.lock().unwrap().push(call);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RandomTool;

#[reverie::tool]
impl Tool for RandomTool {
    type GlobalState = RandomCalls;
    type ThreadState = ();

    fn subscriptions(_cfg: &()) -> Subscription {
        [Sysno::getrandom].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let Syscall::Getrandom(call) = syscall else {
            panic!("unexpected subscribed syscall: {syscall:?}");
        };
        guest.send_rpc((call.buflen(), call.flags())).await;
        if call.flags() == libc::GRND_NONBLOCK as usize {
            assert_eq!(call.buflen(), 16);
            guest
                .memory()
                .write_exact(call.buf().expect("ordinary buffer"), &[0x42; 16])?;
            Ok(16)
        } else {
            assert_eq!((call.buflen(), call.flags()), (0, u32::MAX as usize));
            Ok(-i64::from(libc::EINVAL))
        }
    }
}

#[test]
fn subscribed_vdso_rejects_query_and_dispatches_initialized_ordinary_calls() {
    let initialized = InitializedVdso::new();
    let log = reverie_ptrace::testing::check_fn::<RandomTool, _>(|| unsafe {
        let mut guarded_params = [0xa5_u8; 80];
        assert_eq!(
            (initialized.function)(
                std::ptr::null_mut(),
                0,
                0,
                guarded_params.as_mut_ptr().add(8).cast(),
                usize::MAX
            ),
            -(libc::ENOSYS as isize)
        );
        assert_eq!(guarded_params, [0xa5; 80]);
        let mut guarded_bytes = [0xa5_u8; 32];
        assert_eq!(
            (initialized.function)(
                guarded_bytes.as_mut_ptr().add(8).cast(),
                16,
                libc::GRND_NONBLOCK,
                initialized.state,
                initialized.state_size
            ),
            16
        );
        assert_eq!(&guarded_bytes[..8], &[0xa5; 8]);
        assert_eq!(&guarded_bytes[8..24], &[0x42; 16]);
        assert_eq!(&guarded_bytes[24..], &[0xa5; 8]);
        assert_eq!(
            (initialized.function)(
                std::ptr::null_mut(),
                0,
                u32::MAX,
                initialized.state,
                initialized.state_size
            ),
            -(libc::EINVAL as isize)
        );
    });
    assert_eq!(
        *log.0.lock().unwrap(),
        vec![(16, libc::GRND_NONBLOCK as usize), (0, u32::MAX as usize)]
    );
}
