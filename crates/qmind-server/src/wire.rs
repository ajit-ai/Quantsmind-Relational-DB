//! Minimal Postgres wire-protocol (v3) listener over blocking TCP.
//! Scope (M5a): trust auth, simple Query protocol. Values as TEXT (OID 25).
use qmind_sql::{Engine, SqlValue};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

const PROTOCOL_V3: i32 = 196608;
pub type SharedEngine = Arc<Mutex<Engine<std::fs::File>>>;

pub fn serve<W: Write + Send + 'static>(listener: TcpListener, engine: Arc<Mutex<Engine<W>>>) {
    for stream in listener.incoming() {
        let Ok(s) = stream else { continue };
        let e = Arc::clone(&engine);
        std::thread::spawn(move || {
            let _ = handle_conn(s, e);
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

fn handle_conn<W: Write>(mut s: TcpStream, eng: Arc<Mutex<Engine<W>>>) -> std::io::Result<()> {
    let mut lb = [0u8; 4];
    if s.read_exact(&mut lb).is_err() {
        return Ok(());
    }
    let len = i32::from_be_bytes(lb) as usize;
    if len < 8 || len > 10_000 {
        return Ok(());
    }
    let mut buf = vec![0u8; len - 4];
    s.read_exact(&mut buf)?;
    let proto = i32::from_be_bytes(buf[0..4].try_into().unwrap());
    if proto == 80877103 {
        // SSLRequest
        s.write_all(b"N")?;
        s.flush()?;
        return handle_conn(s, eng);
    }
    if proto != PROTOCOL_V3 {
        return Ok(());
    }
    write_msg(&mut s, b'R', &0i32.to_be_bytes())?;
    param(&mut s, "server_version", "16-qmind")?;
    param(&mut s, "client_encoding", "UTF8")?;
    ready(&mut s)?;

    loop {
        let Some((tag, payload)) = read_packet(&mut s)? else {
            return Ok(());
        };
        match tag {
            b'Q' => {
                let sql = String::from_utf8_lossy(&payload)
                    .trim_end_matches('\0')
                    .to_string();
                run_query(&mut s, &eng, &sql)?;
            }
            b'X' => return Ok(()),
            _ => {
                error(&mut s, "unsupported message")?;
                ready(&mut s)?;
            }
        }
    }
}

fn run_query<W: Write>(
    s: &mut TcpStream,
    eng: &Arc<Mutex<Engine<W>>>,
    sql: &str,
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
        let mut guard = match eng.lock() {
            Ok(g) => g,
            Err(_) => {
                error(s, "engine poisoned")?;
                return ready(s);
            }
        };
        match guard.execute(st) {
            Ok(res) => {
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
                drop(guard);
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
    write_msg(s, b'Z', &[b'I'])
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
