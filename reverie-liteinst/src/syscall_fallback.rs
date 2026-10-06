//! LiteInst's use of the in-guest fallback continuation, which lives in
//! [`reverie_inguest::guest::continuation`]: it registers the runtime's
//! dispatch functions as the continuation's handlers and keeps the two
//! observation symbols the fixtures look up by name.

use std::io;

use liteinst2::trampoline::HookContext;
use reverie_inguest::guest::context::RegisterContext;
use reverie_inguest::guest::continuation;
pub(crate) use reverie_inguest::guest::continuation::complete;
pub(crate) use reverie_inguest::guest::continuation::enable_nested_runtime_access;
pub(crate) use reverie_inguest::guest::continuation::prepare_instruction_signal;
pub(crate) use reverie_inguest::guest::continuation::prepare_signal;
pub(crate) use reverie_inguest::guest::continuation::rebind_fork_child;
use reverie_inguest::guest::event::InstructionEventKind;

/// Registers LiteInst's dispatch functions as the continuation's handlers and
/// prepares this thread's continuation.
pub(crate) fn initialize() -> io::Result<()> {
    // SAFETY: both handlers only pass the continuation's register block on to
    // the runtime's dispatch, which uses it for the call and keeps nothing.
    unsafe {
        continuation::register_handlers(continuation::ContinuationHandlers {
            syscall: dispatch_syscall,
            instruction: dispatch_instruction,
        })
    }?;
    continuation::initialize()
}

// HookContext and RegisterContext have the same layout (checked in
// tool_host.rs), so the continuation's context is passed on by a cast.
unsafe fn dispatch_syscall(context: *mut RegisterContext, pkru: &mut Option<u32>) {
    unsafe { crate::runtime::dispatch_fallback_context(context.cast::<HookContext>(), pkru) };
}

unsafe fn dispatch_instruction(context: *mut RegisterContext, kind: InstructionEventKind) {
    unsafe { crate::runtime::dispatch_fallback_instruction(context.cast::<HookContext>(), kind) };
}

/// Diagnostic observations for the current thread's owned continuation.
/// Selectors 0/1/2 are actual fallback entry, reached ordinary callback, and
/// prepared genuine completion frame. They are not a kernel-stop census.
#[unsafe(no_mangle)]
pub extern "C" fn reverie_liteinst_owned_fallback_observation(selector: u32) -> u64 {
    continuation::owned_observation(selector)
}

/// Check a fixture's actual callback local address against owned stack bounds.
#[unsafe(no_mangle)]
pub extern "C" fn reverie_liteinst_on_owned_fallback_stack(address: usize) -> bool {
    continuation::on_owned_stack(address)
}
