use super::Allocator;
use crate::sealed::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use async_channel::{bounded, Receiver, Sender};
use core::time::Duration;
use futures::{
    future::{BoxFuture, FutureExt},
    stream::StreamExt,
};
use pollster::FutureExt as _;

/// # Introduction
/// Lock-free and runtime agnostic async allocator pool.
/// When fetching an allocator, if there is no idle allocator in pool,
/// then will create a new allocator. Otherwise, it reuses the idle allocator.
///
/// # Examples
/// ## Manually Free
/// By default, the pool will not free idle allocators in pool.
///
/// ```
/// use zallocator::pool::AsyncAllocatorPool;
///
/// # tokio_test::block_on(async {
/// let pool = AsyncAllocatorPool::new(2);
/// let a = pool.fetch(1024, "a").await.unwrap();
/// let b = pool.fetch(1024, "b").await.unwrap();
/// let c = pool.fetch(1024, "c").await.unwrap();
///
/// pool.put(a).await;
/// pool.put(b).await;
///
/// // c will be freed directly, because the pool is full.
/// pool.put(c).await;
/// assert_eq!(2, pool.idle_allocators());
///
/// # });
/// ```
///
/// ## Auto Free
/// Auto free idle allocators in pool, this will spawn a new thread (by any async runtime spawner, this example use `tokio`) when constructing the pool.
/// ```
/// use zallocator::pool::AsyncAllocatorPool;
/// use tokio::time::{sleep, Duration};
///
/// # tokio_test::block_on(async {
/// let pool = AsyncAllocatorPool::with_free(2, core::time::Duration::from_millis(1000), tokio::spawn);
/// let a = pool.fetch(1024, "a").await.unwrap();
/// let b = pool.fetch(1024, "b").await.unwrap();
/// pool.put(a).await;
/// pool.put(b).await;
///
/// assert_eq!(2, pool.idle_allocators());
/// pool.fetch(1024, "c").await.unwrap();
///
/// sleep(Duration::from_millis(1000)).await;
/// assert_eq!(1, pool.idle_allocators());
///
/// sleep(Duration::from_millis(2000)).await;
/// assert_eq!(0, pool.idle_allocators());
/// # });
/// ```
///
#[derive(Debug)]
pub struct AsyncAllocatorPool {
    num_fetches: Arc<AtomicU64>,
    inner: Inner,
    close_tx: Option<Sender<()>>,
}

#[derive(Debug)]
struct Inner {
    alloc_tx: Sender<Allocator>,
    alloc_rx: Receiver<Allocator>,
}

impl Inner {
    #[inline]
    fn new(cap: usize) -> Self {
        let (alloc_tx, alloc_rx) = bounded(cap);
        Self { alloc_tx, alloc_rx }
    }
}

impl AsyncAllocatorPool {
    /// Creates a new pool without auto free idle allocators
    ///
    /// # Example
    /// ```
    /// use zallocator::pool::AsyncAllocatorPool;
    /// use std::time::Duration;
    ///
    /// # tokio_test::block_on(async {
    /// let pool = AsyncAllocatorPool::new(2);
    /// let a = pool.fetch(1024, "test").await.unwrap();
    /// pool.put(a).await;
    /// # });
    /// ```
    #[cfg(all(
        feature = "async-channel",
        feature = "pollster",
        feature = "futures",
        feature = "async-io"
    ))]
    #[inline]
    pub fn new(cap: usize) -> Self {
        Self {
            num_fetches: Arc::new(AtomicU64::new(0)),
            inner: Inner::new(cap),
            close_tx: None,
        }
    }

    /// Creates a new pool with a thread will auto free idle allocators
    ///
    /// # Example
    /// ```
    /// use zallocator::pool::AsyncAllocatorPool;
    /// use std::time::Duration;
    ///
    /// # tokio_test::block_on(async {
    /// let pool = AsyncAllocatorPool::with_free(2, Duration::from_millis(1000), tokio::spawn);
    /// let a = pool.fetch(1024, "test").await.unwrap();
    /// pool.put(a).await;
    /// # });
    /// ```
    #[inline]
    pub fn with_free<S, R>(cap: usize, idle_timeout: Duration, spawner: S) -> Self
    where
        S: Fn(BoxFuture<'static, ()>) -> R + Send + Sync + 'static,
    {
        let inner = Inner::new(cap);
        let num_fetches = Arc::new(AtomicU64::new(0));
        let (close_tx, close_rx) = bounded(1);

        FreeupProcessor::new(
            inner.alloc_rx.clone(),
            close_rx,
            idle_timeout,
            num_fetches.clone(),
        )
        .spawn(Box::new(move |fut| {
            spawner(fut);
        }));
        Self {
            num_fetches,
            inner,
            close_tx: Some(close_tx),
        }
    }

    /// Try to fetch an allocator, if there is no idle allocator in pool,
    /// then the function will create a new allocator.
    ///
    /// # Example
    /// ```
    /// use zallocator::pool::AsyncAllocatorPool;
    /// use std::time::Duration;
    ///
    /// # tokio_test::block_on(async {
    /// let pool = AsyncAllocatorPool::new(2);
    /// let a = pool.fetch(1024, "test").await.unwrap();
    /// # });
    /// ```
    pub async fn fetch(&self, size: usize, tag: &'static str) -> super::Result<Allocator> {
        self.num_fetches.fetch_add(1, Ordering::Relaxed);
        futures::select! {
            msg = self.inner.alloc_rx.recv().fuse() => {
                msg.map(|a| {
                    a.reset();
                    a.set_tag(tag);
                    a
                }).or_else(|_| Allocator::new(size, tag))
            },
            default => Allocator::new(size, tag),
        }
    }

    /// Put an allocator back into the pool.
    ///
    /// # Example
    /// ```
    /// use zallocator::pool::AsyncAllocatorPool;
    /// use std::time::Duration;
    ///
    /// # tokio_test::block_on(async {
    /// let pool = AsyncAllocatorPool::new(2);
    /// let a = pool.fetch(1024, "test").await.unwrap();
    /// pool.put(a).await;
    /// # });
    /// ```
    pub async fn put(&self, alloc: Allocator) {
        if !self.inner.alloc_tx.is_full() && alloc.can_put_back() {
            if let Err(e) = self.inner.alloc_tx.send(alloc).await {
                e.into_inner().release();
            }
        }
    }

    /// Returns how many idle allocators in the pool.
    ///
    /// # Example
    /// ```
    /// use zallocator::pool::AsyncAllocatorPool;
    /// use std::time::Duration;
    ///
    /// # tokio_test::block_on(async {
    /// let pool = AsyncAllocatorPool::new(2);
    /// let a = pool.fetch(1024, "test").await.unwrap();
    /// pool.put(a).await;
    ///
    /// assert_eq!(pool.idle_allocators(), 1);
    /// # });
    /// ```
    #[inline]
    pub fn idle_allocators(&self) -> usize {
        self.inner.alloc_rx.len()
    }
}

impl Drop for AsyncAllocatorPool {
    fn drop(&mut self) {
        if self.close_tx.is_some() {
            if let Some(sender) = &self.close_tx {
                let _ = sender.send(()).block_on();
            }
        }
    }
}

struct FreeupProcessor {
    rx: Receiver<Allocator>,
    close_rx: Receiver<()>,
    ticker: Duration,
    num_fetches: Arc<AtomicU64>,
}

impl FreeupProcessor {
    fn new(
        rx: Receiver<Allocator>,
        close_rx: Receiver<()>,
        ticker: Duration,
        num_fetches: Arc<AtomicU64>,
    ) -> FreeupProcessor {
        Self {
            rx,
            close_rx,
            ticker,
            num_fetches,
        }
    }

    fn spawn(mut self, spawner: Box<dyn Fn(BoxFuture<'static, ()>) + Send + Sync>) {
        (spawner)(Box::pin(async move {
            let mut last = 0;
            let mut interval = async_io::Timer::interval(self.ticker);
            loop {
                futures::select! {
                    _ = self.close_rx.recv().fuse() => {
                        while let Some(a) = self.rx.next().await {
                            a.release();
                        }
                        return;
                    },
                    _ = interval.next().fuse() => {
                        let fetches = self.num_fetches.load(Ordering::SeqCst);
                        if fetches != last {
                            // Some retrievals were made since the last time. So, let's avoid doing a release.
                            last = fetches;
                            continue;
                        }
                        if let Ok(a) = self.rx.recv().await {
                            a.release();
                        }
                    }
                }
            }
        }));
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_allocator_pool() {
        let pool = AsyncAllocatorPool::new(2);
        let a = pool.fetch(1024, "a").await.unwrap();
        let b = pool.fetch(1024, "b").await.unwrap();
        let c = pool.fetch(1024, "c").await.unwrap();
        pool.put(a).await;
        pool.put(b).await;
        pool.put(c).await;

        #[cfg(all(
            feature = "async-channel",
            feature = "pollster",
            feature = "futures",
            feature = "async-io"
        ))]
        assert_eq!(2, pool.idle_allocators());
    }

    #[tokio::test]
    async fn test_allocator_pool_with_free() {
        let pool =
            AsyncAllocatorPool::with_free(2, core::time::Duration::from_millis(1000), tokio::spawn);
        let a = pool.fetch(1024, "a").await.unwrap();
        let b = pool.fetch(1024, "b").await.unwrap();
        pool.put(a).await;
        pool.put(b).await;

        assert_eq!(2, pool.idle_allocators());

        pool.fetch(1024, "c").await.unwrap();
        tokio::time::sleep(core::time::Duration::from_millis(1000)).await;
        assert_eq!(1, pool.idle_allocators());
        tokio::time::sleep(core::time::Duration::from_millis(2000)).await;
        assert_eq!(0, pool.idle_allocators());
    }
}
