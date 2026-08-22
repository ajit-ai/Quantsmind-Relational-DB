use criterion::{criterion_group, criterion_main, Criterion};
use qmind_kernel::page::PageHeader;
use qmind_kernel::wal::WalRecord;
use qmind_kernel::BTree;

fn bench_page_header_encode(c: &mut Criterion) {
    let mut buf = vec![0u8; qmind_kernel::PAGE_SIZE];
    c.bench_function("page_header_encode", |b| {
        b.iter(|| PageHeader::new(0xDEAD_BEEF).encode_into(&mut buf))
    });
}

/// Contract (docs/ROADMAP.md M1): >= 500K inserts/s single-threaded.
fn bench_btree_insert_100k(c: &mut Criterion) {
    let mut group = c.benchmark_group("btree");
    group.throughput(criterion::Throughput::Elements(100_000));
    group.bench_function("insert_100k_sequential_u64", |b| {
        b.iter(|| {
            let mut t = BTree::new();
            for i in 0..100_000u64 {
                t.insert(&i.to_be_bytes(), i);
            }
            t
        })
    });
    group.finish();
}

fn bench_btree_get_hot(c: &mut Criterion) {
    let mut t = BTree::new();
    for i in 0..100_000u64 {
        t.insert(&i.to_be_bytes(), i);
    }
    let mut i = 0u64;
    c.bench_function("btree/get_hot_10k", |b| {
        b.iter(|| {
            for _ in 0..10_000 {
                let k = i % 100_000;
                std::hint::black_box(t.get(&k.to_be_bytes()));
                i = i.wrapping_add(7919); // prime stride, stays cache-hostile
            }
        })
    });
}

fn bench_wal_group_commit(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal");
    group.throughput(criterion::Throughput::Elements(1_000));
    group.bench_function("append_1k_commit_one_group", |b| {
        b.iter(|| {
            let mut w = qmind_kernel::WalWriter::new(Vec::new());
            for i in 0..1000u64 {
                w.append(&WalRecord::Insert {
                    txn: i,
                    page: i,
                    slot: 0,
                });
            }
            w.commit_group().unwrap()
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_page_header_encode,
    bench_btree_insert_100k,
    bench_btree_get_hot,
    bench_wal_group_commit
);
criterion_main!(benches);
