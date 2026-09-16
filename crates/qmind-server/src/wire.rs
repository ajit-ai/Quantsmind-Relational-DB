//! Minimal Postgres wire-protocol (v3) listener over blocking TCP.
//! Scope (M5a): trust auth, simple Query protocol. Values as TEXT (OID 25).
//! P5: SELECT/SHOW ride the read side of an `RwLock` and run on snapshots, so
//! readers never block each other or wait for the writer's commit.
use qmind_sql::{Engine, SqlValue};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

const PROTOCOL_V3: i32 = 196608;
pub type SharedEngine = Arc<RwLock<Engine<std::fs::File>>>;

pub fn serve<W: Write + Send + Sync + 'static>(
    listener: TcpListener,
    engine: Arc<RwLock<Engine<W>>>,
) {
    let next_session = Arc::new(AtomicU64::new(1));
    for stream in listener.incoming() {
        let Ok(s) = stream else { continue };
        let e = Arc::clone(&engine);
        let ns = Arc::clone(&next_session);
        std::thread::spawn(move || {
            // R4-MULTIWRITER: every connection is a distinct session that may
            // own its own explicit transaction across packets, concurrently
            // with every other connection.
            let sid = ns.fetch_add(1, Ordering::SeqCst);
            let _ = handle_conn(s, e, sid);
        });
    }
}

fn read_packet(stream: &mut TcpStream) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let mut t = [0u8; 1];
    if stream.read_exact(&mut t).is_err() {
        return Ok(None);
    }
    let mut l = [0u8; 4];
    stream.read_exact(&mut l)?;
    let len = i32::from_be_bytes(l) as usize;
    if len < 4 {
        return Ok(None);
    }
    let mut p = vec![0u8; len - 4];
    stream.read_exact(&mut p)?;
    Ok(Some((t[0], p)))
}

fn handle_conn<W: Write>(
    mut s: TcpStream,
    eng: Arc<RwLock<Engine<W>>>,
    session: u64,
) -> std::io::Result<()> {
    let mut lb = [0u8; 4];
    if s.read_exact(&mut lb).is_err() {
        return Ok(());
    }
    let len = i32::from_be_bytes(lb) as usize;
    if !(8..=10_000).contains(&len) {
        return Ok(());
    }
    let mut buf = vec![0u8; len - 4];
    s.read_exact(&mut buf)?;
    let proto = i32::from_be_bytes(buf[0..4].try_into().unwrap());
    if proto == 80877103 {
        // SSLRequest
        s.write_all(b"N")?;
        s.flush()?;
        return handle_conn(s, eng, session);
    }
    if proto != PROTOCOL_V3 {
        return Ok(());
    }
    write_msg(&mut s, b'R', &0i32.to_be_bytes())?;
    param(&mut s, "server_version", "16-qmind")?;
    param(&mut s, "client_encoding", "UTF8")?;
    ready(&mut s)?;

    let mut session_has_txn = false;
    while let Some((tag, payload)) = read_packet(&mut s)? {
        match tag {
            b'Q' => {
                let sql = String::from_utf8_lossy(&payload)
                    .trim_end_matches('\0')
                    .to_string();
                run_query(&mut s, &eng, session, &sql, &mut session_has_txn)?;
            }
            b'X' => break,
            _ => {
                error(&mut s, "unsupported message")?;
                ready(&mut s)?;
            }
        }
    }
    // R4-MULTIWRITER: a session that owns an explicit transaction and then
    // disconnects (Terminate packet or TCP close) must have that transaction
    // rolled back so its row locks are released and other sessions are never
    // blocked. The rollback is per session; every other session's transaction
    // is unaffected. ROLLBACK is a pure in-memory discard here: nothing was
    // materialized mid-transaction.
    release_session_txn(&eng, session);
    Ok(())
}

/// R4-MULTIWRITER: if `session` holds an explicit transaction, abort it so the
/// transaction's locks and pending writes are released when the connection
/// ends without an explicit COMMIT or ROLLBACK. Safe to call at most once per
/// connection; when the engine holds no transaction for the session this is a
/// no-op (`txn_rollback` errors before emitting any side effect).
fn release_session_txn<W: Write>(eng: &Arc<RwLock<Engine<W>>>, session: u64) {
    if let Ok(guard) = eng.read() {
        if !guard.session_in_transaction(session) {
            return;
        }
    }
    if let Ok(mut guard) = eng.write() {
        let _ = guard.execute_session(session, "ROLLBACK");
    }
}

fn run_query<W: Write>(
    s: &mut TcpStream,
    eng: &Arc<RwLock<Engine<W>>>,
    session: u64,
    sql: &str,
    session_has_txn: &mut bool,
) -> std::io::Result<()> {
    // Multiple statements separated by ';' execute sequentially.
    let stmts: Vec<&str> = sql
        .split(';')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .collect();
    if stmts.is_empty() {
        empty_query_response(s)?;
        return ready(s);
    }
    for st in stmts {
        let kw = st.split_whitespace().next().map(|w| w.to_ascii_uppercase());
        // R4-MULTIWRITER routing: while this session owns its own explicit
        // transaction, every statement (including SELECT) runs on the write
        // path so the transaction's own uncommitted writes stay visible and
        // join the same transaction. Sessions outside a transaction read on a
        // committed-only snapshot and write in autocommit. Multiple sessions
        // may hold explicit transactions concurrently; row-key write conflicts
        // surface deterministically as statement errors (no blocking).
        let owned = {
            let guard = match eng.read() {
                Ok(g) => g,
                Err(_) => {
                    error(s, "engine poisoned")?;
                    return ready(s);
                }
            };
            *session_has_txn || guard.session_in_transaction(session)
        };
        let res = if !owned && matches!(kw.as_deref(), Some("SELECT") | Some("SHOW")) {
            // Fast path: read on a committed-only snapshot under the shared
            // guard; never polluted by any session's uncommitted writes.
            let guard = match eng.read() {
                Ok(g) => g,
                Err(_) => {
                    error(s, "engine poisoned")?;
                    return ready(s);
                }
            };
            guard.execute_read(st)
        } else {
            // Write path: session-owned transaction (own-write visibility) or
            // autocommit DML/DDL. The engine write guard serializes statement
            // execution while leaving transactions per-session independent.
            let mut guard = match eng.write() {
                Ok(g) => g,
                Err(_) => {
                    error(s, "engine poisoned")?;
                    return ready(s);
                }
            };
            guard.execute_session(session, st)
        };
        match res {
            Ok(res) => {
                match kw.as_deref() {
                    Some("BEGIN") => *session_has_txn = true,
                    Some("COMMIT") | Some("ROLLBACK") => *session_has_txn = false,
                    _ => {}
                }
                if !res.columns.is_empty() {
                    row_description(s, &res.columns)?;
                    for row in &res.rows {
                        data_row(s, row)?;
                    }
                }
                let tag = if res.columns.is_empty() || res.rows_affected > 0 {
                    format!(
                        "INSERT 0 {}",
                        res.rows_affected.max(if res.columns.is_empty() {
                            res.rows_affected
                        } else {
                            res.rows.len() as u64
                        })
                    )
                } else {
                    format!("SELECT {}", res.rows.len())
                };
                command_complete(
                    s,
                    &if res.columns.is_empty() && res.rows_affected == 0 {
                        "CREATE TABLE".into()
                    } else {
                        tag
                    },
                )?;
            }
            Err(e) => {
                // A failed COMMIT or ROLLBACK still ends this session's
                // transaction — `txn_commit`/`txn_rollback` have already taken
                // (and either finished or discarded) it. Resetting the flag
                // keeps the session's tracking in sync with the engine state;
                // no transaction is left half-open.
                if matches!(kw.as_deref(), Some("COMMIT") | Some("ROLLBACK")) {
                    *session_has_txn = false;
                }
                error(s, &e)?;
                break;
            }
        }
    }
    ready(s)
}

fn write_msg(s: &mut TcpStream, tag: u8, body: &[u8]) -> std::io::Result<()> {
    let mut m = Vec::with_capacity(body.len() + 5);
    m.push(tag);
    m.extend_from_slice(&((body.len() as i32 + 4).to_be_bytes()));
    m.extend_from_slice(body);
    s.write_all(&m)
}

fn cstr(v: &str) -> Vec<u8> {
    let mut b = v.as_bytes().to_vec();
    b.push(0);
    b
}

fn param(s: &mut TcpStream, k: &str, v: &str) -> std::io::Result<()> {
    let mut b = cstr(k);
    b.extend_from_slice(&cstr(v));
    write_msg(s, b'S', &b)
}

fn ready(s: &mut TcpStream) -> std::io::Result<()> {
    write_msg(s, b'Z', b"I")
}

fn empty_query_response(s: &mut TcpStream) -> std::io::Result<()> {
    write_msg(s, b'I', &[])
}

fn command_complete(s: &mut TcpStream, tag: &str) -> std::io::Result<()> {
    write_msg(s, b'C', &cstr(tag))
}

fn row_description(s: &mut TcpStream, cols: &[String]) -> std::io::Result<()> {
    let mut b = Vec::new();
    b.extend_from_slice(&(cols.len() as i16).to_be_bytes());
    for name in cols {
        b.extend_from_slice(&cstr(name)); // name
        b.extend_from_slice(&0i32.to_be_bytes()); // table oid
        b.extend_from_slice(&0i16.to_be_bytes()); // attnum
        b.extend_from_slice(&25i32.to_be_bytes()); // TEXT oid
        b.extend_from_slice(&0i16.to_be_bytes()); // typlen -1 var
        b.extend_from_slice(&(-1i32).to_be_bytes());
        b.extend_from_slice(&0i32.to_be_bytes()); // typmod
        b.extend_from_slice(&0i16.to_be_bytes()); // text format
    }
    write_msg(s, b'T', &b)
}

fn data_row(s: &mut TcpStream, row: &[SqlValue]) -> std::io::Result<()> {
    let mut b = Vec::new();
    b.extend_from_slice(&(row.len() as i16).to_be_bytes());
    for v in row {
        let txt = match v {
            SqlValue::Null => None,
            other => Some(other.to_string()),
        };
        match txt {
            Some(t) => {
                b.extend_from_slice(&(t.len() as i32).to_be_bytes());
                b.extend_from_slice(t.as_bytes());
            }
            None => b.extend_from_slice(&(-1i32).to_be_bytes()),
        }
    }
    write_msg(s, b'D', &b)
}

fn error(s: &mut TcpStream, msg: &str) -> std::io::Result<()> {
    let mut b = Vec::new();
    b.push(b'S');
    b.extend_from_slice(&cstr("ERROR"));
    b.push(b'M');
    b.extend_from_slice(&cstr(msg));
    b.push(0);
    write_msg(s, b'E', &b)
}
