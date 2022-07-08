use zallocator::pool::AsyncAllocatorPool;

#[tokio::main]
async fn main() {
    let pool =
        AsyncAllocatorPool::with_free(2, tokio::time::Duration::from_millis(1000), tokio::spawn);
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
