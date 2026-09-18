//! Allocation reserves for instrumentation planning and tool dispatch.
//!
//! A seccomp SIGSYS handler cannot let the system allocator issue brk or mmap.
//! During a bounded installation scope, allocations from liteinst2 and
//! iced-x86 therefore come from this prepublished process-lifetime buffer.
//! Objects that survive registration are intentionally never reclaimed.
//!
//! Tool callbacks use a separate reusable arena because a callback can interrupt
//! the guest allocator itself. Its temporary and persistent allocations are
//! therefore isolated from libc until they are released or the process exits.

use core::alloc::GlobalAlloc;
use core::alloc::Layout;
use core::cell::UnsafeCell;
use core::mem::align_of;
use core::mem::size_of;
use core::ptr;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering;
use std::alloc::System;
use std::cell::Cell;
use std::io;

use reverie_preload::trap::raw_syscall6;

pub(crate) const PATCH_HEAP_BYTES: usize = 32 * 1024 * 1024;
// One install sees at most a 64-byte instruction snapshot and emits into one
// 4-KiB arena slot. This keeps one MiB of contiguous reusable heap available
// for the bounded iced-x86 scan/plan/encode vectors, error formatting, the
// InstalledHook, and both published program-counter mapping copies. The probe
// is fallible and runs under the process-wide install lock before any of those
// infallible Rust allocations can start.
pub(crate) const PATCH_INSTALL_HEADROOM_BYTES: usize = 1024 * 1024;
const TOOL_HEAP_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const PREPARATION_HEAP_BYTES: usize = 60 * 1024 * 1024;
const FREE_LIST_END: usize = usize::MAX;

#[repr(align(64))]
struct ToolHeapBytes<const BYTES: usize>(UnsafeCell<[u8; BYTES]>);

// SAFETY: the owning ToolHeap serializes every access to these bytes.
unsafe impl<const BYTES: usize> Sync for ToolHeapBytes<BYTES> {}

impl<const BYTES: usize> ToolHeapBytes<BYTES> {
    const fn new() -> Self {
        Self(UnsafeCell::new([0; BYTES]))
    }

    fn base(&self) -> *mut u8 {
        self.0.get().cast::<u8>()
    }
}

#[repr(C)]
struct ToolHeapBlock {
    span: usize,
    next: usize,
}

/// Reusable storage for allocations made while the guest allocator is interrupted.
struct ToolHeap<const BYTES: usize> {
    bytes: &'static ToolHeapBytes<BYTES>,
    next: UnsafeCell<usize>,
    free_head: UnsafeCell<usize>,
    locked: AtomicBool,
}

// SAFETY: every metadata access is serialized by `locked`.
unsafe impl<const BYTES: usize> Sync for ToolHeap<BYTES> {}

impl<const BYTES: usize> ToolHeap<BYTES> {
    const fn new(bytes: &'static ToolHeapBytes<BYTES>) -> Self {
        Self {
            bytes,
            next: UnsafeCell::new(0),
            free_head: UnsafeCell::new(FREE_LIST_END),
            locked: AtomicBool::new(false),
        }
    }

    fn base(&self) -> *mut u8 {
        self.bytes.base()
    }

    fn lock(&self) -> ToolHeapLock<'_, BYTES> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        ToolHeapLock { heap: self }
    }

    fn layout_end(&self, block_offset: usize, layout: Layout) -> Option<(*mut u8, usize)> {
        let base = self.base() as usize;
        let block_address = base.checked_add(block_offset)?;
        let payload_start = block_address
            .checked_add(size_of::<ToolHeapBlock>())?
            .checked_add(size_of::<usize>())?;
        let payload_address = align_up(payload_start, layout.align().max(align_of::<usize>()))?;
        let payload_end = payload_address.checked_add(layout.size().max(1))?;
        Some((payload_address as *mut u8, payload_end - base))
    }

    fn block(&self, offset: usize) -> *mut ToolHeapBlock {
        self.base().wrapping_add(offset).cast()
    }

    fn allocate(&self, layout: Layout) -> *mut u8 {
        let _guard = self.lock();
        let mut previous = FREE_LIST_END;
        // SAFETY: the heap lock serializes free-list access.
        let mut current = unsafe { *self.free_head.get() };

        while current != FREE_LIST_END {
            let block = self.block(current);
            // SAFETY: free-list offsets always point at initialized headers.
            let (span, next) = unsafe { ((*block).span, (*block).next) };
            let fits = self
                .layout_end(current, layout)
                .filter(|(_, end)| *end <= current.saturating_add(span));
            if let Some((pointer, payload_end)) = fits {
                let block_end = current + span;
                let remainder =
                    align_up(payload_end, align_of::<ToolHeapBlock>()).filter(|remainder| {
                        *remainder < block_end
                            && self
                                .layout_end(*remainder, Layout::from_size_align(1, 1).unwrap())
                                .is_some_and(|(_, minimum_end)| minimum_end <= block_end)
                    });
                // SAFETY: the heap lock serializes free-list mutation.
                unsafe {
                    let replacement = if let Some(remainder) = remainder {
                        self.block(remainder).write(ToolHeapBlock {
                            span: block_end - remainder,
                            next,
                        });
                        (*block).span = remainder - current;
                        remainder
                    } else {
                        next
                    };
                    if previous == FREE_LIST_END {
                        *self.free_head.get() = replacement;
                    } else {
                        (*self.block(previous)).next = replacement;
                    }
                    (*block).next = FREE_LIST_END;
                    pointer
                        .sub(size_of::<usize>())
                        .cast::<usize>()
                        .write(current);
                }
                return pointer;
            }
            previous = current;
            current = next;
        }

        // SAFETY: the heap lock serializes bump-cursor access.
        let cursor = unsafe { *self.next.get() };
        let Some(block_offset) = align_up(cursor, align_of::<ToolHeapBlock>()) else {
            return ptr::null_mut();
        };
        let Some((pointer, end)) = self.layout_end(block_offset, layout) else {
            return ptr::null_mut();
        };
        if end > BYTES {
            return ptr::null_mut();
        }

        // SAFETY: this fresh bump range is exclusive and within the heap.
        unsafe {
            self.block(block_offset).write(ToolHeapBlock {
                span: end - block_offset,
                next: FREE_LIST_END,
            });
            pointer
                .sub(size_of::<usize>())
                .cast::<usize>()
                .write(block_offset);
            *self.next.get() = end;
        }
        pointer
    }

    unsafe fn deallocate(&self, pointer: *mut u8) {
        // SAFETY: each tool-heap allocation records its block offset here.
        let block_offset = unsafe { pointer.sub(size_of::<usize>()).cast::<usize>().read() };
        let _guard = self.lock();
        // SAFETY: the block header remains reserved until this allocation is
        // freed. The sorted free list permits exact adjacent-block coalescing.
        unsafe {
            let mut before_previous = FREE_LIST_END;
            let mut previous = FREE_LIST_END;
            let mut current = *self.free_head.get();
            while current != FREE_LIST_END && current < block_offset {
                before_previous = previous;
                previous = current;
                current = (*self.block(current)).next;
            }

            let (merged, merged_predecessor) = if previous != FREE_LIST_END
                && align_up(
                    previous + (*self.block(previous)).span,
                    align_of::<ToolHeapBlock>(),
                ) == Some(block_offset)
            {
                (*self.block(previous)).span =
                    block_offset + (*self.block(block_offset)).span - previous;
                (previous, before_previous)
            } else {
                (*self.block(block_offset)).next = current;
                if previous == FREE_LIST_END {
                    *self.free_head.get() = block_offset;
                } else {
                    (*self.block(previous)).next = block_offset;
                }
                (block_offset, previous)
            };

            let next = (*self.block(merged)).next;
            if next != FREE_LIST_END
                && align_up(
                    merged + (*self.block(merged)).span,
                    align_of::<ToolHeapBlock>(),
                ) == Some(next)
            {
                (*self.block(merged)).span = next + (*self.block(next)).span - merged;
                (*self.block(merged)).next = (*self.block(next)).next;
            }

            if merged + (*self.block(merged)).span == *self.next.get() {
                let after = (*self.block(merged)).next;
                if merged_predecessor == FREE_LIST_END {
                    *self.free_head.get() = after;
                } else {
                    (*self.block(merged_predecessor)).next = after;
                }
                *self.next.get() = merged;
            }
        }
    }

    fn contains(&self, pointer: *mut u8) -> bool {
        let base = self.base() as usize;
        (base..base + BYTES).contains(&(pointer as usize))
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn high_water(&self) -> usize {
        let _guard = self.lock();
        // SAFETY: the heap lock serializes cursor access.
        unsafe { *self.next.get() }
    }
}

struct ToolHeapLock<'a, const BYTES: usize> {
    heap: &'a ToolHeap<BYTES>,
}

thread_local! {
    // Const, no-drop TLS is important here: the global allocator consults
    // these counters before it can choose a safe backing heap.
    static INSTALLATION_DEPTH: Cell<usize> = const { Cell::new(0) };
    static DISPATCH_DEPTH: Cell<usize> = const { Cell::new(0) };
}

static PREPARATION_ACTIVE: AtomicBool = AtomicBool::new(false);
static QUIESCENT_INSTALL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Allocation scope for globally quiescent one-time runtime preparation.
///
/// Entry does not touch dynamically allocated TLS. The explicit after-loader
/// path stops every other task, and the legacy constructor path enters before
/// application threads exist, so a process-wide flag is exact for this phase.
/// Normal target returns unwind this RAII scope. A controller-side refusal is
/// safe only because the after-loader caller treats every nonlocal call abort
/// as terminal and never resumes the stopped target with this flag still set.
pub(crate) struct PreparationAllocationScope;

impl Drop for PreparationAllocationScope {
    fn drop(&mut self) {
        PREPARATION_ACTIVE.store(false, Ordering::Release);
    }
}

pub(crate) fn enter_preparation() -> Option<PreparationAllocationScope> {
    PREPARATION_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| PreparationAllocationScope)
}

fn preparation_active() -> bool {
    PREPARATION_ACTIVE.load(Ordering::Acquire)
}

/// Allocation scope for one explicit stopped-task site installation.
///
/// This selects the process-lifetime patch heap without first touching the
/// DSO's dynamic TLS. It is valid only while the controller keeps every other
/// task and every signal handler stopped. As with preparation, a nonlocal
/// controller abort must remain terminal rather than resume past this guard.
pub(crate) struct QuiescentPatchAllocationScope;

impl Drop for QuiescentPatchAllocationScope {
    fn drop(&mut self) {
        QUIESCENT_INSTALL_ACTIVE.store(false, Ordering::Release);
    }
}

pub(crate) fn enter_quiescent_install() -> Option<QuiescentPatchAllocationScope> {
    QUIESCENT_INSTALL_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| QuiescentPatchAllocationScope)
}

fn quiescent_install_active() -> bool {
    QUIESCENT_INSTALL_ACTIVE.load(Ordering::Acquire)
}

pub(crate) struct PatchAllocationScope;

impl Drop for PatchAllocationScope {
    fn drop(&mut self) {
        // Every concurrent installation begins inside the SIGSYS or SIGSEGV
        // handler. Leave the live mask blocked until that handler returns: the
        // kernel's rt_sigreturn restores the exact mask saved in its signal
        // frame. Clearing allocator routing first ensures a pending user
        // handler cannot run inside this Drop and siglongjmp past the decrement.
        INSTALLATION_DEPTH.set(INSTALLATION_DEPTH.get() - 1);
    }
}

/// Block every blockable signal for one concurrent install in a kernel-created
/// signal handler. Drop clears allocator routing but intentionally leaves the
/// live mask blocked; the enclosing `rt_sigreturn` restores the exact saved
/// signal-frame mask after all Rust guards are gone.
///
/// # Safety
///
/// The caller must be executing beneath a real signal frame that will return
/// through `rt_sigreturn`, or terminate the process without resuming the guest.
/// An ordinary caller would otherwise leave its blockable signals masked.
pub(crate) unsafe fn enter_signal_handler() -> io::Result<PatchAllocationScope> {
    let blocked = u64::MAX;
    let result = unsafe {
        raw_syscall6(
            libc::SYS_rt_sigprocmask,
            [
                libc::SIG_BLOCK as u64,
                (&raw const blocked) as u64,
                0,
                size_of::<u64>() as u64,
                0,
                0,
            ],
        )
    };
    if result < 0 {
        return Err(io::Error::from_raw_os_error((-result) as i32));
    }
    INSTALLATION_DEPTH.set(INSTALLATION_DEPTH.get() + 1);
    Ok(PatchAllocationScope)
}

fn installation_active() -> bool {
    INSTALLATION_DEPTH.get() != 0
}

// TODO-HUMAN-REVIEW(PR-148): Review the dispatch allocator scope API.
pub(crate) struct DispatchAllocationScope;

impl Drop for DispatchAllocationScope {
    fn drop(&mut self) {
        DISPATCH_DEPTH.set(DISPATCH_DEPTH.get() - 1);
    }
}

// TODO-HUMAN-REVIEW(PR-148): Review signal-context tool allocation isolation.
pub(crate) fn enter_dispatch() -> DispatchAllocationScope {
    DISPATCH_DEPTH.set(DISPATCH_DEPTH.get() + 1);
    DispatchAllocationScope
}

fn dispatch_active() -> bool {
    DISPATCH_DEPTH.get() != 0
}

pub(crate) struct PatchAllocator;

// SAFETY: normal allocations delegate to System. Installation and dispatch
// allocations use separate serialized reusable storage independent of the
// interrupted guest allocator. Install objects that escape publication remain
// allocated; temporary scanner/planner objects return to the same free list.
unsafe impl GlobalAlloc for PatchAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if preparation_active() {
            PREPARATION_HEAP.allocate(layout)
        } else if quiescent_install_active() {
            PATCH_HEAP.allocate(layout)
        } else if installation_active() {
            PATCH_HEAP.allocate(layout)
        } else if dispatch_active() {
            TOOL_HEAP.allocate(layout)
        } else {
            // SAFETY: forwarded to the process system allocator.
            unsafe { System.alloc(layout) }
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
        if PREPARATION_HEAP.contains(pointer) {
            // SAFETY: the pointer was allocated by PREPARATION_HEAP.
            unsafe { PREPARATION_HEAP.deallocate(pointer) };
        } else if TOOL_HEAP.contains(pointer) {
            // SAFETY: the pointer was allocated by TOOL_HEAP.
            unsafe { TOOL_HEAP.deallocate(pointer) };
        } else if PATCH_HEAP.contains(pointer) {
            // SAFETY: the pointer was allocated by PATCH_HEAP. Published
            // process-lifetime objects do not call dealloc; temporary install
            // state does and becomes reusable here.
            unsafe { PATCH_HEAP.deallocate(pointer) };
        } else if preparation_active()
            || quiescent_install_active()
            || installation_active()
            || dispatch_active()
        {
            // A bounded scope may drop state allocated before it began. Leaking
            // that old block is preferable to reentering the system allocator.
        } else {
            // SAFETY: non-patch pointers came from System with this layout.
            unsafe { System.dealloc(pointer, layout) };
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        if PREPARATION_HEAP.contains(pointer) {
            let Ok(new_layout) = Layout::from_size_align(new_size, old.align()) else {
                return ptr::null_mut();
            };
            let replacement = PREPARATION_HEAP.allocate(new_layout);
            if !replacement.is_null() {
                // SAFETY: both allocations are valid and non-overlapping.
                unsafe {
                    ptr::copy_nonoverlapping(pointer, replacement, old.size().min(new_size));
                    PREPARATION_HEAP.deallocate(pointer);
                }
            }
            return replacement;
        }
        if TOOL_HEAP.contains(pointer) {
            let Ok(new_layout) = Layout::from_size_align(new_size, old.align()) else {
                return ptr::null_mut();
            };
            let replacement = TOOL_HEAP.allocate(new_layout);
            if !replacement.is_null() {
                // SAFETY: both allocations are valid and non-overlapping.
                unsafe {
                    ptr::copy_nonoverlapping(pointer, replacement, old.size().min(new_size));
                    TOOL_HEAP.deallocate(pointer);
                }
            }
            return replacement;
        }
        if PATCH_HEAP.contains(pointer) {
            let Ok(new_layout) = Layout::from_size_align(new_size, old.align()) else {
                return ptr::null_mut();
            };
            let replacement = PATCH_HEAP.allocate(new_layout);
            if !replacement.is_null() {
                // SAFETY: both allocations are valid and non-overlapping.
                unsafe {
                    ptr::copy_nonoverlapping(pointer, replacement, old.size().min(new_size));
                    PATCH_HEAP.deallocate(pointer);
                }
            }
            return replacement;
        }

        // Check the process-wide quiescent scope before either TLS counter.
        let preparation = preparation_active();
        let quiescent_install = !preparation && quiescent_install_active();
        let installation = !preparation && !quiescent_install && installation_active();
        let dispatch = !preparation && !quiescent_install && !installation && dispatch_active();
        if !preparation && !quiescent_install && !installation && !dispatch {
            // SAFETY: non-patch pointers came from System with this layout.
            return unsafe { System.realloc(pointer, old, new_size) };
        }
        let Ok(new_layout) = Layout::from_size_align(new_size, old.align()) else {
            return ptr::null_mut();
        };
        let replacement = if preparation {
            PREPARATION_HEAP.allocate(new_layout)
        } else if quiescent_install || installation {
            PATCH_HEAP.allocate(new_layout)
        } else {
            TOOL_HEAP.allocate(new_layout)
        };
        if !replacement.is_null() {
            // SAFETY: the replacement is valid and does not overlap the source.
            unsafe {
                ptr::copy_nonoverlapping(pointer, replacement, old.size().min(new_size));
            }
        }
        replacement
    }
}

impl<const BYTES: usize> Drop for ToolHeapLock<'_, BYTES> {
    fn drop(&mut self) {
        self.heap.locked.store(false, Ordering::Release);
    }
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment - 1)
        .map(|address| address & !(alignment - 1))
}

static TOOL_HEAP_STORAGE: ToolHeapBytes<TOOL_HEAP_BYTES> = ToolHeapBytes::new();
static PATCH_HEAP_STORAGE: ToolHeapBytes<PATCH_HEAP_BYTES> = ToolHeapBytes::new();
static PREPARATION_HEAP_STORAGE: ToolHeapBytes<PREPARATION_HEAP_BYTES> = ToolHeapBytes::new();
static TOOL_HEAP: ToolHeap<TOOL_HEAP_BYTES> = ToolHeap::new(&TOOL_HEAP_STORAGE);
static PATCH_HEAP: ToolHeap<PATCH_HEAP_BYTES> = ToolHeap::new(&PATCH_HEAP_STORAGE);
static PREPARATION_HEAP: ToolHeap<PREPARATION_HEAP_BYTES> =
    ToolHeap::new(&PREPARATION_HEAP_STORAGE);

pub(crate) fn patch_install_capacity_available() -> bool {
    let layout = Layout::from_size_align(PATCH_INSTALL_HEADROOM_BYTES, 64)
        .expect("fixed patch-install headroom layout is valid");
    let reservation = PATCH_HEAP.allocate(layout);
    if reservation.is_null() {
        return false;
    }
    // SAFETY: reservation is live and came directly from PATCH_HEAP.
    unsafe { PATCH_HEAP.deallocate(reservation) };
    true
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn preparation_owns(pointer: *mut u8) -> bool {
    PREPARATION_HEAP.contains(pointer)
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn patch_owns(pointer: *mut u8) -> bool {
    PATCH_HEAP.contains(pointer)
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn allocator_high_water() -> (usize, usize, usize) {
    (
        PREPARATION_HEAP.high_water(),
        PATCH_HEAP.high_water(),
        TOOL_HEAP.high_water(),
    )
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn preparation_capacity() -> usize {
    PREPARATION_HEAP_BYTES
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicBool;
    use core::sync::atomic::AtomicU8;
    use std::sync::Mutex;
    use std::sync::MutexGuard;

    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static SPLIT_STORAGE: ToolHeapBytes<16_384> = ToolHeapBytes::new();
    static SPLIT_HEAP: ToolHeap<16_384> = ToolHeap::new(&SPLIT_STORAGE);
    static COALESCE_STORAGE: ToolHeapBytes<4096> = ToolHeapBytes::new();
    static COALESCE_HEAP: ToolHeap<4096> = ToolHeap::new(&COALESCE_STORAGE);
    static REWIND_STORAGE: ToolHeapBytes<4096> = ToolHeapBytes::new();
    static REWIND_HEAP: ToolHeap<4096> = ToolHeap::new(&REWIND_STORAGE);
    static ALIGNED_SPLIT_STORAGE: ToolHeapBytes<16_384> = ToolHeapBytes::new();
    static ALIGNED_SPLIT_HEAP: ToolHeap<16_384> = ToolHeap::new(&ALIGNED_SPLIT_STORAGE);
    static PENDING_SIGNAL_CALLED: AtomicBool = AtomicBool::new(false);
    static PENDING_SIGNAL_USED_PATCH_HEAP: AtomicBool = AtomicBool::new(false);
    static PENDING_SIGNAL_SITE_STATE: AtomicU8 = AtomicU8::new(0);
    static PENDING_SIGNAL_OBSERVED_SITE_STATE: AtomicU8 = AtomicU8::new(0);
    static PENDING_SIGNAL_NATIVE_RESTORED: AtomicBool = AtomicBool::new(false);
    static PENDING_SIGNAL_OBSERVED_NATIVE_RESTORED: AtomicBool = AtomicBool::new(false);

    extern "C" fn pending_install_signal_handler(_signal: libc::c_int) {
        PENDING_SIGNAL_CALLED.store(true, Ordering::Release);
        PENDING_SIGNAL_OBSERVED_SITE_STATE.store(
            PENDING_SIGNAL_SITE_STATE.load(Ordering::Acquire),
            Ordering::Release,
        );
        PENDING_SIGNAL_OBSERVED_NATIVE_RESTORED.store(
            PENDING_SIGNAL_NATIVE_RESTORED.load(Ordering::Acquire),
            Ordering::Release,
        );
        let layout = unsafe { Layout::from_size_align_unchecked(64, 16) };
        let allocator = PatchAllocator;
        let pointer = unsafe { allocator.alloc(layout) };
        PENDING_SIGNAL_USED_PATCH_HEAP.store(
            !pointer.is_null() && PATCH_HEAP.contains(pointer),
            Ordering::Release,
        );
        if !pointer.is_null() {
            unsafe { allocator.dealloc(pointer, layout) };
        }
    }

    fn test_guard() -> MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[test]
    fn reusable_heap_splits_large_free_blocks_for_small_persistent_allocations() {
        let _test_guard = test_guard();
        let large = Layout::from_size_align(4096, 8).unwrap();
        let small = Layout::from_size_align(16, 8).unwrap();
        let remainder = Layout::from_size_align(3500, 8).unwrap();

        let original = SPLIT_HEAP.allocate(large);
        let sentinel = SPLIT_HEAP.allocate(small);
        assert!(!original.is_null() && !sentinel.is_null());
        let high_water = SPLIT_HEAP.high_water();
        unsafe { SPLIT_HEAP.deallocate(original) };

        let persistent = SPLIT_HEAP.allocate(small);
        let reused_remainder = SPLIT_HEAP.allocate(remainder);
        assert!(!persistent.is_null());
        assert!(!reused_remainder.is_null());
        assert_eq!(SPLIT_HEAP.high_water(), high_water);
        unsafe {
            SPLIT_HEAP.deallocate(persistent);
            SPLIT_HEAP.deallocate(reused_remainder);
            SPLIT_HEAP.deallocate(sentinel);
        }
    }

    #[test]
    fn reusable_heap_coalesces_adjacent_free_blocks() {
        let _test_guard = test_guard();
        let piece = Layout::from_size_align(512, 8).unwrap();
        let first = COALESCE_HEAP.allocate(piece);
        let second = COALESCE_HEAP.allocate(piece);
        let third = COALESCE_HEAP.allocate(piece);
        let sentinel = COALESCE_HEAP.allocate(piece);
        assert!(!first.is_null() && !second.is_null() && !third.is_null() && !sentinel.is_null());
        let high_water = COALESCE_HEAP.high_water();
        unsafe {
            COALESCE_HEAP.deallocate(second);
            COALESCE_HEAP.deallocate(first);
            COALESCE_HEAP.deallocate(third);
        }

        let joined = COALESCE_HEAP.allocate(Layout::from_size_align(1500, 8).unwrap());
        assert!(!joined.is_null());
        assert_eq!(COALESCE_HEAP.high_water(), high_water);
        unsafe {
            COALESCE_HEAP.deallocate(joined);
            COALESCE_HEAP.deallocate(sentinel);
        }
        assert_eq!(COALESCE_HEAP.high_water(), 0);

        let first = COALESCE_HEAP.allocate(piece);
        let second = COALESCE_HEAP.allocate(piece);
        let third = COALESCE_HEAP.allocate(piece);
        let sentinel = COALESCE_HEAP.allocate(piece);
        let high_water = COALESCE_HEAP.high_water();
        unsafe {
            COALESCE_HEAP.deallocate(first);
            COALESCE_HEAP.deallocate(third);
            COALESCE_HEAP.deallocate(second);
        }
        let joined = COALESCE_HEAP.allocate(Layout::from_size_align(1500, 8).unwrap());
        assert!(!joined.is_null());
        assert_eq!(COALESCE_HEAP.high_water(), high_water);
        unsafe {
            COALESCE_HEAP.deallocate(joined);
            COALESCE_HEAP.deallocate(sentinel);
        }
        assert_eq!(COALESCE_HEAP.high_water(), 0);
        assert!(
            COALESCE_HEAP
                .allocate(Layout::from_size_align(4097, 1).unwrap())
                .is_null()
        );
    }

    #[test]
    fn reusable_heap_rewinds_a_fully_freed_bump_prefix() {
        let _test_guard = test_guard();
        let piece = Layout::from_size_align(512, 8).unwrap();
        let first = REWIND_HEAP.allocate(piece);
        let second = REWIND_HEAP.allocate(piece);
        let third = REWIND_HEAP.allocate(piece);
        assert!(!first.is_null() && !second.is_null() && !third.is_null());
        unsafe {
            REWIND_HEAP.deallocate(second);
            REWIND_HEAP.deallocate(first);
            REWIND_HEAP.deallocate(third);
        }
        assert_eq!(REWIND_HEAP.high_water(), 0);

        let large = Layout::from_size_align(3000, 8).unwrap();
        let joined = REWIND_HEAP.allocate(large);
        assert_eq!(joined, first);
        unsafe { REWIND_HEAP.deallocate(joined) };
        assert_eq!(REWIND_HEAP.high_water(), 0);

        let odd = [509, 513, 517].map(|size| Layout::from_size_align(size, 8).unwrap());
        let odd_first = REWIND_HEAP.allocate(odd[0]);
        let odd_second = REWIND_HEAP.allocate(odd[1]);
        let odd_third = REWIND_HEAP.allocate(odd[2]);
        assert!(!odd_first.is_null() && !odd_second.is_null() && !odd_third.is_null());
        unsafe {
            REWIND_HEAP.deallocate(odd_second);
            REWIND_HEAP.deallocate(odd_first);
            REWIND_HEAP.deallocate(odd_third);
        }
        assert_eq!(REWIND_HEAP.high_water(), 0);
        let joined_across_padding = REWIND_HEAP.allocate(large);
        assert_eq!(joined_across_padding, odd_first);
        unsafe { REWIND_HEAP.deallocate(joined_across_padding) };
    }

    #[test]
    fn reusable_heap_splits_and_reuses_highly_aligned_blocks() {
        let _test_guard = test_guard();
        let large = Layout::from_size_align(4096, 4096).unwrap();
        let small = Layout::from_size_align(16, 4096).unwrap();
        let remainder = Layout::from_size_align(3000, 8).unwrap();
        let sentinel_layout = Layout::from_size_align(64, 8).unwrap();

        let original = ALIGNED_SPLIT_HEAP.allocate(large);
        let sentinel = ALIGNED_SPLIT_HEAP.allocate(sentinel_layout);
        assert!(!original.is_null() && !sentinel.is_null());
        assert_eq!((original as usize) % 4096, 0);
        let high_water = ALIGNED_SPLIT_HEAP.high_water();
        unsafe { ALIGNED_SPLIT_HEAP.deallocate(original) };

        let retained = ALIGNED_SPLIT_HEAP.allocate(small);
        let reused_remainder = ALIGNED_SPLIT_HEAP.allocate(remainder);
        assert_eq!(retained, original);
        assert!(!reused_remainder.is_null());
        assert_eq!(ALIGNED_SPLIT_HEAP.high_water(), high_water);
        unsafe {
            ALIGNED_SPLIT_HEAP.deallocate(retained);
            ALIGNED_SPLIT_HEAP.deallocate(reused_remainder);
            ALIGNED_SPLIT_HEAP.deallocate(sentinel);
        }
        assert_eq!(ALIGNED_SPLIT_HEAP.high_water(), 0);
    }

    #[test]
    fn patch_heap_reclaims_more_than_one_arena_of_install_scratch() {
        let _test_guard = test_guard();
        let allocator = PatchAllocator;
        let temporary = Layout::from_size_align(64 * 1024, 64).unwrap();
        let persistent = Layout::from_size_align(256, 64).unwrap();
        let before = allocator_high_water();
        let _scope = enter_quiescent_install().unwrap();

        let retained = unsafe { allocator.alloc(persistent) };
        assert!(!retained.is_null() && PATCH_HEAP.contains(retained));
        let retained_high_water = PATCH_HEAP.high_water();
        for iteration in 0..256 {
            let scratch = unsafe { allocator.alloc(temporary) };
            assert!(!scratch.is_null());
            unsafe { scratch.write_bytes(iteration as u8, temporary.size()) };
            let grown = unsafe { allocator.realloc(scratch, temporary, 96 * 1024) };
            assert!(!grown.is_null());
            assert_eq!(unsafe { grown.read() }, iteration as u8);
            unsafe {
                allocator.dealloc(
                    grown,
                    Layout::from_size_align(96 * 1024, temporary.align()).unwrap(),
                )
            };
            assert_eq!(PATCH_HEAP.high_water(), retained_high_water);
            assert!(patch_install_capacity_available());
        }
        unsafe { allocator.dealloc(retained, persistent) };
        assert_eq!(allocator_high_water(), before);
    }

    #[test]
    fn patch_headroom_probe_fails_closed_before_fragmented_heap_exhaustion() {
        let _test_guard = test_guard();
        let allocator = PatchAllocator;
        let persistent = Layout::from_size_align(4096, 64).unwrap();
        let before = allocator_high_water();
        let _scope = enter_quiescent_install().unwrap();
        let mut retained = [core::ptr::null_mut(); 8192];
        let mut count = 0;
        while count < retained.len() && patch_install_capacity_available() {
            let pointer = unsafe { allocator.alloc(persistent) };
            assert!(!pointer.is_null());
            retained[count] = pointer;
            count += 1;
        }
        assert!(count < retained.len());
        assert!(!patch_install_capacity_available());

        // Keep the highest block live as a sentinel so freeing alternating
        // lower blocks cannot coalesce with the unused bump tail.
        for index in (0..count.saturating_sub(1)).step_by(2) {
            unsafe { allocator.dealloc(retained[index], persistent) };
            retained[index] = core::ptr::null_mut();
        }
        assert!(
            !patch_install_capacity_available(),
            "small fragmented holes must not satisfy contiguous headroom"
        );

        for pointer in retained
            .into_iter()
            .rev()
            .filter(|pointer| !pointer.is_null())
        {
            unsafe { allocator.dealloc(pointer, persistent) };
        }
        assert!(patch_install_capacity_available());
        assert_eq!(allocator_high_water(), before);
    }

    #[test]
    fn pending_signal_waits_for_sigreturn_after_patch_scope_cleanup() {
        let _test_guard = test_guard();
        PENDING_SIGNAL_CALLED.store(false, Ordering::Release);
        PENDING_SIGNAL_USED_PATCH_HEAP.store(false, Ordering::Release);
        PENDING_SIGNAL_SITE_STATE.store(0, Ordering::Release);
        PENDING_SIGNAL_OBSERVED_SITE_STATE.store(0, Ordering::Release);
        PENDING_SIGNAL_NATIVE_RESTORED.store(false, Ordering::Release);
        PENDING_SIGNAL_OBSERVED_NATIVE_RESTORED.store(false, Ordering::Release);
        let signal = libc::SIGUSR2;

        let mut action = unsafe { core::mem::zeroed::<libc::sigaction>() };
        action.sa_sigaction = pending_install_signal_handler as *const () as usize;
        assert_eq!(unsafe { libc::sigemptyset(&mut action.sa_mask) }, 0);
        let mut old_action = unsafe { core::mem::zeroed::<libc::sigaction>() };
        assert_eq!(
            unsafe { libc::sigaction(signal, &action, &mut old_action) },
            0
        );

        let signal_bit = 1_u64 << (signal - 1);
        let mut original_mask = 0_u64;
        let unblocked = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_UNBLOCK as u64,
                    (&raw const signal_bit) as u64,
                    (&raw mut original_mask) as u64,
                    size_of::<u64>() as u64,
                    0,
                    0,
                ],
            )
        };
        assert_eq!(unblocked, 0);

        let scope = unsafe { enter_signal_handler() }.unwrap();
        PENDING_SIGNAL_SITE_STATE.store(1, Ordering::Release);
        let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
        let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
        assert!(pid > 0 && tid > 0);
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_tgkill,
                    [pid as u64, tid as u64, signal as u64, 0, 0, 0],
                )
            },
            0
        );
        assert!(!PENDING_SIGNAL_CALLED.load(Ordering::Acquire));
        // Model install_site_hook's required ordering: terminal state is
        // published while the mask scope is still live. Drop clears allocator
        // routing but deliberately leaves the pending handler blocked until
        // the enclosing signal handler reaches rt_sigreturn.
        PENDING_SIGNAL_SITE_STATE.store(3, Ordering::Release);
        PENDING_SIGNAL_NATIVE_RESTORED.store(true, Ordering::Release);
        drop(scope);
        assert!(!PENDING_SIGNAL_CALLED.load(Ordering::Acquire));
        assert!(!installation_active());

        // Emulate the enclosing rt_sigreturn restoring the signal-entry mask.
        // SIGUSR2 was made unblocked immediately before entering the synthetic
        // handler, so clearing just that bit from the saved test mask is exact.
        let signal_entry_mask = original_mask & !signal_bit;
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        (&raw const signal_entry_mask) as u64,
                        0,
                        size_of::<u64>() as u64,
                        0,
                        0,
                    ],
                )
            },
            0
        );
        assert!(PENDING_SIGNAL_CALLED.load(Ordering::Acquire));
        assert!(!PENDING_SIGNAL_USED_PATCH_HEAP.load(Ordering::Acquire));
        assert_eq!(
            PENDING_SIGNAL_OBSERVED_SITE_STATE.load(Ordering::Acquire),
            3
        );
        assert!(PENDING_SIGNAL_OBSERVED_NATIVE_RESTORED.load(Ordering::Acquire));
        let mut observed_mask = 0_u64;
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        0,
                        (&raw mut observed_mask) as u64,
                        size_of::<u64>() as u64,
                        0,
                        0,
                    ],
                )
            },
            0
        );
        assert_eq!(observed_mask, signal_entry_mask);
        assert_eq!(
            unsafe { libc::sigaction(signal, &old_action, core::ptr::null_mut()) },
            0
        );
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        (&raw const original_mask) as u64,
                        0,
                        size_of::<u64>() as u64,
                        0,
                        0,
                    ],
                )
            },
            0
        );
    }

    #[test]
    fn dispatch_heap_reuses_freed_blocks() {
        let _test_guard = test_guard();
        let allocator = PatchAllocator;
        let layout = Layout::from_size_align(256, 64).unwrap();
        let _scope = enter_dispatch();

        // SAFETY: allocations and deallocations use the same allocator and layout.
        let first = unsafe { allocator.alloc(layout) };
        assert!(!first.is_null());
        // SAFETY: first is live and was allocated with layout.
        unsafe { allocator.dealloc(first, layout) };

        // SAFETY: layout is valid for this allocator.
        let second = unsafe { allocator.alloc(layout) };
        assert_eq!(second, first);
        // SAFETY: second is live and was allocated with layout.
        unsafe { allocator.dealloc(second, layout) };
    }

    #[test]
    fn dispatch_heap_honors_large_alignment() {
        let _test_guard = test_guard();
        let allocator = PatchAllocator;
        let _scope = enter_dispatch();
        for alignment in [8, 64, 4096] {
            let layout = Layout::from_size_align(257, alignment).unwrap();
            // SAFETY: layout is valid for this allocator.
            let pointer = unsafe { allocator.alloc(layout) };
            assert!(!pointer.is_null());
            assert_eq!((pointer as usize) % alignment, 0);
            // SAFETY: pointer is live and was allocated with layout.
            unsafe { allocator.dealloc(pointer, layout) };
        }
    }

    #[test]
    fn dispatch_heap_realloc_preserves_bytes_when_growing_and_shrinking() {
        let _test_guard = test_guard();
        let allocator = PatchAllocator;
        let small = Layout::from_size_align(64, 32).unwrap();
        let _scope = enter_dispatch();
        let high_water_before = allocator_high_water();
        // SAFETY: small is valid for this allocator.
        let pointer = unsafe { allocator.alloc(small) };
        assert!(!pointer.is_null());
        for index in 0..small.size() {
            // SAFETY: index is within the live small allocation.
            unsafe { pointer.add(index).write(index as u8) };
        }

        // SAFETY: pointer is live and was allocated with small.
        let grown = unsafe { allocator.realloc(pointer, small, 512) };
        assert!(!grown.is_null());
        for index in 0..small.size() {
            // SAFETY: index is within the live grown allocation.
            assert_eq!(unsafe { grown.add(index).read() }, index as u8);
        }
        let grown_layout = Layout::from_size_align(512, small.align()).unwrap();
        // SAFETY: grown is live and was allocated with grown_layout.
        let shrunk = unsafe { allocator.realloc(grown, grown_layout, 16) };
        assert!(!shrunk.is_null());
        for index in 0..16 {
            // SAFETY: index is within the live shrunk allocation.
            assert_eq!(unsafe { shrunk.add(index).read() }, index as u8);
        }
        let shrunk_layout = Layout::from_size_align(16, small.align()).unwrap();
        // SAFETY: shrunk is live and was allocated with shrunk_layout.
        unsafe { allocator.dealloc(shrunk, shrunk_layout) };
        assert_eq!(allocator_high_water(), high_water_before);
    }

    #[test]
    fn failed_dispatch_realloc_keeps_the_source_live() {
        let _test_guard = test_guard();
        let allocator = PatchAllocator;
        let layout = Layout::from_size_align(64, 16).unwrap();
        let _scope = enter_dispatch();
        // SAFETY: layout is valid for this allocator.
        let pointer = unsafe { allocator.alloc(layout) };
        assert!(!pointer.is_null());
        // SAFETY: pointer covers layout.size writable bytes.
        unsafe { pointer.write_bytes(0xa5, layout.size()) };

        // SAFETY: pointer is live; the requested size exceeds the bounded arena.
        let failed = unsafe { allocator.realloc(pointer, layout, TOOL_HEAP_BYTES + 1) };
        assert!(failed.is_null());
        for index in 0..layout.size() {
            // SAFETY: failed realloc leaves the original allocation live.
            assert_eq!(unsafe { pointer.add(index).read() }, 0xa5);
        }
        // SAFETY: pointer remains live with its original layout.
        unsafe { allocator.dealloc(pointer, layout) };
    }

    #[test]
    fn system_realloc_migrates_into_the_dispatch_heap() {
        let _test_guard = test_guard();
        let allocator = PatchAllocator;
        let layout = Layout::from_size_align(64, 16).unwrap();
        // SAFETY: layout is valid for System.
        let original = unsafe { System.alloc(layout) };
        assert!(!original.is_null());
        // SAFETY: original covers layout.size writable bytes.
        unsafe { original.write_bytes(0x3c, layout.size()) };

        let scope = enter_dispatch();
        // SAFETY: original is live and was allocated with layout.
        let migrated = unsafe { allocator.realloc(original, layout, 128) };
        assert!(!migrated.is_null());
        assert!(TOOL_HEAP.contains(migrated));
        drop(scope);
        for index in 0..layout.size() {
            // SAFETY: migrated contains at least the copied original bytes.
            assert_eq!(unsafe { migrated.add(index).read() }, 0x3c);
        }
        let migrated_layout = Layout::from_size_align(128, layout.align()).unwrap();
        // SAFETY: migrated remains owned by TOOL_HEAP after dispatch ends.
        unsafe { allocator.dealloc(migrated, migrated_layout) };
    }

    #[test]
    fn dispatch_heap_supports_concurrent_allocate_free() {
        let _test_guard = test_guard();
        let threads: [_; 4] = std::array::from_fn(|thread_index| {
            std::thread::spawn(move || {
                let allocator = PatchAllocator;
                let _scope = enter_dispatch();
                for iteration in 0..1000 {
                    let size = 1 + (thread_index * 17 + iteration) % 1024;
                    let layout = Layout::from_size_align(size, 64).unwrap();
                    // SAFETY: layout is valid for this allocator.
                    let pointer = unsafe { allocator.alloc(layout) };
                    assert!(!pointer.is_null());
                    // SAFETY: pointer covers layout.size writable bytes.
                    unsafe { pointer.write_bytes(thread_index as u8, layout.size()) };
                    // SAFETY: pointer is live and was allocated with layout.
                    unsafe { allocator.dealloc(pointer, layout) };
                }
            })
        });
        for thread in threads {
            thread.join().unwrap();
        }
    }
}
