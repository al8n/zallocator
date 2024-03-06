use zallocator::pool::AllocatorPool;

#[tokio::main]
async fn main() {
    let pool = AllocatorPool::with_task(2, tokio::time::Duration::from_millis(1000), tokio::spawn);
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
}
