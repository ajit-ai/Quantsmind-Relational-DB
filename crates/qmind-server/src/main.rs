//! qmind-server — Postgres wire protocol listener (M5, D-004).
//!
//! tokio runtime, scram-sha-256 auth, simple + extended query protocol.

fn main() {
    let engine = qmind_sql::ENGINE;
    println!(
        "{} server v{} (wire listener lands in M5 — see docs/ROADMAP.md)",
        engine.name, engine.version
    );
}
