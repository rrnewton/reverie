//! Feature-only C entry points for bounded ToolRegion spare-lease controls.
//! These exports select this native root's actual Fixed Tool allocator backing.

use reverie_inguest::guest::tool_region::mapping_fixture;
pub use reverie_inguest::guest::tool_region::mapping_fixture::MappingFixtureSnapshot;

/// Prepare a real spare lease and a verified free extent after runtime setup.
///
/// # Safety
/// The caller satisfies [`mapping_fixture::prepare`]'s ordinary single-thread
/// and output-record contract; this image declares the Fixed Tool allocator.
/// Never invoke this export inside a callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn m6_mapping_fixture_prepare(
    output: *mut MappingFixtureSnapshot,
    bytes: usize,
) -> i32 {
    unsafe { mapping_fixture::prepare(mapping_fixture::Backing::Fixed, output, bytes) }
}

/// Observe the retained spare-lease and publication facts without mutation.
///
/// # Safety
/// The caller satisfies [`mapping_fixture::observe`]'s contract in this Fixed image.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn m6_mapping_fixture_observe(
    output: *mut MappingFixtureSnapshot,
    bytes: usize,
) -> i32 {
    unsafe { mapping_fixture::observe(mapping_fixture::Backing::Fixed, output, bytes) }
}

/// Lease/reprotect/reset the originally observed free extent via trusted APIs.
///
/// # Safety
/// The caller satisfies [`mapping_fixture::reuse`]'s contract in this Fixed image.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn m6_mapping_fixture_reuse(
    output: *mut MappingFixtureSnapshot,
    bytes: usize,
) -> i32 {
    unsafe { mapping_fixture::reuse(mapping_fixture::Backing::Fixed, output, bytes) }
}

/// Release only the fixture's own spare lease and its retained Tool state.
///
/// # Safety
/// The caller satisfies [`mapping_fixture::finish`]'s contract in this Fixed image.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn m6_mapping_fixture_finish() -> i32 {
    unsafe { mapping_fixture::finish(mapping_fixture::Backing::Fixed) }
}

// These C owner-protection controls exercise the real shared FullTool host,
// using the same qualified Fixed leaf and the normal installer. Both feature
// configurations expose it so the independent raw-mseal regression can prove
// the shared host guard even when the legacy selectors are available.
mod full_tool {
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;

    use reverie::Error;
    use reverie::Guest;
    use reverie::Tool;
    use reverie::syscalls::Syscall;
    use reverie::syscalls::SyscallInfo;

    static INSTALL_RESULT: AtomicI32 = AtomicI32::new(-libc::EINPROGRESS);
    static CALLBACKS: [AtomicU64; 1024] = [const { AtomicU64::new(0) }; 1024];

    #[derive(Default)]
    struct MappingFixtureTool;

    #[reverie::tool]
    impl Tool for MappingFixtureTool {
        type GlobalState = ();
        type ThreadState = ();

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            CALLBACKS[syscall.number() as usize].fetch_add(1, Ordering::Relaxed);
            Ok(guest.inject(syscall).await?)
        }
    }

    unsafe extern "C" fn install_body() {
        // generic_entry already authenticated and published its fresh scalar
        // stack. Enter the declared Fixed Tool allocator before any env/path
        // allocation, just as the regular explicit generic initializer does.
        let _private_allocations = reverie_inguest::guest::alloc::enter_dispatch();
        let installed = crate::patch_alloc::preflight_allocator().and_then(|()| {
            let coordinator = std::env::var_os(crate::COORDINATOR_ENV).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "mapping FullTool fixture requires its actual coordinator socket",
                )
            })?;
            // SAFETY: the C fixture invokes this exactly once in single-thread
            // startup, before any filter or application-created thread.
            unsafe {
                crate::install_tool_quiescent::<MappingFixtureTool>(std::path::PathBuf::from(
                    coordinator,
                ))
            }
        });
        let result = match installed {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("mapping FullTool fixture installation failed: {error}");
                -error.raw_os_error().unwrap_or(libc::EIO)
            }
        };
        INSTALL_RESULT.store(result, Ordering::Release);
    }

    /// Install the C fixture's genuine FullTool on the qualified Fixed leaf.
    ///
    /// # Safety
    /// Call once in trusted single-thread startup, with clear DF, before any
    /// application seccomp filter or runtime installation. The shared native
    /// generic entry owns and authenticates the actual first Rust setup stack.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn m6_mapping_fixture_install_full_tool() -> i32 {
        unsafe { reverie_inguest::guest::tool_region::generic_entry(install_body) };
        INSTALL_RESULT.load(Ordering::Acquire)
    }

    /// Actual entries into the fixture's subscribed Tool callback.
    #[unsafe(no_mangle)]
    pub extern "C" fn m6_mapping_fixture_full_tool_callback_count(number: i64) -> u64 {
        usize::try_from(number)
            .ok()
            .and_then(|number| CALLBACKS.get(number))
            .map_or(0, |count| count.load(Ordering::Relaxed))
    }
}
