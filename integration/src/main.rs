extern crate alloc;

use futures::future::join_all;
use spin::Mutex;
use zallocator::{release_all, Zallocator};

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() {
    let a = Zallocator::new(63, "test").unwrap();
    const N: u64 = 10240;
    const M: u64 = 16;

    let m = alloc::sync::Arc::new(Mutex::new(hashbrown::HashSet::<usize>::new()));
    let futures = (0..M)
        .map(|_| {
            let m = m.clone();
            let a = a.clone();
            tokio::spawn(async move {
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
            })
        })
        .collect::<Vec<_>>();

    join_all(futures).await;

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
