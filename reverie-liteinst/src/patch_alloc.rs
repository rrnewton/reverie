//! The allocation reserve for instrumentation planning.
//!
//! A seccomp SIGSYS handler cannot let the system allocator issue brk or mmap.
//! During a bounded installation scope, allocations from liteinst2 and
//! iced-x86 therefore come from this prepublished process-lifetime buffer.
//! Objects that survive registration are intentionally never reclaimed. Every
//! other allocation goes to reverie-inguest's [`GuestAllocator`], which keeps
//! Tool callbacks on their own heap.

use core::alloc::GlobalAlloc;
use core::alloc::Layout;
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;
use std::cell::Cell;

pub(crate) use reverie_inguest::guest::alloc::DispatchAllocationScope;
use reverie_inguest::guest::alloc::GuestAllocator;
pub(crate) use reverie_inguest::guest::alloc::enter_dispatch;

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

pub(crate) struct PatchAllocator;

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
}
