use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use zallocator::Zallocator;

fn benches_allocator(c: &mut Criterion) {
    let a = Zallocator::new(15, "test").unwrap();
    c.bench_function("allocator allocate", |b| {
        b.iter_batched(
            || a.clone(),
            |a| {
                a.allocate(1).unwrap();
            },
            BatchSize::LargeInput,
        )
    });
}

criterion_group! {
    benches,
    benches_allocator,
}

criterion_main!(benches);
