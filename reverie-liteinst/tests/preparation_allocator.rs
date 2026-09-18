#![forbid(unsafe_op_in_unsafe_fn)]
#![allow(dead_code, unused_imports)]

use core::alloc::GlobalAlloc;
use core::alloc::Layout;
use std::alloc::System;

#[path = "../src/patch_alloc.rs"]
mod patch_alloc;

#[global_allocator]
static ALLOCATOR: patch_alloc::PatchAllocator = patch_alloc::PatchAllocator;

fn main() {
    let layout = Layout::from_size_align(64, 32).unwrap();
    let (preparation_before, patch_before, tool_before) = patch_alloc::allocator_high_water();

    // SAFETY: layout is valid for System and both allocations are initialized
    // or retained explicitly below.
    let original = unsafe { System.alloc(layout) };
    assert!(!original.is_null());
    unsafe { original.write_bytes(0x6d, layout.size()) };
    let pre_scope_drop = unsafe { System.alloc(layout) };
    assert!(!pre_scope_drop.is_null());

    let scope = patch_alloc::enter_preparation().unwrap();
    assert!(patch_alloc::enter_preparation().is_none());
    // SAFETY: all layouts are valid. These calls exercise the installed global
    // allocator while the process is single-threaded and signal-free.
    let allocated = unsafe { std::alloc::alloc(layout) };
    let zeroed = unsafe { std::alloc::alloc_zeroed(layout) };
    let migrated = unsafe { std::alloc::realloc(original, layout, 128) };
    assert!(patch_alloc::preparation_owns(allocated));
    assert!(patch_alloc::preparation_owns(zeroed));
    assert!(patch_alloc::preparation_owns(migrated));
    for index in 0..layout.size() {
        assert_eq!(unsafe { zeroed.add(index).read() }, 0);
        assert_eq!(unsafe { migrated.add(index).read() }, 0x6d);
    }
    unsafe { std::alloc::dealloc(pre_scope_drop, layout) };
    let (preparation_baseline, patch_during, tool_during) = patch_alloc::allocator_high_water();
    assert!(preparation_before < preparation_baseline);
    assert!(preparation_baseline - preparation_before <= 4096);
    assert_eq!(patch_during, patch_before);
    assert_eq!(tool_during, tool_before);

    let large_layout = Layout::from_size_align(16 * 1024, 64).unwrap();
    let retained_layout = Layout::from_size_align(128, 64).unwrap();
    let remainder_layout = Layout::from_size_align(15 * 1024, 64).unwrap();
    let large = unsafe { std::alloc::alloc(large_layout) };
    let sentinel = unsafe { std::alloc::alloc(layout) };
    assert!(!large.is_null() && !sentinel.is_null());
    let reserved_high_water = patch_alloc::allocator_high_water().0;
    unsafe { std::alloc::dealloc(large, large_layout) };

    let retained = unsafe { std::alloc::alloc(retained_layout) };
    let reused_remainder = unsafe { std::alloc::alloc(remainder_layout) };
    assert_eq!(retained, large);
    assert!(!reused_remainder.is_null());
    assert_eq!(patch_alloc::allocator_high_water().0, reserved_high_water);
    unsafe {
        retained.write_bytes(0xa5, retained_layout.size());
        reused_remainder.write_bytes(0x5a, remainder_layout.size());
        assert_eq!(retained.read(), 0xa5);
        assert_eq!(
            reused_remainder.add(remainder_layout.size() - 1).read(),
            0x5a
        );
        std::alloc::dealloc(retained, retained_layout);
        std::alloc::dealloc(reused_remainder, remainder_layout);
        std::alloc::dealloc(sentinel, layout);
    }
    let (preparation_during, patch_during, tool_during) = patch_alloc::allocator_high_water();
    assert_eq!(patch_during, patch_before);
    assert_eq!(tool_during, tool_before);
    assert!(preparation_during < patch_alloc::preparation_capacity());
    drop(scope);

    // Escaped preparation allocations remain owned and reclaimable after the
    // scope. The two System sources were deliberately leaked by scoped
    // realloc/dealloc and are cleaned up directly here.
    unsafe {
        std::alloc::dealloc(allocated, layout);
        std::alloc::dealloc(zeroed, layout);
        std::alloc::dealloc(migrated, Layout::from_size_align(128, 32).unwrap());
        System.dealloc(original, layout);
        System.dealloc(pre_scope_drop, layout);
    }

    fn fail_scope() -> Result<(), ()> {
        let _scope = patch_alloc::enter_preparation().ok_or(())?;
        Err(())
    }
    assert_eq!(fail_scope(), Err(()));
    assert!(patch_alloc::enter_preparation().is_some());

    let (_, patch_before, tool_before) = patch_alloc::allocator_high_water();
    let quiescent = patch_alloc::enter_quiescent_install().unwrap();
    assert!(patch_alloc::enter_quiescent_install().is_none());
    let patch_allocation = unsafe { std::alloc::alloc(layout) };
    assert!(patch_alloc::patch_owns(patch_allocation));
    let (_, patch_during, tool_during) = patch_alloc::allocator_high_water();
    assert!(patch_during > patch_before);
    assert_eq!(tool_during, tool_before);
    drop(quiescent);
    unsafe { std::alloc::dealloc(patch_allocation, layout) };
    let (_, patch_after, tool_after) = patch_alloc::allocator_high_water();
    assert_eq!(patch_after, patch_before);
    assert_eq!(tool_after, tool_before);
}
