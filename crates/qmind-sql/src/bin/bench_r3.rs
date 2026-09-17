//! R3 reproducible benchmark harness (macro/workload-level).
//!
//! Drives a real file-backed database through the workload shapes described in
//! `benchmarks/README.md` and `benchmarks/R3_HARNESS.md` at a configurable
//! scale, and writes a Markdown report into `benchmarks/results/r3/`.
//!
//! Prefer running through `benchmarks/scripts/run_r3.ps1` (or `run_r3.sh`),
//! which records the machine/environment block (commit, Rust, CPU, RAM) so
//! every report is reproducible and self-describing. Direct invocation:
//!
//! ```text
//! cargo run -p qmind-sql --release --bin bench_r3 -- --rows 100000
//! ```
//!
//! Dataset sizes are configurable (1_000 .. 10_000_000+). Results are real
//! measurements only. Workload categories the engine cannot execute today are
//! reported as `not measured` rather than invented; the execution path used
//! for each row is recorded explicitly.

use std::path::{Path, PathBuf};
use std::time::Instant;

use qmind_sql::Engine;

const DEFAULT_ROWS: u64 = 100_000;
const DEFAULT_ITERATIONS: u32 = 3;
const DEFAULT_CHUNK: u64 = 500;

struct Args {
    rows: u64,
    iterations: u32,
    chunk: u64,
    out: PathBuf,
    keep: bool,
}

fn parse_args() -> Args {
    let mut rows = DEFAULT_ROWS;
    let mut iterations = DEFAULT_ITERATIONS;
    let mut chunk = DEFAULT_CHUNK;
    let mut out = PathBuf::from("benchmarks/results/r3");
    let mut keep = false;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--rows" => {
                rows = it
                    .next()
                    .expect("--rows requires a value")
                    .parse()
                    .expect("--rows must be an integer")
            }
            "--iterations" => {
                iterations = it
                    .next()
                    .expect("--iterations requires a value")
                    .parse()
                    .expect("--iterations must be an integer")
            }
            "--chunk" => {
                chunk = it
                    .next()
                    .expect("--chunk requires a value")
                    .parse()
                    .expect("--chunk must be an integer")
            }
            "--out" => out = PathBuf::from(it.next().expect("--out requires a value")),
            "--keep" => keep = true,
            "--help" | "-h" => {
                println!("bench_r3 --rows <n> --iterations <k> --chunk <n> --out <dir> [--keep]");
                std::process::exit(0);
            }
            other => panic!("bench_r3: unknown argument {other}"),
        }
    }
    if rows == 0 {
        panic!("bench_r3: --rows must be positive");
    }
    if chunk == 0 {
        panic!("bench_r3: --chunk must be positive");
    }
    Args {
        rows,
        iterations,
        chunk,
        out,
        keep,
    }
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}

fn rows_per_s(rows: u64, ms: u64) -> f64 {
    if ms == 0 {
        0.0
    } else {
        rows as f64 / (ms as f64 / 1000.0)
    }
}

fn format_rate(r: f64) -> String {
    if r >= 1_000_000.0 {
        format!("{:.2}M", r / 1_000_000.0)
    } else if r >= 1_000.0 {
        format!("{:.2}K", r / 1_000.0)
    } else {
        format!("{:.2}", r)
    }
}

struct Entry {
    name: &'static str,
    rows: u64,
    ms_mean: u64,
    rows_per_s: f64,
    note: &'static str,
}

struct Report {
    rows: u64,
    iterations: u32,
    chunk: u64,
    entries: Vec<Entry>,
}

impl Report {
    fn new(rows: u64, iterations: u32, chunk: u64) -> Report {
        Report {
            rows,
            iterations,
            chunk,
            entries: Vec::new(),
        }
    }

    fn add(
        &mut self,
        name: &'static str,
        rows: u64,
        ms_total: u64,
        iters: u32,
        note: &'static str,
    ) {
        let ms_mean = ms_total / u64::from(iters.max(1));
        let rate = rows_per_s(rows, ms_mean);
        self.entries.push(Entry {
            name,
            rows,
            ms_mean,
            rows_per_s: rate,
            note,
        });
    }

    fn render(&self) -> String {
        let mut s = String::new();
        s.push_str("# R3 Benchmark Report\n\n");
        s.push_str(&format!("- dataset rows: **{}**\n", self.rows));
        s.push_str(&format!("- iterations per step: {}\n", self.iterations));
        s.push_str(&format!(
            "- insert chunk: {} rows/statement\n- seed: fixed (deterministic LCG)\n",
            self.chunk
        ));
        s.push_str(&format!(
            "- engine: quantsmind v{}\n",
            qmind_sql::ENGINE.version
        ));
        s.push('\n');

        s.push_str("## Environment\n\n");
        s.push_str("| field | value |\n|---|---|\n");
        s.push_str(&format!(
            "| commit | {} |\n",
            env_or("R3_BENCH_COMMIT", "n/a")
        ));
        s.push_str(&format!(
            "| rustc | {} |\n",
            env_or("R3_BENCH_RUSTC", "n/a")
        ));
        s.push_str(&format!("| cpu | {} |\n", env_or("R3_BENCH_CPU", "n/a")));
        s.push_str(&format!("| ram | {} |\n", env_or("R3_BENCH_RAM", "n/a")));
        s.push_str(&format!(
            "| os | {} ({}) |\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        s.push_str(&format!(
            "| build mode | {} |\n",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        ));
        s.push('\n');

        s.push_str("## Measured workloads\n\n");
        s.push_str("| workload | rows processed | mean wall (ms) | rows/s | execution path |\n");
        s.push_str("|---|---|---|---|---|\n");
        for e in &self.entries {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                e.name,
                e.rows,
                e.ms_mean,
                format_rate(e.rows_per_s),
                e.note
            ));
        }
        s.push('\n');

        s.push_str("## Honesty notes\n\n");
        s.push_str("- Aggregation and GROUP BY run through the **batch aggregate** pipeline\n");
        s.push_str("  (`R3-EXEC-1`): the scan is read from persistent storage and groups are\n");
        s.push_str("  materialized in memory (external spill is deferred).\n");
        s.push_str("- JOIN runs through the **batch hash join** pipeline (`R3-EXEC-2`): both\n");
        s.push_str(
            "  inputs are materialized from persistent storage and the right (build) side\n",
        );
        s.push_str("  is hashed; memory is O(left+right), spill is deferred.\n");
        s.push_str(
            "- Full scan and filtered scan are streaming (batch size 2048) and bounded-memory.\n",
        );
        s.push_str(
            "- ORDER BY uses the materialized fallback (the streaming path sorts in memory).\n",
        );
        s.push_str("- Insert throughput is fsync-per-commit (each statement commits its own WAL\n");
        s.push_str("  group); there is no batching across statements yet.\n");
        s
    }

    fn write(&self, out: &Path, name: &str) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(out)?;
        let path = out.join(format!("report-{name}.md"));
        std::fs::write(&path, self.render())?;
        Ok(path)
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

fn scan_iter(eng: &mut Engine<std::fs::File>, sql: &str, iterations: u32) -> (u64, u64) {
    let mut rows = 0u64;
    let start = Instant::now();
    for _ in 0..iterations {
        let mut n = 0u64;
        eng.stream_query(sql, |b| {
            n += b.num_rows() as u64;
            Ok(())
        })
        .expect("stream_query");
        rows = n;
    }
    (rows, elapsed_ms(start))
}

fn bench(eng: &mut Engine<std::fs::File>, args: &Args, report: &mut Report) {
    let iters = args.iterations;

    eng.execute(
        "CREATE TABLE ticks (id INTEGER NOT NULL, sym TEXT NOT NULL, val INTEGER NOT NULL, g INTEGER NOT NULL)",
    )
    .expect("create ticks");
    eng.execute("CREATE TABLE dim (dg INTEGER NOT NULL, label TEXT NOT NULL)")
        .expect("create dim");
    for g in 0..4 {
        eng.execute(&format!("INSERT INTO dim VALUES ({g}, 'grp_{g}')"))
            .expect("insert dim");
    }

    let start = Instant::now();
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut i: u64 = 0;
    while i < args.rows {
        let mut sql = String::from("INSERT INTO ticks VALUES ");
        let end = (i + args.chunk).min(args.rows);
        while i < end {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let val = (seed >> 33) % 1_000_000;
            let sym = i % 1000;
            sql.push_str(&format!(
                "({}, 's{:03}', {}, {}),",
                i,
                sym,
                val as i64,
                i % 4
            ));
            i += 1;
        }
        sql.pop();
        eng.execute(&sql).expect("bulk insert chunk");
    }
    report.add(
        "bulk_insert",
        args.rows,
        elapsed_ms(start),
        1,
        "fsync-per-statement",
    );

    let (rows, ms) = scan_iter(eng, "SELECT id FROM ticks", iters);
    report.add("wf_full_scan", rows, ms, iters, "streaming, bounded");

    let (rows, ms) = scan_iter(eng, "SELECT id FROM ticks WHERE id % 10 = 0", iters);
    report.add("wf_filtered_scan", rows, ms, iters, "streaming, bounded");

    let (rows, ms) = scan_iter(eng, "SELECT id, sym, val FROM ticks", iters);
    report.add("wf_projection", rows, ms, iters, "streaming, bounded");

    let (rows, ms) = scan_iter(eng, "SELECT id FROM ticks LIMIT 1000", iters);
    report.add("wf_limit", rows, ms, iters, "streaming, bounded");

    let (rows, ms) = scan_iter(eng, "SELECT id FROM ticks ORDER BY val LIMIT 100", iters);
    report.add("wf_order_small", rows, ms, iters, "materialized fallback");

    let (rows, ms) = scan_iter(eng, "SELECT id FROM ticks ORDER BY val", iters);
    report.add("wf_order_full", rows, ms, iters, "materialized fallback");

    aggs(eng, args, report);
    join_iter(eng, args, report);
}

fn aggs(eng: &mut Engine<std::fs::File>, args: &Args, report: &mut Report) {
    let iters = args.iterations;

    let (rows, ms) = scan_iter(eng, "SELECT COUNT(*) FROM ticks", iters);
    report.add(
        "wf_agg_count",
        rows,
        ms,
        iters,
        "streaming (batch aggregate)",
    );

    let (rows, ms) = scan_iter(eng, "SELECT SUM(val) FROM ticks", iters);
    report.add("wf_agg_sum", rows, ms, iters, "streaming (batch aggregate)");

    let (rows, ms) = scan_iter(eng, "SELECT AVG(val) FROM ticks", iters);
    report.add("wf_agg_avg", rows, ms, iters, "streaming (batch aggregate)");

    let (rows, ms) = scan_iter(eng, "SELECT g, COUNT(*) FROM ticks GROUP BY g", iters);
    report.add(
        "wf_group_by",
        rows,
        ms,
        iters,
        "streaming (batch aggregate)",
    );
}

fn join_iter(eng: &mut Engine<std::fs::File>, args: &Args, report: &mut Report) {
    let (rows, ms) = scan_iter(
        eng,
        "SELECT id FROM ticks JOIN dim ON g = dg",
        args.iterations,
    );
    report.add(
        "wf_join",
        rows,
        ms,
        args.iterations,
        "streaming (batch hash join, materialized build)",
    );
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = std::fs::metadata(&p) {
                total += meta.len();
            }
        }
    }
    total
}

fn scale(r: u64) -> String {
    match r {
        1_000 => "1K".to_string(),
        10_000 => "10K".to_string(),
        100_000 => "100K".to_string(),
        1_000_000 => "1M".to_string(),
        10_000_000 => "10M".to_string(),
        n => n.to_string(),
    }
}

fn main() {
    let args = parse_args();
    let dir = std::env::temp_dir().join(format!(
        "qmind-bench-r3-{}-pid{}-{}",
        scale(args.rows),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    let mut report = Report::new(args.rows, args.iterations, args.chunk);

    let mut eng = Engine::<std::fs::File>::create_db(&dir).expect("create_db");
    bench(&mut eng, &args, &mut report);

    let close_start = Instant::now();
    eng.close().expect("close");
    let close_ms = elapsed_ms(close_start);

    let open_start = Instant::now();
    let mut eng = Engine::<std::fs::File>::open_db(&dir).expect("open_db");
    let open_ms = elapsed_ms(open_start);

    let (rows, ms) = scan_iter(&mut eng, "SELECT id FROM ticks", 1);
    if rows != report.rows {
        panic!(
            "bench_r3: reopen mismatch: expected {} rows, scanned {} — refusing to write results",
            report.rows, rows
        );
    }
    eng.close().expect("final close");
    let wal_path = dir.join("wal").join("wal.log");
    let wal_bytes = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    let tables_bytes = dir_size(&dir.join("tables"));

    report.add(
        "wf_close",
        0,
        close_ms,
        1,
        "close (WAL flush + page flush), wall-time only",
    );
    report.add(
        "wf_open",
        0,
        open_ms,
        1,
        "open_db recovery + page rebuild, wall-time only",
    );
    report.add(
        "wf_scan_after_reopen",
        rows,
        ms,
        1,
        "streaming after reopen (sanity)",
    );

    let name = format!("{}-r{}", scale(report.rows), report.iterations);
    let path = report.write(&args.out, &name).expect("write report");
    println!("{}", report.render());
    println!("---");
    println!(
        "sizes: wal={} bytes, tables={} bytes, report={}",
        wal_bytes,
        tables_bytes,
        path.display()
    );

    if !args.keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
}
