use crate::sealed::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use super::{Buffer, Result};
#[cfg(all(feature = "future", not(feature = "std")))]
use alloc::boxed::Box;
#[cfg(feature = "future")]
use core::{future::Future, pin::Pin};

#[cfg(any(feature = "std", feature = "future"))]
use core::time::Duration;
use crossbeam_queue::ArrayQueue;

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

/// # Introduction
///
/// Lock-free and runtime agnostic async allocator pool.
/// When fetching an allocator, if there is no idle allocator in pool,
/// then will create a new allocator. Otherwise, it reuses the idle allocator.
///
/// # Examples
///
/// ## Manually Free
/// By default, the pool will not free idle allocators in pool.
///
/// ```
/// use zallocator::pool::AllocatorPool;
///
/// # tokio_test::block_on(async {
/// let pool = AllocatorPool::new(2);
/// let a = pool.fetch(1024, "a").unwrap();
/// let b = pool.fetch(1024, "b").unwrap();
/// let c = pool.fetch(1024, "c").unwrap();
///
/// pool.put(a);
/// pool.put(b);
///
/// // c will be freed directly, because the pool is full.
/// pool.put(c);
/// assert_eq!(2, pool.idles());
///
/// # });
/// ```
///
/// ## Auto Free by `std::thread::spawn`
/// Auto free idle allocators in pool, this will spawn a new thread when constructing the pool.
/// ```
/// use zallocator::pool::AllocatorPool;
/// use tokio::time::{sleep, Duration};
///
/// # #[cfg(feature = "std")]
/// # tokio_test::block_on(async {
/// let pool = AllocatorPool::with_spawn(2, core::time::Duration::from_millis(1000));
/// let a = pool.fetch(1024, "a").unwrap();
/// let b = pool.fetch(1024, "b").unwrap();
/// pool.put(a);
/// pool.put(b);
///
/// assert_eq!(2, pool.idles());
/// pool.fetch(1024, "c").unwrap();
///
/// sleep(Duration::from_millis(1000)).await;
/// assert_eq!(1, pool.idles());
///
/// sleep(Duration::from_millis(2000)).await;
/// assert_eq!(0, pool.idles());
/// # });
/// ```
///
/// ## Auto Free by task
/// Auto free idle allocators in pool, this will spawn a new task (by any async runtime spawner, this example use `tokio`)
/// when constructing the pool.
///
/// ```
/// use zallocator::pool::AllocatorPool;
/// use tokio::time::{sleep, Duration};
///
/// # #[cfg(feature = "tokio")]
/// # tokio_test::block_on(async {
/// let pool = AllocatorPool::with_task(2, core::time::Duration::from_millis(1000), tokio::spawn);
/// let a = pool.fetch(1024, "a").unwrap();
/// let b = pool.fetch(1024, "b").unwrap();
/// pool.put(a);
/// pool.put(b);
///
/// assert_eq!(2, pool.idles());
/// pool.fetch(1024, "c").unwrap();
///
/// sleep(Duration::from_millis(1000)).await;
/// assert_eq!(1, pool.idles());
///
/// sleep(Duration::from_millis(2000)).await;
/// assert_eq!(0, pool.idles());
/// # });
/// ```
///
/// ## Auto Free by local task
///
/// Auto free idle allocators in pool, this will spawn a new local task (by any async runtime spawner, this example use `tokio`)
/// when constructing the pool.
///
/// ```
/// use zallocator::pool::AllocatorPool;
/// use tokio::time::{sleep, Duration};
///
/// # #[cfg(feature = "tokio")]
/// # tokio_test::block_on(async {
///
/// let local = tokio::task::LocalSet::new();
///
/// local.run_until(async {
///     let pool = AllocatorPool::with_task(2, core::time::Duration::from_millis(1000), tokio::task::spawn_local);
///     let a = pool.fetch(1024, "a").unwrap();
///     let b = pool.fetch(1024, "b").unwrap();
///     pool.put(a);
///     pool.put(b);
///
///     assert_eq!(2, pool.idles());
///     pool.fetch(1024, "c").unwrap();
///
///     sleep(Duration::from_millis(1000)).await;
///     assert_eq!(1, pool.idles());
///
///     sleep(Duration::from_millis(2000)).await;
///     assert_eq!(0, pool.idles());
/// }).await;
/// # });
/// ```
///
#[derive(Debug)]
pub struct AllocatorPool<H = ()>
where
    H: Handle,
{
    queue: Arc<ArrayQueue<Allocator>>,
    handle: Option<H>,
    num_fetches: Arc<AtomicU64>,
}

impl AllocatorPool {
    /// Creates a new pool without auto free idle allocators
    ///
    /// # Example
    /// ```
    /// use zallocator::pool::AllocatorPool;
    ///
    /// let pool = AllocatorPool::new(2);
    /// let a = pool.fetch(1024, "test").unwrap();
    /// pool.put(a);
    /// ```
    #[inline]
    pub fn new(cap: usize) -> Self {
        Self {
            queue: Arc::new(ArrayQueue::new(cap)),
            handle: None,
            num_fetches: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl<H: Handle> Drop for AllocatorPool<H> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
            while let Some(alloc) = self.queue.pop() {
                alloc.release();
            }
        }
    }
}

#[cfg(feature = "std")]
impl AllocatorPool<std::sync::mpsc::SyncSender<()>> {
    /// Creates a new pool with a thread will auto free idle allocators
    ///
    /// # Example
    ///
    /// ```
    /// use zallocator::pool::AllocatorPool;
    /// use std::time::Duration;
    ///
    /// let pool = AllocatorPool::with_spawn(2, Duration::from_millis(1000));
    /// let a = pool.fetch(1024, "test").unwrap();
    /// pool.put(a);
    /// ```
    pub fn with_spawn(cap: usize, idle_timeout: Duration) -> Self {
        let num_fetches = Arc::new(AtomicU64::new(0));
        let queue = Arc::new(ArrayQueue::new(cap));
        let (close_tx, close_rx) = std::sync::mpsc::sync_channel(1);

        FreeupProcessor::new(queue.clone(), idle_timeout, num_fetches.clone()).spawn(close_rx);
        Self {
            num_fetches,
            queue,
            handle: Some(close_tx),
        }
    }
}

impl<H: Handle> AllocatorPool<H> {
    /// Creates a new pool with a thread will auto free idle allocators
    ///
    /// # Example
    ///
    /// ```
    /// use zallocator::pool::AllocatorPool;
    /// use std::time::Duration;
    ///
    /// # #[cfg(feature = "tokio")]
    /// # tokio_test::block_on(async {
    /// let pool = AllocatorPool::with_task(2, Duration::from_millis(1000), tokio::spawn);
    /// let a = pool.fetch(1024, "test").unwrap();
    /// pool.put(a);
    /// # });
    /// ```
    #[inline]
    #[cfg(feature = "future")]
    #[cfg_attr(docsrs, doc(cfg(feature = "future")))]
    pub fn with_task<S>(cap: usize, idle_timeout: Duration, spawner: S) -> Self
    where
        S: Fn(Pin<Box<dyn Future<Output = ()> + Send>>) -> H,
    {
        let queue = Arc::new(ArrayQueue::new(cap));
        let num_fetches = Arc::new(AtomicU64::new(0));
        let h = FreeupProcessor::new(queue.clone(), idle_timeout, num_fetches.clone())
            .spawn_task(spawner);

        Self {
            num_fetches,
            queue,
            handle: Some(h),
        }
    }

    /// Creates a new pool with a thread will auto free idle allocators
    ///
    /// # Example
    /// ```
    /// use zallocator::pool::AllocatorPool;
    /// use std::time::Duration;
    ///
    /// # #[cfg(feature = "tokio")]
    /// # tokio_test::block_on(async {
    ///
    /// let local = tokio::task::LocalSet::new();
    ///
    /// local.run_until(async {
    ///     let pool = AllocatorPool::with_local_task(2, Duration::from_millis(1000), tokio::task::spawn_local);
    ///     let a = pool.fetch(1024, "test").unwrap();
    ///     pool.put(a);
    /// }).await;
    ///
    /// # });
    /// ```
    #[inline]
    #[cfg(feature = "future")]
    #[cfg_attr(docsrs, doc(cfg(feature = "future")))]
    pub fn with_local_task<S>(cap: usize, idle_timeout: Duration, local_spawner: S) -> Self
    where
        S: Fn(Pin<Box<dyn Future<Output = ()>>>) -> H,
    {
        let queue = Arc::new(ArrayQueue::new(cap));
        let num_fetches = Arc::new(AtomicU64::new(0));
        let h = FreeupProcessor::new(queue.clone(), idle_timeout, num_fetches.clone())
            .spawn_task_local(local_spawner);

        Self {
            num_fetches,
            queue,
            handle: Some(h),
        }
    }

    /// Try to fetch an allocator, if there is no idle allocator in pool,
    /// then the function will create a new allocator.
    ///
    /// # Example
    /// ```
    /// use zallocator::pool::AllocatorPool;
    /// use std::time::Duration;
    ///
    /// let pool = AllocatorPool::new(2);
    /// let a = pool.fetch(1024, "test").unwrap();
    /// ```
    pub fn fetch(&self, size: usize, tag: &'static str) -> super::Result<Allocator> {
        self.num_fetches.fetch_add(1, Ordering::AcqRel);
        if let Some(alloc) = self.queue.pop() {
            alloc.set_tag(tag);
            alloc.reset();
            Ok(alloc)
        } else {
            Allocator::new(size, tag)
        }
    }

    /// Put an allocator back into the pool.
    ///
    /// # Example
    /// ```
    /// use zallocator::pool::AllocatorPool;
    /// use std::time::Duration;
    ///
    /// let pool = AllocatorPool::new(2);
    /// let a = pool.fetch(1024, "test").unwrap();
    /// pool.put(a);
    /// ```
    pub fn put(&self, alloc: Allocator) {
        if !self.queue.is_full() && alloc.can_put_back() {
            if let Err(alloc) = self.queue.push(alloc) {
                alloc.release();
            }
        }
    }

    /// Returns how many idle allocators in the pool.
    ///
    /// # Example
    /// ```
    /// use zallocator::pool::AllocatorPool;
    /// use std::time::Duration;
    ///
    /// let pool = AllocatorPool::new(2);
    /// let a = pool.fetch(1024, "test").unwrap();
    /// pool.put(a);
    ///
    /// assert_eq!(pool.idles(), 1);
    /// ```
    #[inline]
    pub fn idles(&self) -> usize {
        self.queue.len()
    }
}

#[cfg(any(feature = "std", feature = "future"))]
struct FreeupProcessor {
    queue: Arc<ArrayQueue<Allocator>>,
    ticker: Duration,
    num_fetches: Arc<AtomicU64>,
}

#[cfg(any(feature = "std", feature = "future"))]
impl FreeupProcessor {
    fn new(
        queue: Arc<ArrayQueue<Allocator>>,
        ticker: Duration,
        num_fetches: Arc<AtomicU64>,
    ) -> FreeupProcessor {
        Self {
            queue,
            ticker,
            num_fetches,
        }
    }

    #[cfg(feature = "future")]
    fn spawn_task_local<S, H>(self, spawner: S) -> H
    where
        S: FnOnce(Pin<Box<dyn Future<Output = ()>>>) -> H,
    {
        (spawner)(Box::pin(async move {
            let mut last = 0;
            let delay = futures_timer::Delay::new(self.ticker);
            let mut delay = core::pin::pin!(delay);
            loop {
                (&mut delay).await;
                let fetches = self.num_fetches.load(Ordering::Acquire);
                if fetches != last {
                    // Some retrievals were made since the last time. So, let's avoid doing a release.
                    last = fetches;
                    delay.reset(self.ticker);
                    continue;
                }
                if let Some(a) = self.queue.pop() {
                    a.release();
                }
                delay.reset(self.ticker);
            }
        }))
    }

    #[cfg(feature = "future")]
    fn spawn_task<S, H>(self, spawner: S) -> H
    where
        S: FnOnce(Pin<Box<dyn Future<Output = ()> + Send>>) -> H,
    {
        (spawner)(Box::pin(async move {
            let mut last = 0;
            let delay = futures_timer::Delay::new(self.ticker);
            let mut delay = core::pin::pin!(delay);
            loop {
                (&mut delay).await;
                let fetches = self.num_fetches.load(Ordering::SeqCst);
                if fetches != last {
                    // Some retrievals were made since the last time. So, let's avoid doing a release.
                    last = fetches;
                    delay.reset(self.ticker);
                    continue;
                }
                if let Some(a) = self.queue.pop() {
                    a.release();
                }
                delay.reset(self.ticker);
            }
        }))
    }

    #[cfg(feature = "std")]
    fn spawn(self, close_rx: std::sync::mpsc::Receiver<()>) {
        crate::sealed::sync::spawn(move || {
            let mut last = 0;
            loop {
                std::thread::sleep(self.ticker);
                if close_rx.try_recv().is_ok() {
                    while let Some(a) = self.queue.pop() {
                        a.release();
                    }
                    return;
                }

                let fetches = self.num_fetches.load(Ordering::Acquire);
                if fetches != last {
                    // Some retrievals were made since the last time. So, let's avoid doing a release.
                    last = fetches;
                    continue;
                }

                if let Some(a) = self.queue.pop() {
                    a.release();
                }
            }
        });
    }
}

/// A handle is used to abort the background task, if any.
pub trait Handle {
    /// Abort the handle
    fn abort(self);
}

impl Handle for () {
    #[inline]
    fn abort(self) {}
}

#[cfg(feature = "std")]
impl Handle for std::sync::mpsc::SyncSender<()> {
    #[inline]
    fn abort(self) {
        let _ = self.send(());
    }
}

#[cfg(feature = "tokio")]
impl Handle for tokio::task::JoinHandle<()> {
    #[inline]
    fn abort(self) {
        tokio::task::JoinHandle::abort(&self);
    }
}

#[cfg(feature = "async-std")]
impl Handle for async_std::task::JoinHandle<()> {
    #[inline]
    fn abort(self) {
        async_std::task::block_on(async {
            async_std::task::JoinHandle::cancel(self).await;
        });
    }
}

#[cfg(feature = "smol")]
impl Handle for smol::Task<()> {
    #[inline]
    fn abort(self) {
        drop(self);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_allocator_pool() {
        let pool = AllocatorPool::new(2);
        let a = pool.fetch(1024, "a").unwrap();
        let b = pool.fetch(1024, "b").unwrap();
        let c = pool.fetch(1024, "c").unwrap();
        pool.put(a);
        pool.put(b);
        pool.put(c);

        assert_eq!(2, pool.idles());
    }

    #[cfg(any(feature = "tokio", feature = "async-std", feature = "smol"))]
    macro_rules! allocator_pool_with_spawn {
        ($run:ident($rt:ident::$spawn:ident($sleep:ty))) => {
            paste::paste! {
                #[test]
                #[cfg(feature = "future")]
                fn [< test_allocator_pool_with_spawn_ $rt >]() {
                    $run(async {
                        let pool =
                            AllocatorPool::with_task(2, core::time::Duration::from_millis(1000), $spawn);
                        let a = pool.fetch(1024, "a").unwrap();
                        let b = pool.fetch(1024, "b").unwrap();
                        pool.put(a);
                        pool.put(b);

                        assert_eq!(2, pool.idles());

                        pool.fetch(1024, "c").unwrap();
                        $sleep(core::time::Duration::from_millis(1000)).await;
                        assert_eq!(1, pool.idles());
                        $sleep(core::time::Duration::from_millis(2000)).await;
                        assert_eq!(0, pool.idles());
                    });
                }
            }
        };
    }

    #[cfg(feature = "tokio")]
    fn tokio_run(fut: impl core::future::Future<Output = ()> + Send) {
        tokio::runtime::Runtime::new().unwrap().block_on(fut)
    }

    #[cfg(feature = "tokio")]
    use tokio::spawn as tokio_spawn;

    #[cfg(feature = "tokio")]
    allocator_pool_with_spawn!(tokio_run(tokio::tokio_spawn(tokio::time::sleep)));

    #[cfg(feature = "smol")]
    use smol::{block_on as smol_run, spawn as smol_spawn};

    #[cfg(feature = "smol")]
    async fn smol_sleep(d: Duration) {
        smol::Timer::after(d).await;
    }

    #[cfg(feature = "smol")]
    allocator_pool_with_spawn!(smol_run(smol::smol_spawn(smol_sleep)));

    #[cfg(feature = "async-std")]
    use async_std::task::{
        block_on as async_std_run, sleep as async_std_sleep, spawn as async_std_spawn,
    };

    #[cfg(feature = "async-std")]
    allocator_pool_with_spawn!(async_std_run(async_std::async_std_spawn(async_std_sleep)));

    #[tokio::test]
    #[cfg(feature = "tokio")]
    async fn test_allocator_pool_with_spawn_local() {
        let local = tokio::task::LocalSet::new();
        let _ = local
            .run_until(async {
                let pool = AllocatorPool::with_local_task(
                    2,
                    core::time::Duration::from_millis(1000),
                    tokio::task::spawn_local,
                );
                let a = pool.fetch(1024, "a").unwrap();
                let b = pool.fetch(1024, "b").unwrap();
                pool.put(a);
                pool.put(b);

                assert_eq!(2, pool.idles());

                pool.fetch(1024, "c").unwrap();
                tokio::time::sleep(core::time::Duration::from_millis(1000)).await;
                assert_eq!(1, pool.idles());
                tokio::time::sleep(core::time::Duration::from_millis(2000)).await;
                assert_eq!(0, pool.idles());
            })
            .await;
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_allocator_pool_with_spawn() {
        let pool = AllocatorPool::with_spawn(2, core::time::Duration::from_millis(1000));
        let a = pool.fetch(1024, "a").unwrap();
        let b = pool.fetch(1024, "b").unwrap();
        pool.put(a);
        pool.put(b);

        assert_eq!(2, pool.idles());

        pool.fetch(1024, "c").unwrap();
        std::thread::sleep(core::time::Duration::from_millis(1000));
        assert_eq!(1, pool.idles());
        std::thread::sleep(core::time::Duration::from_millis(2000));
        assert_eq!(0, pool.idles());
    }
}
