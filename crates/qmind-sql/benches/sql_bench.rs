use criterion::{criterion_group, criterion_main, Criterion};
use qmind_sql::{Engine, SqlValue};

fn bench_insert_10k(c: &mut Criterion) {
    let mut group = c.benchmark_group("sql");
    group.throughput(criterion::Throughput::Elements(10_000));
    group.bench_function("insert_10k_rows", |b| {
        b.iter_batched(
            || {
                let mut eng = Engine::new(Vec::new());
                eng.execute("CREATE TABLE t (id INTEGER NOT NULL, val TEXT)")
                    .unwrap();
                eng
            },
            |mut eng| {
                for i in 0..10_000u64 {
                    eng.execute(&format!("INSERT INTO t VALUES ({i}, 'v{i}')"))
                        .unwrap();
                }
                eng
            },
            criterion::BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_select_where(c: &mut Criterion) {
    let mut group = c.benchmark_group("sql");
    group.throughput(criterion::Throughput::Elements(10_000));
    group.bench_function("select_where_10k", |b| {
        b.iter_batched(
            || {
                let mut eng = Engine::new(Vec::new());
                eng.execute("CREATE TABLE t (id INTEGER NOT NULL, val TEXT)")
                    .unwrap();
                for i in 0..10_000u64 {
                    eng.execute(&format!("INSERT INTO t VALUES ({i}, 'v{i}')"))
                        .unwrap();
                }
                eng
            },
            |mut eng| {
                let r = eng.execute("SELECT val FROM t WHERE id >= 5000").unwrap();
                assert_eq!(r.rows.len(), 5000);
                eng
            },
            criterion::BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_select_count_star(c: &mut Criterion) {
    let mut group = c.benchmark_group("sql");
    group.throughput(criterion::Throughput::Elements(10_000));
    group.bench_function("count_star_10k", |b| {
        b.iter_batched(
            || {
                let mut eng = Engine::new(Vec::new());
                eng.execute("CREATE TABLE t (id INTEGER NOT NULL, val TEXT)")
                    .unwrap();
                for i in 0..10_000u64 {
                    eng.execute(&format!("INSERT INTO t VALUES ({i}, 'v{i}')"))
                        .unwrap();
                }
                eng
            },
            |mut eng| {
                let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
                assert_eq!(r.rows[0][0], SqlValue::Int(10_000));
                eng
            },
            criterion::BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_group_by(c: &mut Criterion) {
    let mut group = c.benchmark_group("sql");
    group.throughput(criterion::Throughput::Elements(10_000));
    group.bench_function("group_by_10k", |b| {
        b.iter_batched(
            || {
                let mut eng = Engine::new(Vec::new());
                eng.execute("CREATE TABLE t (id INTEGER NOT NULL, cat TEXT, v INTEGER)")
                    .unwrap();
                for i in 0..10_000u64 {
                    eng.execute(&format!(
                        "INSERT INTO t VALUES ({i}, 'cat{}', {})",
                        i % 20,
                        i % 100
                    ))
                    .unwrap();
                }
                eng
            },
            |mut eng| {
                let r = eng
                    .execute("SELECT cat, COUNT(*), SUM(v) FROM t GROUP BY cat")
                    .unwrap();
                assert_eq!(r.rows.len(), 20);
                eng
            },
            criterion::BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_join(c: &mut Criterion) {
    let mut group = c.benchmark_group("sql");
    group.throughput(criterion::Throughput::Elements(1_000));
    group.bench_function("inner_join_1k_x_100", |b| {
        b.iter_batched(
            || {
                let mut eng = Engine::new(Vec::new());
                eng.execute("CREATE TABLE a (id INTEGER NOT NULL, val INTEGER NOT NULL)")
                    .unwrap();
                eng.execute("CREATE TABLE b (aid INTEGER NOT NULL, amt INTEGER NOT NULL)")
                    .unwrap();
                for i in 0..1_000u64 {
                    eng.execute(&format!("INSERT INTO a VALUES ({i}, {})", i % 10))
                        .unwrap();
                }
                for i in 0..100u64 {
                    eng.execute(&format!("INSERT INTO b VALUES ({i}, {})", i * 5))
                        .unwrap();
                }
                eng
            },
            |mut eng| {
                let r = eng
                    .execute("SELECT val, amt FROM a INNER JOIN b ON id = aid")
                    .unwrap();
                assert!(!r.rows.is_empty());
                eng
            },
            criterion::BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("sql");
    group.throughput(criterion::Throughput::Elements(1));
    group.bench_function("parse_select_complex", |b| {
        b.iter(|| {
            qmind_sql::parser::Parser::parse(
                "SELECT name, COUNT(*), SUM(amount) FROM users \
                 INNER JOIN orders ON id = uid \
                 WHERE amount > 100 AND name != 'admin' \
                 GROUP BY name LIMIT 50",
            )
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_insert_10k,
    bench_select_where,
    bench_select_count_star,
    bench_group_by,
    bench_join,
    bench_parse,
);
criterion_main!(benches);
