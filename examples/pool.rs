use zallocator::pool::AllocatorPool;

fn main() {
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
