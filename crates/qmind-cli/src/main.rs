//! qmind-cli — interactive shell client (M5).
//!
//! Speaks the wire protocol against local or remote qmind-server.

fn main() {
    let engine = qmind_sql::ENGINE;
    println!(
        "{} shell v{} (REPL lands in M5 — see docs/ROADMAP.md)",
        engine.name, engine.version
    );
}
