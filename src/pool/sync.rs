use super::Allocator;
use crate::sealed::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use core::time::Duration;
use crossbeam_channel::{bounded, select, Receiver, Sender};

/// # Introduction
/// Lock-free allocator pool.
///
/// When fetching an allocator, if there is no idle allocator in pool,
/// then will create a new allocator. Otherwise, it reuses the idle allocator.
///
/// # Examples
/// ## Manually Free
/// By default, the pool will not free idle allocators in pool.
///
/// ```
/// use zallocator::pool::AllocatorPool;
///
/// let pool = AllocatorPool::new(2);
/// let a = pool.fetch(1024, "a").unwrap();
/// let b = pool.fetch(1024, "b").unwrap();
/// let c = pool.fetch(1024, "c").unwrap();
/// pool.put(a);
/// pool.put(b);
///
/// // c will be freed directly, because the pool is full.
/// pool.put(c);
/// assert_eq!(2, pool.idle_allocators());
/// ```
///
/// ## Auto Free
/// Auto free idle allocators in pool, this will spawn a new thread (by `std::thread::spawn`) when constructing the pool.
/// ```
/// use zallocator::pool::AllocatorPool;
/// use std::time::Duration;
/// use std::thread::sleep;
///
/// let pool = AllocatorPool::with_free(2, core::time::Duration::from_millis(1000));
/// let a = pool.fetch(1024, "a").unwrap();
/// let b = pool.fetch(1024, "b").unwrap();
/// pool.put(a);
/// pool.put(b);
///
/// assert_eq!(2, pool.idle_allocators());
/// pool.fetch(1024, "c").unwrap();
///
/// sleep(core::time::Duration::from_millis(1000));
/// assert_eq!(1, pool.idle_allocators());
///
/// sleep(core::time::Duration::from_millis(1200));
/// assert_eq!(0, pool.idle_allocators());
/// ```
///
pub struct AllocatorPool {
    num_fetches: Arc<AtomicU64>,
    inner: Inner,
    close_tx: Option<Sender<()>>,
}

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

impl AllocatorPool {
    /// Creates a new pool without auto free idle allocators
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
    /// use zallocator::pool::AllocatorPool;
    /// use std::time::Duration;
    ///
    /// let pool = AllocatorPool::with_free(2, Duration::from_millis(1000));
    /// let a = pool.fetch(1024, "test").unwrap();
    /// pool.put(a);
    /// ```
    pub fn with_free(cap: usize, idle_timeout: Duration) -> Self {
        let inner = Inner::new(cap);
        let num_fetches = Arc::new(AtomicU64::new(0));
        let (close_tx, close_rx) = bounded(1);

        FreeupProcessor::new(
            inner.alloc_rx.clone(),
            close_rx,
            idle_timeout,
            num_fetches.clone(),
        )
        .spawn();
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
    /// use zallocator::pool::AllocatorPool;
    /// use std::time::Duration;
    ///
    /// let pool = AllocatorPool::new(2);
    /// let a = pool.fetch(1024, "test").unwrap();
    /// ```
    pub fn fetch(&self, size: usize, tag: &'static str) -> super::Result<Allocator> {
        self.num_fetches.fetch_add(1, Ordering::Relaxed);
        select! {
            recv(self.inner.alloc_rx) -> msg => msg.map(|mut a| {
                a.reset();
                a.set_tag(tag);
                a
            }).or_else(|_| Allocator::new(size, tag)),
            default => Allocator::new(size, tag),
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
        if !self.inner.alloc_tx.is_full() {
            if let Err(e) = self.inner.alloc_tx.send(alloc) {
                e.into_inner().release();
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
    /// assert_eq!(pool.idle_allocators(), 1);
    /// ```
    #[inline]
    pub fn idle_allocators(&self) -> usize {
        self.inner.alloc_rx.len()
    }
}

impl Drop for AllocatorPool {
    fn drop(&mut self) {
        if let Some(close_tx) = &self.close_tx {
            let _ = close_tx.send(());
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

    fn spawn(self) {
        std::thread::spawn(move || {
            let mut last = 0;
            loop {
                select! {
                    recv(self.close_rx) -> _ => {
                        for a in self.rx {
                            a.release();
                        }
                        return;
                    }
                    default(self.ticker) => {
                        let fetches = self.num_fetches.load(Ordering::SeqCst);
                        if fetches != last {
                            // Some retrievals were made since the last time. So, let's avoid doing a release.
                            last = fetches;
                            continue;
                        }
                        select! {
                            recv(self.rx) -> msg => {
                                if let Ok(a) = msg {
                                    a.release();
                                }
                            }
                        }
                    }
                }
            }
        });
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
        assert_eq!(2, pool.idle_allocators());
    }

    #[test]
    fn test_allocator_pool_with_free() {
        let pool = AllocatorPool::with_free(2, core::time::Duration::from_millis(1000));
        let a = pool.fetch(1024, "a").unwrap();
        let b = pool.fetch(1024, "b").unwrap();
        pool.put(a);
        pool.put(b);

        assert_eq!(2, pool.idle_allocators());

        pool.fetch(1024, "c").unwrap();
        std::thread::sleep(core::time::Duration::from_millis(1000));
        assert_eq!(1, pool.idle_allocators());
        std::thread::sleep(core::time::Duration::from_millis(2000));
        assert_eq!(0, pool.idle_allocators());
    }
}
