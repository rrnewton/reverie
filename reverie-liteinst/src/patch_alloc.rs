//! The allocation reserve for instrumentation planning.
//!
//! A seccomp SIGSYS handler cannot let the system allocator issue brk or mmap.
//! During a bounded installation scope, allocations from liteinst2 and
//! iced-x86 therefore come from this prepublished process-lifetime buffer.
//! Objects that survive registration are intentionally never reclaimed. Preload
//! leaves use [`PrivatePatchAllocator`], whose other allocations always use the
//! reusable Tool arena. Embedded legacy Tools may explicitly use [`PatchAllocator`]
//! to preserve their scoped allocation behavior.

use core::alloc::GlobalAlloc;
use core::alloc::Layout;
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;
use std::cell::Cell;

use reverie_inguest::guest::alloc::GuestAllocator;
use reverie_inguest::guest::alloc::PrivateToolAllocator;
use reverie_inguest::guest::alloc::tool_heap_contains;

const PATCH_HEAP_BYTES: usize = 32 * 1024 * 1024;

struct PatchHeap {
    bytes: UnsafeCell<[u8; PATCH_HEAP_BYTES]>,
    next: AtomicUsize,
}

// SAFETY: reservations use a single atomic cursor and never overlap.
unsafe impl Sync for PatchHeap {}

impl PatchHeap {
    const fn new() -> Self {
        Self {
            bytes: UnsafeCell::new([0; PATCH_HEAP_BYTES]),
            next: AtomicUsize::new(0),
        }
    }

    fn allocate(&self, layout: Layout) -> *mut u8 {
        let base = self.bytes.get().cast::<u8>() as usize;
        let mut current = self.next.load(Ordering::Relaxed);
        loop {
            let Some(aligned_address) = base
                .checked_add(current)
                .and_then(|address| address.checked_add(layout.align() - 1))
                .map(|address| address & !(layout.align() - 1))
            else {
                return ptr::null_mut();
            };
            let offset = aligned_address - base;
            let Some(end) = offset.checked_add(layout.size()) else {
                return ptr::null_mut();
            };
            if end > PATCH_HEAP_BYTES {
                return ptr::null_mut();
            }
            match self
                .next
                .compare_exchange_weak(current, end, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return aligned_address as *mut u8,
                Err(observed) => current = observed,
            }
        }
    }

    fn contains(&self, pointer: *mut u8) -> bool {
        let base = self.bytes.get().cast::<u8>() as usize;
        (base..base + PATCH_HEAP_BYTES).contains(&(pointer as usize))
    }
}

static PATCH_HEAP: PatchHeap = PatchHeap::new();
thread_local! {
    // Const, no-drop TLS is important here: the global allocator consults
    // this counter before it can choose a safe backing heap.
    static INSTALLATION_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub(crate) struct PatchAllocationScope;

impl Drop for PatchAllocationScope {
    fn drop(&mut self) {
        INSTALLATION_DEPTH.set(INSTALLATION_DEPTH.get() - 1);
    }
}

pub(crate) fn enter() -> PatchAllocationScope {
    INSTALLATION_DEPTH.set(INSTALLATION_DEPTH.get() + 1);
    PatchAllocationScope
}

fn installation_active() -> bool {
    INSTALLATION_DEPTH.get() != 0
}

#[cfg(feature = "allocator-fixture")]
pub(crate) fn fixture_installation_depth() -> usize {
    INSTALLATION_DEPTH.get()
}

#[cfg(feature = "allocator-fixture")]
pub(crate) fn fixture_patch_heap_bounds() -> (usize, usize) {
    let base = PATCH_HEAP.bytes.get().cast::<u8>() as usize;
    (base, base + PATCH_HEAP_BYTES)
}

/// Legacy allocator for embedded Tools: private during installation/dispatch,
/// System outside those scopes. It is not the isolated preload allocator.
pub struct PatchAllocator;

/// Allocator for a real preload: every Rust allocation uses one of the two
/// Tool-owned arenas, independent of dispatch depth. Installation storage has
/// process lifetime; other allocations can be reused after deallocation.
pub struct PrivatePatchAllocator;

impl PrivatePatchAllocator {
    /// Check arbitrary-address backing-store ownership without dereferencing it.
    /// A mismatch exits through the trusted gate with status 127. This is only
    /// a range check, not proof that the address names a live allocation.
    #[doc(hidden)]
    pub fn require_owned_address(address: usize) {
        if !PATCH_HEAP.contains(address as *mut u8) {
            PrivateToolAllocator::require_owned_address(address);
        }
    }
}

/// Check the caller's compiler-selected allocator before any Tool installation
/// effects. The caller holds a dispatch allocation scope for legacy roots.
pub(crate) fn preflight_allocator() -> std::io::Result<()> {
    let layout = Layout::new::<u64>();
    // SAFETY: a nonzero valid layout; freed below through the same allocator.
    let pointer = unsafe { std::alloc::alloc(layout) };
    if pointer.is_null() {
        return Err(std::io::Error::from_raw_os_error(libc::ENOMEM));
    }
    let owned = PATCH_HEAP.contains(pointer) || tool_heap_contains(pointer);
    // SAFETY: pointer is the live allocation just returned by std::alloc.
    unsafe { std::alloc::dealloc(pointer, layout) };
    if owned {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(libc::EOPNOTSUPP))
    }
}

// SAFETY: allocations honor Layout within disjoint owned arenas. Reallocation
// and deallocation follow pointer ownership even after allocation scopes end.
unsafe impl GlobalAlloc for PrivatePatchAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if installation_active() {
            PATCH_HEAP.allocate(layout)
        } else {
            // SAFETY: forwarded with the caller's valid layout.
            unsafe { PrivateToolAllocator.alloc(layout) }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { self.alloc(layout) };
        if !pointer.is_null() {
            // SAFETY: the allocation holds layout.size writable bytes.
            unsafe { pointer.write_bytes(0, layout.size()) };
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Self::require_owned_address(pointer as usize);
        if !PATCH_HEAP.contains(pointer) {
            // SAFETY: the caller supplies a live allocation from the Tool heap.
            unsafe { PrivateToolAllocator.dealloc(pointer, layout) };
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        Self::require_owned_address(pointer as usize);
        if !PATCH_HEAP.contains(pointer) {
            // SAFETY: the live pointer belongs to the reusable Tool heap.
            return unsafe { PrivateToolAllocator.realloc(pointer, old, new_size) };
        }
        let Ok(layout) = Layout::from_size_align(new_size, old.align()) else {
            return ptr::null_mut();
        };
        let replacement = PATCH_HEAP.allocate(layout);
        if !replacement.is_null() {
            // SAFETY: the fresh patch reservation cannot overlap the source.
            unsafe { ptr::copy_nonoverlapping(pointer, replacement, old.size().min(new_size)) };
        }
        replacement
    }
}

// SAFETY: installation allocations use process-lifetime storage that is never
// freed; everything else is GuestAllocator's.
unsafe impl GlobalAlloc for PatchAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if installation_active() {
            PATCH_HEAP.allocate(layout)
        } else {
            // SAFETY: forwarded with the caller's layout.
            unsafe { GuestAllocator.alloc(layout) }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { self.alloc(layout) };
        if !pointer.is_null() {
            // SAFETY: alloc returned layout.size writable bytes.
            unsafe { pointer.write_bytes(0, layout.size()) };
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if PATCH_HEAP.contains(pointer) {
            // Installation allocations remain valid for the process lifetime.
        } else {
            // SAFETY: the pointer did not come from PATCH_HEAP.
            unsafe { GuestAllocator.dealloc(pointer, layout) };
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        if !PATCH_HEAP.contains(pointer) {
            // SAFETY: the pointer did not come from PATCH_HEAP.
            return unsafe { GuestAllocator.realloc(pointer, old, new_size) };
        }
        let Ok(new_layout) = Layout::from_size_align(new_size, old.align()) else {
            return ptr::null_mut();
        };
        let replacement = PATCH_HEAP.allocate(new_layout);
        if !replacement.is_null() {
            // SAFETY: the replacement is valid and does not overlap the source.
            unsafe {
                ptr::copy_nonoverlapping(pointer, replacement, old.size().min(new_size));
            }
        }
        replacement
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::MutexGuard;

    use reverie_inguest::guest::alloc::enter_dispatch;
    use reverie_inguest::guest::alloc::tool_heap_contains;

    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_guard() -> MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[test]
    fn installation_allocations_stay_in_the_installation_heap() {
        let _test_guard = test_guard();
        let allocator = PatchAllocator;
        let layout = Layout::from_size_align(64, 16).unwrap();
        let pointer = {
            let _scope = enter();
            // SAFETY: layout is valid for this allocator.
            unsafe { allocator.alloc(layout) }
        };
        assert!(PATCH_HEAP.contains(pointer));
        // SAFETY: pointer covers layout.size writable bytes.
        unsafe { pointer.write_bytes(0x5a, layout.size()) };

        // After the scope ends, a realloc still copies within the heap.
        // SAFETY: pointer is live and was allocated with layout.
        let grown = unsafe { allocator.realloc(pointer, layout, 128) };
        assert!(PATCH_HEAP.contains(grown));
        for index in 0..layout.size() {
            // SAFETY: grown holds at least the copied bytes.
            assert_eq!(unsafe { grown.add(index).read() }, 0x5a);
        }
        // Installation memory is never freed, so the source is still intact.
        // SAFETY: PATCH_HEAP never reclaims its allocations.
        unsafe { allocator.dealloc(pointer, layout) };
        assert_eq!(unsafe { pointer.read() }, 0x5a);
    }

    #[test]
    fn installation_takes_precedence_over_dispatch() {
        let _test_guard = test_guard();
        let allocator = PatchAllocator;
        let layout = Layout::from_size_align(32, 8).unwrap();
        let _dispatch = enter_dispatch();
        let _installation = enter();
        // SAFETY: layout is valid for this allocator.
        let pointer = unsafe { allocator.alloc(layout) };
        assert!(PATCH_HEAP.contains(pointer));
        assert!(!tool_heap_contains(pointer));
    }

    #[test]
    fn dispatch_allocations_go_to_the_tool_heap() {
        let _test_guard = test_guard();
        let allocator = PatchAllocator;
        let layout = Layout::from_size_align(64, 16).unwrap();
        let _scope = enter_dispatch();
        // SAFETY: layout is valid for this allocator.
        let pointer = unsafe { allocator.alloc(layout) };
        assert!(tool_heap_contains(pointer));
        // SAFETY: pointer covers layout.size writable bytes.
        unsafe { pointer.write_bytes(0x3c, layout.size()) };
        // SAFETY: pointer is live and was allocated with layout.
        let grown = unsafe { allocator.realloc(pointer, layout, 256) };
        assert!(tool_heap_contains(grown));
        for index in 0..layout.size() {
            // SAFETY: grown holds at least the copied bytes.
            assert_eq!(unsafe { grown.add(index).read() }, 0x3c);
        }
        // SAFETY: grown is live and was allocated with this layout.
        unsafe { allocator.dealloc(grown, Layout::from_size_align(256, 16).unwrap()) };
    }

    #[test]
    fn other_allocations_go_to_the_system_allocator() {
        let _test_guard = test_guard();
        let allocator = PatchAllocator;
        let layout = Layout::from_size_align(64, 16).unwrap();
        // SAFETY: layout is valid for this allocator.
        let pointer = unsafe { allocator.alloc(layout) };
        assert!(!pointer.is_null());
        assert!(!PATCH_HEAP.contains(pointer) && !tool_heap_contains(pointer));
        // SAFETY: pointer is live and was allocated with layout.
        let grown = unsafe { allocator.realloc(pointer, layout, 128) };
        assert!(!grown.is_null());
        assert!(!PATCH_HEAP.contains(grown) && !tool_heap_contains(grown));
        // SAFETY: grown is live and was allocated with this layout.
        unsafe { allocator.dealloc(grown, Layout::from_size_align(128, 16).unwrap()) };
    }

    #[test]
    fn private_allocations_keep_their_owner_across_scope_changes() {
        let _test_guard = test_guard();
        let allocator = PrivatePatchAllocator;
        let layout = Layout::from_size_align(73, 4096).unwrap();
        // SAFETY: nonzero valid layouts, then only the returned live pointers.
        unsafe {
            let pointer = allocator.alloc_zeroed(layout);
            assert!(!pointer.is_null());
            assert!(tool_heap_contains(pointer));
            assert_eq!(pointer as usize % layout.align(), 0);
            assert_eq!(std::slice::from_raw_parts(pointer, layout.size()), &[0; 73]);
            pointer.write_bytes(0x5a, layout.size());
            let grown = {
                let _dispatch = enter_dispatch();
                let _installation = enter();
                // Pointer ownership wins even while a patch scope is active.
                allocator.realloc(pointer, layout, 8192)
            };
            assert!(!grown.is_null());
            assert!(tool_heap_contains(grown));
            assert_eq!(grown as usize % layout.align(), 0);
            assert_eq!(
                std::slice::from_raw_parts(grown, layout.size()),
                &[0x5a; 73]
            );
            allocator.dealloc(grown, Layout::from_size_align(8192, 4096).unwrap());

            let patch = {
                let _installation = enter();
                allocator.alloc(layout)
            };
            assert!(PATCH_HEAP.contains(patch));
            patch.write_bytes(0x3c, layout.size());
            let patch_grown = allocator.realloc(patch, layout, 128);
            assert!(!patch_grown.is_null());
            assert!(PATCH_HEAP.contains(patch_grown));
            assert_eq!(std::slice::from_raw_parts(patch_grown, 73), &[0x3c; 73]);
            allocator.dealloc(patch_grown, Layout::from_size_align(128, 4096).unwrap());
        }
    }

    #[test]
    fn private_exhaustion_has_no_system_fallback() {
        let _test_guard = test_guard();
        let allocator = PrivatePatchAllocator;
        let huge = Layout::from_size_align(64 * 1024 * 1024 + 4096, 4096).unwrap();
        let small = Layout::from_size_align(64, 16).unwrap();
        // SAFETY: valid layouts and only live pointers are reallocated/freed.
        unsafe {
            assert!(allocator.alloc(huge).is_null());
            assert!(allocator.alloc_zeroed(huge).is_null());
            let pointer = allocator.alloc(small);
            assert!(!pointer.is_null());
            pointer.write_bytes(0xa5, small.size());
            assert!(allocator.realloc(pointer, small, huge.size()).is_null());
            assert_eq!(std::slice::from_raw_parts(pointer, 64), &[0xa5; 64]);
            allocator.dealloc(pointer, small);
            let _installation = enter();
            assert!(allocator.alloc(huge).is_null());
        }
    }
}
