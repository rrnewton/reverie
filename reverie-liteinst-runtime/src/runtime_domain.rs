use std::marker::PhantomData;

core::arch::global_asm!(include_str!("runtime_domain.s"));

unsafe extern "C" {
    fn reverie_liteinst_domain_enter();
    fn reverie_liteinst_domain_leave();
    fn reverie_liteinst_domain_depth() -> u64;
    fn reverie_liteinst_installation_enter();
    fn reverie_liteinst_installation_leave();
    fn reverie_liteinst_installation_depth() -> u64;
    fn reverie_liteinst_allocation_enter();
    fn reverie_liteinst_allocation_leave();
    fn reverie_liteinst_allocation_depth() -> u64;
}

pub(crate) struct Entry {
    _thread_bound: PhantomData<*mut ()>,
}

pub(crate) static PRELOAD_HOOKS: reverie_preload::trap::RuntimeEntryHooks =
    reverie_preload::trap::RuntimeEntryHooks {
        enter: reverie_liteinst_domain_enter,
        leave: reverie_liteinst_domain_leave,
    };

impl Entry {
    #[inline]
    pub(crate) fn enter() -> Self {
        unsafe { reverie_liteinst_domain_enter() };
        #[cfg(test)]
        tests::at(tests::ENTRY);
        Self {
            _thread_bound: PhantomData,
        }
    }
}

impl Drop for Entry {
    #[inline]
    fn drop(&mut self) {
        unsafe { reverie_liteinst_domain_leave() };
        #[cfg(test)]
        tests::at(tests::RETURN);
    }
}

pub(crate) fn installation_enter() {
    unsafe { reverie_liteinst_installation_enter() };
}

pub(crate) fn installation_leave() {
    unsafe { reverie_liteinst_installation_leave() };
}

pub(crate) fn installation_active() -> bool {
    unsafe { reverie_liteinst_installation_depth() != 0 }
}

pub(crate) fn allocation_enter() {
    unsafe { reverie_liteinst_allocation_enter() };
}

pub(crate) fn allocation_leave() {
    unsafe { reverie_liteinst_allocation_leave() };
}

pub(crate) fn allocation_active() -> bool {
    unsafe { reverie_liteinst_domain_depth() != 0 || reverie_liteinst_allocation_depth() != 0 }
}

/// # Safety
/// Missing scopes return None. Use a successful answer for origin classification
/// only inside actual clock_enter and PRELOAD_HOOKS entry scopes; neither may
/// have left or been transferred. The installed clocked SUD path owns both.
/// This query does not establish clock/source admission itself.
pub(crate) unsafe fn interrupted_runtime_in_clocked_preload_handler() -> Option<bool> {
    unsafe { reverie_liteinst_domain_depth() }
        .checked_sub(2)
        .map(|inherited| inherited != 0 || crate::runtime::nested_tool_callback())
}

#[cfg(feature = "test-owned-cpuid")]
pub(crate) fn terminal_clock_scope() -> bool {
    unsafe { reverie_liteinst_domain_depth() == 1 }
}

#[cfg(test)]
pub(crate) mod tests;
