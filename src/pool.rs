mod sync;
pub use sync::*;

#[cfg(all(
    feature = "async-channel",
    feature = "pollster",
    feature = "futures",
    feature = "async-io"
))]
mod r#async;
#[cfg(all(
    feature = "async-channel",
    feature = "pollster",
    feature = "futures",
    feature = "async-io"
))]
pub use r#async::*;

use super::{Buffer, Result};

/// Amortizes the cost of small allocations by allocating memory in bigger chunks.
#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct Allocator {
    z: crate::Zallocator,
}

impl Allocator {
    #[inline]
    fn new(size: usize, tag: &'static str) -> Result<Self> {
        Self::new_in(size, tag)
    }

    #[inline]
    fn new_in(size: usize, tag: &'static str) -> Result<Self> {
        crate::Zallocator::new(size, tag).map(|z| Self { z })
    }

    /// Get the tag of the allocator
    #[inline(always)]
    pub fn get_tag(&self) -> &'static str {
        self.z.get_tag()
    }

    /// Set the tag for this allocator
    #[inline(always)]
    pub fn set_tag(&self, tag: &'static str) {
        self.z.set_tag(tag)
    }

    /// Reset the allocator
    #[inline]
    pub fn reset(&self) {
        self.z.reset();
    }

    /// Returns the size of the allocations so far.
    #[inline]
    pub fn size(&self) -> usize {
        self.z.size()
    }

    /// Release would release the allocator.
    #[inline]
    pub fn release(self) {
        self.z.release();
    }

    /// Allocate a buffer with according to `size` (well-aligned)
    #[inline]
    pub fn allocate_aligned(&self, size: u64) -> Result<Buffer> {
        self.z.allocate_aligned(size)
    }

    /// Allocate a buffer with according to `size` (well-aligned) without checking size
    ///
    /// # Panics
    /// Size larger than `1 << 30`.
    #[inline]
    pub fn allocate_aligned_unchecked(&self, size: u64) -> Buffer {
        self.z.allocate_aligned_unchecked(size)
    }

    /// Allocate a buffer with according to `size`
    #[inline]
    pub fn allocate(&self, size: u64) -> Result<Buffer> {
        self.z.allocate(size)
    }

    /// Allocate a buffer with according to `size` without checking size.
    ///
    /// # Panics
    /// Size larger than `1 << 30`.
    #[inline]
    pub fn allocate_unchecked(&self, size: u64) -> Buffer {
        self.z.allocate_unchecked(size)
    }

    /// Allocate a buffer with the same length of `buf`, and copy the contents of buf to the [`Buffer`][buffer].
    ///
    /// [buffer]: struct.Buffer.html
    #[inline]
    pub fn copy_from(&self, buf: impl AsRef<[u8]>) -> Result<Buffer> {
        self.z.copy_from(buf)
    }

    /// Truncate the allocator to new size.
    #[inline]
    pub fn truncate(&self, max: u64) {
        self.z.truncate(max)
    }

    #[inline]
    pub(crate) fn can_put_back(&self) -> bool {
        self.z.can_put_back()
    }
}
