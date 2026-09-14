//! qmind-server — Postgres wire protocol listener.
//! Usage: qmind-server [DATA_DIR] [PORT]   (defaults: ./qmind-data, 5432, trust auth)

use qmind_server::wire;

use qmind_sql::Engine;
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, RwLock};

fn main() {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "qmind-data".to_string());
    let port: u16 = args.next().and_then(|a| a.parse().ok()).unwrap_or(5432);

    // Durable lifecycle: first run creates the database, later runs recover it.
    let engine = if Path::new(&data_dir).join("db.meta").exists() {
        Arc::new(RwLock::new(
            Engine::<std::fs::File>::open_db(&data_dir)
                .expect("open database: corrupt or incompatible WAL/metadata"),
        ))
    } else {
        Arc::new(RwLock::new(
            Engine::<std::fs::File>::create_db(&data_dir).expect("create database"),
        ))
    };
    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr).expect("bind");
    println!(
        "{} server v{} listening on {} (data dir: {}, trust auth)",
        qmind_sql::ENGINE.name,
        qmind_sql::ENGINE.version,
        addr,
        data_dir
    );
    wire::serve(listener, engine);
}
