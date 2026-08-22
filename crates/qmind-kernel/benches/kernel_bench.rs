use criterion::{criterion_group, criterion_main, Criterion};
use qmind_kernel::page::PageHeader;

fn bench_header_encode(c: &mut Criterion) {
    let mut buf = vec![0u8; qmind_kernel::PAGE_SIZE];
    c.bench_function("page_header_encode", |b| {
        b.iter(|| PageHeader::new(0xDEAD_BEEF).encode_into(&mut buf))
    });
}

criterion_group!(benches, bench_header_encode);
criterion_main!(benches);
