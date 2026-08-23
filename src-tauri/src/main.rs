//! QuantsMind Desktop Studio — Tauri 2 commands over the embedded engine.

use qmind_embed::Database;
use std::sync::Mutex;

#[tauri::command]
fn run_sql(sql: String, state: tauri::State<Mutex<Database<std::fs::File>>>) -> String {
    let mut db = state.lock().expect("engine lock");
    db.execute(&sql)
}

fn main() {
    std::fs::create_dir_all("qmind-data").expect("data dir");
    let wal = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("qmind-data/desktop-wal.log")
        .expect("wal");
    tauri::Builder::default()
        .manage(Mutex::new(Database::new(wal)))
        .invoke_handler(tauri::generate_handler![run_sql])
        .run(tauri::generate_context!())
        .expect("error while running desktop studio");
}