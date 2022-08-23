//! Amortizes the cost of small allocations by allocating memory in bigger chunks.
#![deny(missing_docs)]
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(
    feature = "nightly",
    feature(const_mut_refs, const_slice_from_raw_parts_mut)
)]
extern crate alloc;

use alloc::{vec, vec::Vec};
use core::{
    mem::ManuallyDrop,
    slice::{from_raw_parts, from_raw_parts_mut},
};
use hashbrown::HashMap;
use sealed::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    ALLOC_REF, ZALLOCATORS, ZERO_SIZE_BUFFER_PTR,
};

mod log2;
use log2::log2;

mod util;
use util::iec_bytes;

/// Lock-free allocator pool
#[cfg(any(feature = "pool", feature = "async-pool"))]
pub mod pool;
#[cfg(not(all(test, loom)))]
mod mutex {
    #[cfg(feature = "parking_lot")]
    pub(crate) use parking_lot::Mutex;
    #[cfg(not(feature = "parking_lot"))]
    pub(crate) use spin::Mutex;
}
#[cfg(all(test, loom))]
mod mutex {
    pub(crate) struct Mutex<T>(loom::sync::Mutex<T>);

    impl<T> Mutex<T> {
        pub(crate) fn new(data: T) -> Self {
            Self(loom::sync::Mutex::new(data))
        }

        pub(crate) fn lock(&self) -> loom::sync::MutexGuard<'_, T> {
            self.0.lock().unwrap()
        }
    }
}

use mutex::Mutex;

mod sealed {
    use super::Zallocator;

    #[cfg(not(all(test, loom)))]
    pub(crate) mod sync {
        pub(crate) use alloc::sync::Arc;
        pub(crate) use core::sync::*;
    }

    #[cfg(all(test, loom))]
    pub(crate) mod sync {
        pub(crate) use loom::sync::*;
    }

    use sync::atomic::AtomicU64;

    #[cfg(all(test, loom))]
    loom::lazy_static! {
        pub(crate) static ref ALLOC_REF: AtomicU64 = AtomicU64::new(0);

        pub(crate) static ref ZALLOCATORS: crate::mutex::Mutex<hashbrown::HashMap<u64, Zallocator>> =
            crate::mutex::Mutex::new(hashbrown::HashMap::new());
    }

    #[cfg(not(all(test, loom)))]
    lazy_static::lazy_static! {
        pub(crate) static ref ALLOC_REF: AtomicU64 = {
            #[cfg(feature = "std")]
            let r: u64 = {
                let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("SystemTime before UNIX EPOCH!")
                        .as_nanos() as u64,
                );
                rand::Rng::gen_range(&mut rng, 0..1 << 16)
            };

            #[cfg(not(feature = "std"))]
            let r: u64 = {
                use rand::{rngs::OsRng, RngCore};
                let mut key = [0u8; 8];
                OsRng.fill_bytes(&mut key);
                let r = OsRng.next_u64();
                r
            };
            AtomicU64::new(r << 48)
        };
        pub(crate) static ref ZALLOCATORS: crate::mutex::Mutex<hashbrown::HashMap<u64, Zallocator>> =
            crate::mutex::Mutex::new(hashbrown::HashMap::new());
    }

    pub(crate) static mut ZERO_SIZE_BUFFER_PTR: [u8; 0] = [];
}

/// type alias for core::result::Result<T, Overflow>
pub type Result<T> = core::result::Result<T, Overflow>;

/// Overflow represents an error that exceeds the max size (`1 << 30`) of the chunk allowed to an allocator.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Overflow(u64);

impl core::fmt::Display for Overflow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Cannot allocate a buffer of size {}, exceeds the maximum size {}",
            self.0,
            Zallocator::MAX_ALLOC
        )
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Overflow {}

/// The stats of allocators
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ZallocatorStats {
    tags: HashMap<&'static str, u64>,
    nums: HashMap<&'static str, u64>,
}

impl core::fmt::Display for ZallocatorStats {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (tag, sz) in &self.tags {
            write!(
                f,
                "Tag: {} Num: {} Size: {}",
                *tag,
                self.nums.get(tag).unwrap(),
                iec_bytes(*sz)
            )?;
        }
        Ok(())
    }
}

/// Allocate a new allocator by specific reference
#[inline]
pub fn allocate_from(reference: u64) -> Option<Zallocator> {
    let a = ZALLOCATORS.lock();
    a.get(&reference).map(core::clone::Clone::clone)
}

/// Release an allocator by reference
#[inline]
pub fn release_from(reference: u64) {
    let mut a = ZALLOCATORS.lock();
    a.remove(&reference);
}

/// Returns the stats of the allocators
pub fn stats() -> ZallocatorStats {
    let allocators = ZALLOCATORS.lock();
    let mut tags: HashMap<&'static str, u64> = HashMap::with_capacity(allocators.len());
    let mut nums: HashMap<&'static str, u64> = HashMap::with_capacity(allocators.len());
    for (_, a) in allocators.iter() {
        match tags.get_mut(a.tag) {
            Some(v) => {
                *v += a.allocated();
                let num = nums.get_mut(a.tag).unwrap();
                *num += 1;
            }
            None => {
                tags.insert(a.tag, a.allocated());
                nums.insert(a.tag, 1);
            }
        }
    }

    ZallocatorStats { tags, nums }
}

/// Release all allocators
#[inline]
pub fn release_all() {
    let mut a = ZALLOCATORS.lock();
    *a = HashMap::new();
}

/// Amortizes the cost of small allocations by allocating memory in bigger chunks.
#[derive(Clone)]
#[cfg_attr(not(loom), derive(Debug))]
pub struct Zallocator {
    tag: &'static str,
    inner: Arc<ZallocatorInner>,
    reference: u64,
}

#[cfg_attr(not(loom), derive(Debug))]
struct ZallocatorInner {
    composite_idx: AtomicU64, // Stores bufIdx in 32 MSBs and posIdx in 32 LSBs.
    buffers: Mutex<ZallocatorBuffers>,
}

impl ZallocatorInner {
    #[inline(always)]
    fn new(composite_idx: u64, buffers: ZallocatorBuffers) -> Arc<Self> {
        Arc::new(Self {
            composite_idx: AtomicU64::new(composite_idx),
            buffers: Mutex::new(buffers),
        })
    }
}

#[derive(Debug)]
struct ZallocatorBuffers {
    buffers: Vec<ZallocatorBuffer>,
    allocated: u64,
}

impl ZallocatorBuffers {
    #[inline(always)]
    fn new(cap: u64) -> Self {
        let cap = 1u64 << cap;
        Self {
            buffers: (0..64usize)
                .into_iter()
                .map(|idx| {
                    if idx == 0 {
                        ZallocatorBuffer::new(vec![0; cap as usize])
                    } else {
                        ZallocatorBuffer::new(Vec::new())
                    }
                })
                .collect(),
            allocated: cap,
        }
    }

    #[inline(always)]
    fn add_buffer_at(&mut self, mut buf_idx: usize, size: u64) {
        loop {
            let buffers_len = self.buffers.len();
            assert!(
                buf_idx < buffers_len,
                "Zallocator cannot allocate more than {} buffers",
                buffers_len
            );

            let buf_cap = self.buffers[buf_idx].capacity();
            if buf_cap == 0 {
                break;
            }

            if size <= buf_cap as u64 {
                // No need to do anything. We already have a buffer which can satisfy minSz.
                return;
            }
            buf_idx += 1;
        }
        assert!(buf_idx > 0);
        // We need to allocate a new buffer.
        // Make page_size double of the last allocation.
        let mut page_size: u64 = (2 * self.buffers[buf_idx - 1].capacity()) as u64;

        // Ensure pageSize is bigger than sz.
        while page_size < size {
            page_size *= 2;
        }

        // If bigger than maxAlloc, trim to maxAlloc.
        if page_size > Zallocator::MAX_ALLOC {
            page_size = Zallocator::MAX_ALLOC;
        }

        let buf = ZallocatorBuffer::new(vec![0; page_size as usize]);
        assert!(self.buffers[buf_idx].capacity() == 0);

        self.allocated += page_size;
        self.buffers[buf_idx] = buf;
    }
}

impl Zallocator {
    /// The maximum size of an allocator
    pub const MAX_ALLOC: u64 = 1 << 30;

    const NODE_ALIGN: u64 = (core::mem::size_of::<u64>() - 1) as u64;

    /// Creates an allocator starting with the given size.
    pub fn new(mut size: usize, tag: &'static str) -> Result<Self> {
        let reference = ALLOC_REF.fetch_add(1, Ordering::AcqRel);
        // We should not allow a zero sized page because addBufferWithMinSize
        // will run into an infinite loop trying to double the pagesize.
        if size < 512 {
            size = 512;
        }

        let mut l2 = log2(size) as u64;
        if size.count_ones() > 1 {
            l2 += 1;
        }

        let this = Self {
            tag,
            inner: ZallocatorInner::new(0, ZallocatorBuffers::new(l2)),
            reference,
        };

        let mut allocators = ZALLOCATORS.lock();

        match allocators.entry(reference) {
            hashbrown::hash_map::Entry::Occupied(v) => Ok(v.get().clone()),
            hashbrown::hash_map::Entry::Vacant(v) => {
                v.insert(this.clone());
                Ok(this)
            }
        }
    }

    /// Get the tag of the allocator
    #[inline(always)]
    pub fn get_tag(&self) -> &'static str {
        self.tag
    }

    /// Set the tag for this allocator
    #[inline(always)]
    pub fn set_tag(&mut self, tag: &'static str) {
        self.tag = tag;
    }

    /// Reset the allocator
    #[inline]
    pub fn reset(&self) {
        self.inner.composite_idx.store(0, Ordering::SeqCst);
    }

    /// Returns the size of the allocations so far.
    pub fn size(&self) -> usize {
        let pos = Position::parse(self.inner.composite_idx.load(Ordering::Relaxed));
        let mut sz = 0;
        for (idx, buf) in self.inner.buffers.lock().buffers.iter().enumerate() {
            if idx < pos.buf {
                sz += buf.capacity();
                continue;
            }
            sz += pos.pos;
            return sz;
        }
        // Size should not reach here
        unreachable!()
    }

    /// Release would release the allocator.
    #[inline]
    pub fn release(self) {
        ZALLOCATORS.lock().remove(&self.reference);
    }

    /// Allocate a buffer with according to `size` (well-aligned)
    pub fn allocate_aligned(&self, size: u64) -> Result<Buffer> {
        self.allocate_in(size, true)
    }

    /// Allocate a buffer with according to `size` (well-aligned) without checking size
    ///
    /// # Panics
    /// Size larger than `1 << 30`.
    pub fn allocate_aligned_unchecked(&self, size: u64) -> Buffer {
        self.allocate_in_unchecked(size, true)
    }

    /// Allocate a buffer with according to `size`
    pub fn allocate(&self, size: u64) -> Result<Buffer> {
        self.allocate_in(size, false)
    }

    /// Allocate a buffer with according to `size` without checking size.
    ///
    /// # Panics
    /// Size larger than `1 << 30`.
    pub fn allocate_unchecked(&self, size: u64) -> Buffer {
        self.allocate_in_unchecked(size, false)
    }

    /// Allocate a buffer with the same length of `buf`, and copy the contents of buf to the [`Buffer`][buffer].
    ///
    /// [buffer]: struct.Buffer.html
    pub fn copy_from(&self, buf: impl AsRef<[u8]>) -> Result<Buffer> {
        let b = buf.as_ref();
        self.allocate(b.len() as u64).map(|o| {
            o.copy_from_slice(b);
            o
        })
    }

    /// Truncate the allocator to new size.
    pub fn truncate(&self, max: u64) {
        let mut inner = self.inner.buffers.lock();
        let mut alloc = 0u64;
        let mut new_allocated = inner.allocated;
        let buffers = &mut inner.buffers;
        for b in buffers.iter_mut() {
            let b_cap = b.capacity() as u64;
            if b_cap == 0 {
                break;
            }

            alloc += b_cap;
            if alloc < max {
                continue;
            }
            new_allocated -= b_cap;
            *b = ZallocatorBuffer::null();
        }
        inner.allocated = new_allocated;
    }

    #[inline(always)]
    fn allocate_in(&self, mut size: u64, aligned: bool) -> Result<Buffer> {
        if size > Self::MAX_ALLOC {
            return Err(Overflow(size));
        }

        if size == 0 {
            return Ok(Buffer::null());
        }

        if aligned {
            size += Self::NODE_ALIGN;
        }

        Ok(self.allocate_in_helper(size, aligned))
    }

    #[inline(always)]
    fn allocate_in_unchecked(&self, mut size: u64, aligned: bool) -> Buffer {
        if size > Self::MAX_ALLOC {
            panic!("zallocator: {}", Overflow(size));
        }

        if size == 0 {
            return Buffer::null();
        }

        if aligned {
            size += Self::NODE_ALIGN;
        }

        self.allocate_in_helper(size, aligned)
    }

    #[inline(always)]
    fn allocate_in_helper(&self, size: u64, aligned: bool) -> Buffer {
        loop {
            let pos =
                Position::parse(self.inner.composite_idx.fetch_add(size, Ordering::SeqCst) + size);
            let mut inner = self.inner.buffers.lock();
            let buf = &mut inner.buffers[pos.buf];
            let buf_cap = buf.capacity();
            let buf_ptr = buf.as_mut_ptr();
            if pos.pos > buf_cap {
                let new_pos = self.inner.composite_idx.load(Ordering::Acquire);
                let Position {
                    buf: new_buf_idx,
                    pos: _,
                } = Position::parse(new_pos);

                if new_buf_idx != pos.buf {
                    continue;
                }
                inner.add_buffer_at(pos.buf + 1, size);
                self.inner
                    .composite_idx
                    .store(((pos.buf + 1) as u64) << 32, Ordering::Release);
                // We added a new buffer. Let's acquire slice the right
                continue;
            }

            if aligned {
                let offset = pos.pos - (size as usize);
                buf.vec[offset..pos.pos].fill(0);
                let ptr = unsafe { buf_ptr.add(offset) };
                let aligned =
                    unsafe { (ptr.add(Self::NODE_ALIGN as usize) as u64) & (!Self::NODE_ALIGN) };

                let start = (aligned - (ptr as u64)) + offset as u64;
                buf.set_len(buf_cap - start as usize);
                return Buffer {
                    ptr: buf_ptr,
                    cap: buf_cap,
                    start: start as usize,
                    end: (start + size) as usize,
                    refs: buf.refs(),
                };
            } else {
                let start = pos.pos - (size as usize);
                buf.set_len(buf_cap - size as usize);
                return Buffer {
                    ptr: buf_ptr,
                    cap: buf_cap,
                    start,
                    end: pos.pos,
                    refs: buf.refs(),
                };
            }
        }
    }

    /// Returns how many bytes are allocated
    #[inline(always)]
    pub fn allocated(&self) -> u64 {
        self.inner.buffers.lock().allocated
    }
}

impl core::fmt::Display for Zallocator {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "Zallocator: {}", self.reference)?;
        let mut cum = 0usize;
        let mut sz = 0usize;
        let pos = Position::parse(self.inner.composite_idx.load(Ordering::Relaxed));
        let buffers = self.inner.buffers.lock();

        for (idx, b) in buffers.buffers.iter().enumerate() {
            cum += b.capacity();

            if b.capacity() == 0 {
                break;
            }

            if idx < pos.buf {
                sz += b.capacity();
                writeln!(f, "index: {} len: {} cum: {}", idx, b.capacity(), cum)?;
                continue;
            }

            sz += pos.pos;
            writeln!(f, "index: {} len: {} cum: {}", idx, b.capacity(), cum)?;
        }

        writeln!(f, "buffer_index: {} position_index: {}", pos.buf, pos.pos)?;
        writeln!(f, "Size: {}", sz)?;

        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ZallocatorBuffer {
    vec: ManuallyDrop<Vec<u8>>,
    len: usize,
    refs: Arc<()>,
}

impl ZallocatorBuffer {
    #[inline(always)]
    fn new(vec: Vec<u8>) -> Self {
        let v = ManuallyDrop::new(vec);
        Self {
            vec: v,
            len: 0,
            refs: Arc::new(()),
        }
    }

    #[inline(always)]
    fn null() -> Self {
        Self {
            vec: ManuallyDrop::new(Vec::new()),
            len: 0,
            refs: Arc::new(()),
        }
    }

    #[inline(always)]
    #[allow(dead_code)]
    fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    fn set_len(&mut self, len: usize) {
        self.len = len;
    }

    #[inline(always)]
    fn capacity(&self) -> usize {
        self.vec.capacity()
    }

    #[inline(always)]
    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.vec.as_mut_ptr()
    }

    #[inline(always)]
    fn refs(&self) -> Arc<()> {
        self.refs.clone()
    }
}

impl Drop for ZallocatorBuffer {
    fn drop(&mut self) {
        if Arc::strong_count(&self.refs) == 1 {
            let cap = self.capacity();
            unsafe {
                let _ = Vec::from_raw_parts(self.as_mut_ptr(), cap, cap);
            }
        }
    }
}

unsafe impl Send for ZallocatorBuffer {}
unsafe impl Sync for ZallocatorBuffer {}

/// A buffer that is allocated by the zallocator.
///
/// # Note
/// The buffer guarantees no read/write after deallocate happen (even though all of the allocators are freed,
/// it is safe to do any read or write), but does not promise no data-race when doing concurrent writing (users should take care of this).
#[cfg_attr(not(loom), derive(PartialEq, Eq, Hash))]
#[derive(Debug, Clone)]
pub struct Buffer {
    ptr: *mut u8,
    cap: usize,
    start: usize,
    end: usize,
    refs: Arc<()>,
}

impl Buffer {
    #[inline(always)]
    fn is_dangling(&self) -> bool {
        unsafe { (self.ptr as u64) == (ZERO_SIZE_BUFFER_PTR.as_ptr() as u64) }
    }

    #[inline(always)]
    fn null() -> Self {
        Self {
            ptr: unsafe { ZERO_SIZE_BUFFER_PTR.as_mut_ptr() },
            cap: 0,
            start: 0,
            end: 0,
            refs: Arc::new(()),
        }
    }

    /// Returns a raw pointer to the slice's buffer.
    pub const fn as_ptr(&self) -> *const u8 {
        unsafe { self.ptr.add(self.start) }
    }

    /// Returns an unsafe mutable pointer to the slice's buffer.
    pub const fn as_mut_ptr(&self) -> *mut u8 {
        unsafe { self.ptr.add(self.start) }
    }

    /// Returns the capacity of the buffer
    #[inline(always)]
    pub const fn capacity(&self) -> usize {
        self.end - self.start
    }

    /// Copy src to self
    ///
    /// # Panics
    /// Panics if the length of src is greater than the capacity of buffer.
    #[inline(always)]
    pub const fn copy_from_slice(&self, src: &[u8]) {
        if self.capacity() < src.len() {
            panic!("Buffer capacity is not enough");
        }

        unsafe {
            core::ptr::copy_nonoverlapping(&src[0], self.as_mut_ptr(), src.len());
        }
    }

    /// Returns the undelying mutable slice of the buffer
    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    #[cfg(not(feature = "nightly"))]
    pub fn as_mut_slice(&self) -> &mut [u8] {
        unsafe { from_raw_parts_mut(self.ptr.add(self.start), self.capacity()) }
    }

    /// Returns the undelying mutable slice of the buffer
    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    #[cfg(feature = "nightly")]
    pub const fn as_mut_slice(&self) -> &mut [u8] {
        unsafe { from_raw_parts_mut(self.ptr.add(self.start), self.capacity()) }
    }

    /// Returns the undelying immutable slice of the buffer
    #[inline(always)]
    pub const fn as_slice(&self) -> &[u8] {
        unsafe { from_raw_parts(self.ptr.add(self.start), self.capacity()) }
    }

    /// Returns a new buffer of self for the provided range.
    ///
    /// # Panics
    ///
    /// Requires that `begin <= end` and `end <= self.capacity()`, otherwise slicing
    /// will panic.
    #[inline(always)]
    pub fn slice(&self, range: impl core::ops::RangeBounds<usize>) -> Self {
        let start = match range.start_bound() {
            core::ops::Bound::Included(start) => *start,
            core::ops::Bound::Excluded(start) => start.checked_add(1).expect("out of range"),
            core::ops::Bound::Unbounded => 0,
        };

        assert!(
            self.start.checked_add(start).expect("out of range") <= self.end,
            "out of range"
        );

        let end = match range.end_bound() {
            core::ops::Bound::Included(end) => end.checked_add(1).expect("out of range"),
            core::ops::Bound::Excluded(end) => *end,
            core::ops::Bound::Unbounded => self.end,
        };

        assert!(
            start <= end,
            "range start must not be greater than end: {:?} <= {:?}",
            start,
            end,
        );

        let len = self.capacity();
        assert!(
            end <= len,
            "range end out of bounds: {:?} <= {:?}",
            end,
            len,
        );

        if end == start {
            return Buffer::null();
        }

        assert!(
            (self.start + start).checked_add(end).expect("out of range") <= self.end,
            "out of range"
        );

        Self {
            ptr: self.ptr,
            cap: self.cap,
            start: self.start + start,
            end: self.start + start + end,
            refs: self.refs.clone(),
        }
    }
}

impl core::ops::Deref for Buffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        // Safety: the underlying will not be dropped and
        // the ptr will be not possible owned in another Buffer
        unsafe { from_raw_parts(self.ptr.add(self.start), self.capacity()) }
    }
}

impl core::ops::DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safety: the underlying will not be dropped and
        // the ptr will be not possible owned in another Buffer
        unsafe { from_raw_parts_mut(self.ptr.add(self.start), self.capacity()) }
    }
}

impl AsRef<[u8]> for Buffer {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl AsMut<[u8]> for Buffer {
    fn as_mut(&mut self) -> &mut [u8] {
        self
    }
}

impl core::ops::Index<usize> for Buffer {
    type Output = u8;

    fn index(&self, index: usize) -> &Self::Output {
        let cap = self.capacity();
        assert!(
            cap > index,
            "Index out of range, index: {}, len: {}",
            index,
            cap
        );

        // Safety: the underlying will not be dropped and
        // the ptr will be not possible owned in another Buffer
        // and start + index will not overflow
        unsafe { from_raw_parts(self.ptr.add(self.start), cap).get_unchecked(index) }
    }
}

impl core::ops::IndexMut<usize> for Buffer {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        let cap = self.capacity();
        assert!(
            cap > index,
            "Index out of range, index: {}, len: {}",
            index,
            cap
        );

        // Safety: the underlying will not be dropped and
        // the ptr will be not possible owned in another Buffer
        // and start + index will not overflow
        unsafe { from_raw_parts_mut(self.ptr.add(self.start), cap).get_unchecked_mut(index) }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        if self.is_dangling() {
            return;
        }

        if Arc::strong_count(&self.refs) == 1 {
            unsafe {
                let _ = Vec::from_raw_parts(self.ptr, self.cap, self.cap);
            }
        }
    }
}

unsafe impl Send for Buffer {}
unsafe impl Sync for Buffer {}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
struct Position {
    buf: usize,
    pos: usize,
}

impl Position {
    const fn parse(pos: u64) -> Self {
        Self {
            buf: (pos >> 32) as usize,
            pos: (pos & 0xFFFFFFFF) as usize,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    #[cfg(not(loom))]
    use rand::Rng;

    use super::*;

    #[test]
    #[cfg(not(loom))]
    fn test_allocate_size() {
        let a = Zallocator::new(1024, "test").unwrap();
        assert_eq!(1024, a.inner.buffers.lock().buffers[0].vec.len());
        a.release();

        let a = Zallocator::new(1025, "test").unwrap();
        assert_eq!(2048, a.inner.buffers.lock().buffers[0].vec.len());
        a.release();
    }

    #[test]
    #[cfg(not(loom))]
    fn test_allocate() {
        let a = Zallocator::new(1024, "test allocate").unwrap();

        fn check(a: &Zallocator) {
            assert_eq!(0, a.allocate(0).unwrap().len());
            assert_eq!(1, a.allocate(1).unwrap().len());
            assert_eq!((1 << 20) + 1, a.allocate((1 << 20) + 1).unwrap().len());
            assert_eq!(256 << 20, a.allocate(256 << 20).unwrap().len());
            assert!(a.allocate(Zallocator::MAX_ALLOC + 1).is_err());
        }

        check(&a);
        let prev = a.allocated();
        a.reset();
        check(&a);
        assert_eq!(prev, a.allocated());
        assert!(prev >= 1 + (1 << (20 + 1)) + (256 << 20));
        a.release();
    }

    #[test]
    #[cfg(not(loom))]
    // TODO: fix miri test for allocate_aligned
    #[cfg_attr(miri, ignore)]
    fn test_allocate_aligned() {
        let a = Zallocator::new(1024, "test allocate aligned").unwrap();

        a.allocate(1).unwrap();
        let out = a.allocate(1).unwrap();
        let ptr = (&out[0] as *const u8) as u64;
        assert_eq!(ptr % 8, 1);

        let out = a.allocate_aligned(5).unwrap();
        let ptr = (&out[0] as *const u8) as u64;
        assert_eq!(ptr % 8, 0);

        let out = a.allocate_aligned(3).unwrap();
        let ptr = (&out[0] as *const u8) as u64;
        assert_eq!(ptr % 8, 0);
        a.release();
    }

    #[test]
    #[cfg(not(loom))]
    fn test_allocate_reset() {
        let a = Zallocator::new(1024, "test allocate reset").unwrap();

        let mut buf = [0u8; 128];
        let mut rng = rand::thread_rng();
        rng.fill(&mut buf[..]);
        (0..1000).for_each(|_| {
            a.copy_from(&buf).unwrap();
        });

        let prev = a.allocated();
        a.reset();
        (0..100).for_each(|_| {
            a.copy_from(&buf).unwrap();
        });

        assert_eq!(prev, a.allocated());
        a.release();
    }

    #[test]
    #[cfg(not(loom))]
    fn test_allocate_truncate() {
        let a = Zallocator::new(16, "test allocate truncate").unwrap();
        let mut buf = [0u8; 128];
        let mut rng = rand::thread_rng();
        rng.fill(&mut buf[..]);
        (0..1000).for_each(|_| {
            a.copy_from(&buf).unwrap();
        });

        const N: u64 = 2048;
        a.truncate(N);
        assert!(a.allocated() <= N);
        a.release();
    }

    #[test]
    #[cfg(not(loom))]
    #[cfg_attr(miri, ignore)]
    fn test_concurrent() {
        let a = Zallocator::new(63, "test concurrent").unwrap();
        const N: u64 = 10240;
        const M: u64 = 16;
        let wg = Arc::new(AtomicU64::new(0));

        let m = Arc::new(Mutex::new(hashbrown::HashSet::<usize>::new()));
        let _ = (0..M)
            .map(|_| {
                let wg = wg.clone();
                wg.fetch_add(1, Ordering::SeqCst);
                let m = m.clone();
                let a = a.clone();
                std::thread::spawn(move || {
                    let mut bufs = Vec::with_capacity(N as usize);
                    (0..N).for_each(|_| {
                        let buf = a.allocate(M).unwrap();
                        assert_eq!(buf.len(), 16);
                        bufs.push(buf.as_ptr() as usize);
                    });

                    let mut s = m.lock();
                    bufs.iter().for_each(|b| {
                        assert!(!s.contains(b), "Did not expect to see the same ptr");
                        s.insert(*b);
                    });
                    wg.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect::<Vec<_>>();

        while wg.load(Ordering::SeqCst) != 0 {}

        let set = m.lock();
        assert_eq!(set.len(), (N * M) as usize);

        let mut sorted = set.iter().copied().collect::<Vec<_>>();
        sorted.sort_unstable();
        let mut last = sorted[0];
        for offset in sorted[1..].iter().copied() {
            assert!(
                offset - last >= 16,
                "Should not have less than 16: {} {}",
                offset,
                last
            );
            last = offset;
        }

        release_all();
    }

    #[test]
    fn test_copy_from_slice() {
        let a = Zallocator::new(1024, "test copy_from_slice").unwrap();
        let b1 = a.allocate_unchecked(100);
        b1.copy_from_slice(&[1; 80]);
        let b2 = a.allocate_unchecked(100);
        b2.copy_from_slice(&b1);
        for i in 0..80 {
            assert_eq!(b2[i], 1);
        }

        for i in 80..100 {
            assert_eq!(b2[i], 0);
        }
    }
}
