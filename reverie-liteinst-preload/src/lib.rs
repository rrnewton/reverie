//! Standalone preload ownership. The shared core stays allocator neutral.
#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(target_env = "gnu")]
mod glibc_compat;

#[global_allocator]
static ALLOCATOR: reverie_liteinst::PrivateToolAllocator = reverie_liteinst::PrivateToolAllocator;

#[used]
#[unsafe(link_section = ".init_array")]
static REVERIE_LITEINST_INIT: unsafe extern "C" fn() =
    reverie_liteinst::reverie_liteinst_initialize;
