use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use zallocator::Zallocator;

fn benches_allocator(c: &mut Criterion) {
    let a = Zallocator::new(15, "test").unwrap();
    c.bench_function("allocate", |b| {
        b.iter_batched(
            || a.clone(),
            |a| {
                let buf = a.allocate(1).unwrap();
                assert_eq!(buf.len(), 1);
            },
            BatchSize::LargeInput,
        )
    });

    eprintln!("{a}");
}

criterion_group! {
    benches,
    benches_allocator
}

criterion_main!(benches);
