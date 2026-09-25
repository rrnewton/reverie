/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Vectored-I/O buffer descriptors used by [`crate::MemoryAccess`].
//!
//! With the default `std` feature these are exactly `std::io::IoSlice` and
//! `std::io::IoSliceMut`, so every existing backend and tool sees the same
//! types as before. Without `std`, `core` has no equivalent, so this module
//! provides layout-compatible descriptors with the subset of the `std` API that
//! Reverie uses: construction, (mutable) dereference to the borrowed bytes,
//! `Debug`, and `Copy`/`Clone` for the shared form. Both are laid out like the
//! Linux `struct iovec` (a base pointer followed by a length), as the `std`
//! types are on Unix.

#[cfg(feature = "std")]
pub use std::io::IoSlice;
#[cfg(feature = "std")]
pub use std::io::IoSliceMut;

#[cfg(not(feature = "std"))]
pub use self::no_std::IoSlice;
#[cfg(not(feature = "std"))]
pub use self::no_std::IoSliceMut;

#[cfg(not(feature = "std"))]
mod no_std {
    use core::fmt;
    use core::marker::PhantomData;
    use core::ops::Deref;
    use core::ops::DerefMut;
    use core::slice;

    /// A borrowed, immutable buffer descriptor laid out like `struct iovec`.
    #[derive(Copy, Clone)]
    #[repr(C)]
    pub struct IoSlice<'a> {
        base: *const u8,
        len: usize,
        _borrow: PhantomData<&'a [u8]>,
    }

    // SAFETY: an `IoSlice` is only a shared borrow of `[u8]`, which is `Send`
    // and `Sync`; `std::io::IoSlice` makes the same promise.
    unsafe impl Send for IoSlice<'_> {}
    // SAFETY: see the `Send` implementation above.
    unsafe impl Sync for IoSlice<'_> {}

    impl<'a> IoSlice<'a> {
        /// Creates a descriptor that borrows `buf` for `'a`.
        pub fn new(buf: &'a [u8]) -> Self {
            Self {
                base: buf.as_ptr(),
                len: buf.len(),
                _borrow: PhantomData,
            }
        }
    }

    impl Deref for IoSlice<'_> {
        type Target = [u8];

        fn deref(&self) -> &[u8] {
            // SAFETY: `base` and `len` were taken from a `&'a [u8]` that this
            // value still borrows.
            unsafe { slice::from_raw_parts(self.base, self.len) }
        }
    }

    impl fmt::Debug for IoSlice<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Debug::fmt(self.deref(), f)
        }
    }

    /// A borrowed, mutable buffer descriptor laid out like `struct iovec`.
    #[repr(C)]
    pub struct IoSliceMut<'a> {
        base: *mut u8,
        len: usize,
        _borrow: PhantomData<&'a mut [u8]>,
    }

    // SAFETY: an `IoSliceMut` is only a unique borrow of `[u8]`, which is
    // `Send` and `Sync`; `std::io::IoSliceMut` makes the same promise.
    unsafe impl Send for IoSliceMut<'_> {}
    // SAFETY: see the `Send` implementation above.
    unsafe impl Sync for IoSliceMut<'_> {}

    impl<'a> IoSliceMut<'a> {
        /// Creates a descriptor that uniquely borrows `buf` for `'a`.
        pub fn new(buf: &'a mut [u8]) -> Self {
            Self {
                base: buf.as_mut_ptr(),
                len: buf.len(),
                _borrow: PhantomData,
            }
        }
    }

    impl Deref for IoSliceMut<'_> {
        type Target = [u8];

        fn deref(&self) -> &[u8] {
            // SAFETY: `base` and `len` were taken from a `&'a mut [u8]` that
            // this value still uniquely borrows.
            unsafe { slice::from_raw_parts(self.base, self.len) }
        }
    }

    impl DerefMut for IoSliceMut<'_> {
        fn deref_mut(&mut self) -> &mut [u8] {
            // SAFETY: as for `deref`, and `&mut self` makes this the only
            // live access through the descriptor.
            unsafe { slice::from_raw_parts_mut(self.base, self.len) }
        }
    }

    impl fmt::Debug for IoSliceMut<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Debug::fmt(self.deref(), f)
        }
    }
}
