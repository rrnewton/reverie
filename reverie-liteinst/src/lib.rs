#![feature(thread_local)]
#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("reverie-liteinst requires Linux x86-64");

mod pun;
mod runtime;

// TODO-HUMAN-REVIEW(PR-id): this constructor installs process-wide signal and seccomp state.
/// Initializes the preload runtime when selected by the launcher environment.
///
/// # Safety
///
/// The dynamic loader must call this exactly once before application threads
/// start. Calling it again would stack an irreversible seccomp filter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn reverie_liteinst_initialize() {
    if let Err(error) = runtime::initialize_from_environment() {
        eprintln!("reverie-liteinst initialization failed: {error}");
        unsafe {
            libc::_exit(127);
        }
    }
}

#[used]
#[unsafe(link_section = ".init_array")]
static REVERIE_LITEINST_INIT: unsafe extern "C" fn() = reverie_liteinst_initialize;
