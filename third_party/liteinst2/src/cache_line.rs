//! Cache-line geometry used by live patch publication.

use core::fmt;
use core::num::NonZeroUsize;

/// A validated, non-zero cache-line size in bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CacheLineSize(NonZeroUsize);

impl CacheLineSize {
    /// Creates a cache-line size, returning `None` for zero bytes.
    pub const fn new(bytes: usize) -> Option<Self> {
        match NonZeroUsize::new(bytes) {
            Some(bytes) => Some(Self(bytes)),
            None => None,
        }
    }

    /// Returns the cache-line size in bytes.
    pub const fn get(self) -> usize {
        self.0.get()
    }

    /// Returns the byte offset of `address` within its cache line.
    pub fn offset(self, address: usize) -> usize {
        address % self.get()
    }

    /// Returns the number of bytes from `address` through the current line.
    pub fn bytes_until_boundary(self, address: usize) -> usize {
        self.get() - self.offset(address)
    }

    /// Returns the split offset when `[address, address + len)` crosses a line.
    ///
    /// The comparison avoids computing `address + len`, so classification is
    /// well-defined even near the end of the address space.
    pub fn split_offset(self, address: usize, len: usize) -> Option<usize> {
        let front_len = self.bytes_until_boundary(address);
        (len > front_len).then_some(front_len)
    }

    /// Returns whether `[address, address + len)` crosses a cache-line boundary.
    pub fn crosses(self, address: usize, len: usize) -> bool {
        self.split_offset(address, len).is_some()
    }
}

impl TryFrom<usize> for CacheLineSize {
    type Error = InvalidCacheLineSize;

    fn try_from(bytes: usize) -> Result<Self, Self::Error> {
        Self::new(bytes).ok_or(InvalidCacheLineSize)
    }
}

/// Error returned when a zero-byte cache-line size is requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidCacheLineSize;

impl fmt::Display for InvalidCacheLineSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("cache-line size must be non-zero")
    }
}

impl std::error::Error for InvalidCacheLineSize {}

#[cfg(test)]
mod tests {
    use super::CacheLineSize;

    const LINE: CacheLineSize = CacheLineSize::new(64).unwrap();

    #[test]
    fn range_ending_at_boundary_does_not_cross() {
        assert!(!LINE.crosses(56, 8));
    }

    #[test]
    fn range_extending_past_boundary_crosses() {
        assert_eq!(LINE.split_offset(57, 8), Some(7));
    }

    #[test]
    fn zero_length_range_does_not_cross() {
        assert!(!LINE.crosses(63, 0));
    }

    #[test]
    fn span_larger_than_a_line_crosses() {
        assert_eq!(LINE.split_offset(0, 65), Some(64));
    }

    #[test]
    fn classification_does_not_overflow_at_max_address() {
        assert_eq!(LINE.split_offset(usize::MAX, 2), Some(1));
    }
}
