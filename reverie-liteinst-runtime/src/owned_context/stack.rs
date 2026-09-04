use std::alloc::Layout;
use std::alloc::alloc_zeroed;
use std::alloc::dealloc;
use std::io;
use std::ops::Range;
use std::ptr::NonNull;

#[derive(Debug)]
pub struct StackAllocation {
    pointer: NonNull<u8>,
    layout: Layout,
    #[cfg(test)]
    released: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
}

impl StackAllocation {
    pub fn allocate(bytes: usize) -> io::Result<Self> {
        unsafe { Self::allocate_with(bytes, alloc_zeroed) }
    }

    unsafe fn allocate_with(
        bytes: usize,
        allocator: unsafe fn(Layout) -> *mut u8,
    ) -> io::Result<Self> {
        let layout = Layout::from_size_align(bytes, 16)
            .ok()
            .filter(|layout| layout.size() != 0)
            .ok_or_else(|| io::Error::other("invalid private stack allocation size"))?;
        let pointer = NonNull::new(unsafe { allocator(layout) })
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self {
            pointer,
            layout,
            #[cfg(test)]
            released: None,
        })
    }

    pub fn range(&self) -> Range<usize> {
        self.pointer.as_ptr() as usize..self.pointer.as_ptr() as usize + self.layout.size()
    }

    pub fn leak(self) -> Range<usize> {
        let range = self.range();
        std::mem::forget(self);
        range
    }

    #[cfg(test)]
    pub(crate) fn witness(&mut self, released: std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        self.released = Some(released);
    }
}

impl Drop for StackAllocation {
    fn drop(&mut self) {
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
        #[cfg(test)]
        if let Some(released) = &self.released {
            released.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_zeroed_geometry_and_release() {
        let witness = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut allocation = StackAllocation::allocate(1024 * 1024).unwrap();
        allocation.witness(witness.clone());
        let range = allocation.range();
        assert_eq!(range.len(), 1024 * 1024);
        assert_eq!(range.start % 16, 0);
        assert!(
            unsafe { std::slice::from_raw_parts(range.start as *const u8, range.len()) }
                .iter()
                .all(|byte| *byte == 0)
        );
        drop(allocation);
        assert_eq!(witness.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn invalid_and_failed_allocations_are_fallible() {
        assert!(StackAllocation::allocate(0).is_err());
        assert!(StackAllocation::allocate(usize::MAX).is_err());
        let error =
            unsafe { StackAllocation::allocate_with(4096, |_| std::ptr::null_mut()) }.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
    }
}
