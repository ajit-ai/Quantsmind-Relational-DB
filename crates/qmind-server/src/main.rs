//! qmind-server — Postgres wire protocol listener.
//! Usage: qmind-server [PORT]   (data dir ./qmind-data, trust auth)

use qmind_server::wire;

use qmind_sql::Engine;
use std::net::TcpListener;
use std::sync::{Arc, RwLock};

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(5432);
    std::fs::create_dir_all("qmind-data").expect("create data dir");
    let wal = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("qmind-data/wal.log")
        .expect("open wal");
    let engine = Arc::new(RwLock::new(Engine::new(wal)));
    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr).expect("bind");
    println!(
        "{} server v{} listening on {} (trust auth)",
        qmind_sql::ENGINE.name,
        qmind_sql::ENGINE.version,
        addr
    );
    wire::serve(listener, engine);
}
