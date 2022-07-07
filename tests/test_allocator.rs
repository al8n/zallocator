// extern crate alloc;

// use rand::Rng;
// use zallocator::Zallocator;

// #[test]
// fn test_allocate() {
//     let a = Zallocator::new(1024, "test allocate").unwrap();

//     fn check(a: &Zallocator) {
//         assert_eq!(0, a.allocate(0).unwrap().len());
//         assert_eq!(1, a.allocate(1).unwrap().len());
//         assert_eq!((1 << 20) + 1, a.allocate((1 << 20) + 1).unwrap().len());
//         assert_eq!(256 << 20, a.allocate(256 << 20).unwrap().len());
//         assert!(a.allocate(Zallocator::MAX_ALLOC + 1).is_err());
//     }

//     check(&a);
//     let prev = a.allocated();
//     a.reset();
//     check(&a);
//     assert_eq!(prev, a.allocated());
//     assert!(prev >= 1 + (1 << (20 + 1)) + (256 << 20));
//     a.release();
// }

// #[test]
// fn test_allocate_aligned() {
//     // let a = Zallocator::new(1024, "test allocate aligned").unwrap();

//     // a.allocate(1).unwrap();
//     // let out = a.allocate(1).unwrap();
//     // let ptr = (&out[0] as *const u8) as u64;
//     // assert_eq!(ptr % 8, 1);

//     // let out = a.allocate_aligned(5).unwrap();
//     // let ptr = (&out[0] as *const u8) as u64;
//     // assert_eq!(ptr % 8, 0);

//     // let out = a.allocate_aligned(3).unwrap();
//     // let ptr = (&out[0] as *const u8) as u64;
//     // assert_eq!(ptr % 8, 0);
//     // a.release();
// }

// #[test]
// fn test_allocate_reset() {
//     let a = Zallocator::new(1024, "test allocate reset").unwrap();

//     let mut buf = [0u8; 128];
//     let mut rng = rand::thread_rng();
//     rng.fill(&mut buf[..]);
//     (0..1000).for_each(|_| {
//         a.read(&buf).unwrap();
//     });

//     let prev = a.allocated();
//     a.reset();
//     (0..100).for_each(|_| {
//         a.read(&buf).unwrap();
//     });

//     assert_eq!(prev, a.allocated());
//     a.release();
// }

// #[test]
// fn test_allocate_truncate() {
//     let a = Zallocator::new(16, "test allocate truncate").unwrap();
//     let mut buf = [0u8; 128];
//     let mut rng = rand::thread_rng();
//     rng.fill(&mut buf[..]);
//     (0..1000).for_each(|_| {
//         a.read(&buf).unwrap();
//     });

//     const N: u64 = 2048;
//     a.truncate(N);
//     assert!(a.allocated() <= N);
//     a.release();
// }
