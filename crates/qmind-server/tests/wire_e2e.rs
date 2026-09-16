//! M5: full wire-protocol roundtrip over real TCP sockets.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};

fn start_server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let eng = Arc::new(RwLock::new(qmind_sql::Engine::new(Vec::new())));
    std::thread::spawn(move || qmind_server::wire::serve(listener, eng));
    port
}

fn cstr(v: &str) -> Vec<u8> {
    let mut b = v.as_bytes().to_vec();
    b.push(0);
    b
}

fn connect(port: u16) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let params = b"user\0qmind\0";
    let len = (8 + params.len() + 1) as i32;
    s.write_all(&len.to_be_bytes()).unwrap();
    s.write_all(&196608i32.to_be_bytes()).unwrap();
    s.write_all(params).unwrap();
    s.write_all(&[0]).unwrap();
    read_until_ready(&mut s);
    s
}

fn read_until_ready(s: &mut TcpStream) {
    loop {
        let mut t = [0u8; 1];
        s.read_exact(&mut t).unwrap();
        let mut l = [0u8; 4];
        s.read_exact(&mut l).unwrap();
        let n = i32::from_be_bytes(l) as usize;
        let mut b = vec![0u8; n.saturating_sub(4)];
        if n > 4 {
            s.read_exact(&mut b).unwrap();
        }
        if t[0] == b'Z' {
            return;
        }
    }
}

fn query(s: &mut TcpStream, sql: &str) -> Vec<Vec<String>> {
    match query_result(s, sql) {
        Ok(r) => r,
        Err(e) => panic!("server error for {sql}: {e}"),
    }
}

fn query_result(s: &mut TcpStream, sql: &str) -> Result<Vec<Vec<String>>, String> {
    let body = cstr(sql);
    let mut m = vec![b'Q'];
    m.extend_from_slice(&((body.len() as i32 + 4).to_be_bytes()));
    m.extend_from_slice(&body);
    s.write_all(&m).unwrap();

    let mut rows = Vec::new();
    let mut read_err = None;
    for _ in 0.. {
        let mut t = [0u8; 1];
        s.read_exact(&mut t).unwrap();
        let mut l = [0u8; 4];
        s.read_exact(&mut l).unwrap();
        let n = i32::from_be_bytes(l) as usize;
        let mut b = vec![0u8; n.saturating_sub(4)];
        if n > 4 {
            s.read_exact(&mut b).unwrap();
        }
        match t[0] {
            b'D' => {
                let nf = i16::from_be_bytes(b[0..2].try_into().unwrap());
                let mut pos = 2usize;
                let mut cells = Vec::new();
                for _ in 0..nf {
                    let vl = i32::from_be_bytes(b[pos..pos + 4].try_into().unwrap()) as usize;
                    pos += 4;
                    cells.push(String::from_utf8_lossy(&b[pos..pos + vl]).to_string());
                    pos += vl;
                }
                rows.push(cells);
            }
            b'E' => {
                // ErrorResponse body: 'S' + severity \0 ... 'M' + message \0 ... 0
                let mut pos = 0usize;
                let mut msg = String::new();
                while pos < b.len() && b[pos] != 0 {
                    let code = b[pos];
                    pos += 1;
                    let end = b[pos..]
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(b.len() - pos);
                    let val = String::from_utf8_lossy(&b[pos..pos + end]).to_string();
                    if code == b'M' {
                        msg = val;
                    }
                    pos += end + 1;
                }
                // Remember the error but keep draining until ReadyState so the
                // stream isn't left desynchronized for the next query.
                read_err = Some(msg);
            }
            b'Z' => {
                if let Some(msg) = read_err {
                    return Err(msg);
                }
                return Ok(rows);
            }
            _ => {}
        }
    }
    unreachable!("packet loop is infinite")
}

#[test]
fn wire_end_to_end_sql_over_tcp() {
    let port = start_server();
    let mut c1 = connect(port);

    query(&mut c1, "CREATE TABLE net_users (id INTEGER, name TEXT);");
    query(
        &mut c1,
        "INSERT INTO net_users VALUES (1, 'Ada'), (2, 'Grace');",
    );
    let rows = query(&mut c1, "SELECT name FROM net_users WHERE id = 2;");
    assert_eq!(rows, vec![vec!["Grace".to_string()]]);

    let mut c2 = connect(port);
    let rows = query(&mut c2, "SELECT COUNT(*) FROM net_users;");
    assert_eq!(rows, vec![vec!["2".to_string()]]);
}

#[test]
fn concurrent_readers_never_see_torn_writes() {
    let port = start_server();
    let mut c = connect(port);
    query(&mut c, "CREATE TABLE counters (id INTEGER, v INTEGER);");
    query(&mut c, "INSERT INTO counters VALUES (1, 0);");

    // Writer appends 20-row batches as single multi-row commits.
    let writer = std::thread::spawn(move || {
        let mut wc = connect(port);
        for b in 0..10u64 {
            let values: Vec<String> = (0..20u64)
                .map(|i| format!("({}, {})", 1000 + b * 20 + i, i))
                .collect();
            query(
                &mut wc,
                &format!("INSERT INTO counters VALUES {};", values.join(",")),
            );
        }
    });

    // Readers run COUNT concurrently; under snapshot isolation every observed
    // count must be a fully-committed batch boundary (1 + 20k), never torn.
    let mut readers = Vec::new();
    for _ in 0..4 {
        let p = port;
        readers.push(std::thread::spawn(move || {
            let mut rc = connect(p);
            for _ in 0..80 {
                let rows = query(&mut rc, "SELECT COUNT(*) FROM counters;");
                let n: u64 = rows[0][0].parse().unwrap();
                assert_eq!(n % 20, 1, "reader observed a torn commit: count={n}");
            }
        }));
    }
    for h in readers {
        h.join().unwrap();
    }
    writer.join().unwrap();
}

#[test]
fn transaction_session_sees_own_writes_others_see_committed_only() {
    let port = start_server();
    let mut c1 = connect(port);
    query(&mut c1, "CREATE TABLE txn_t (id INTEGER, v TEXT);");

    // Session 1 begins a transaction and inserts.
    query(&mut c1, "BEGIN;");
    query(&mut c1, "INSERT INTO txn_t VALUES (1, 'uncommitted');");
    // Own writes are visible on the owning session across packets.
    let rows = query(&mut c1, "SELECT v FROM txn_t;");
    assert_eq!(rows, vec![vec!["uncommitted".to_string()]]);

    // Session 2 (foreign) must NOT observe the uncommitted row.
    let mut c2 = connect(port);
    let rows = query(&mut c2, "SELECT COUNT(*) FROM txn_t;");
    assert_eq!(rows, vec![vec!["0".to_string()]]);

    // Session 1 commits; session 2 now sees it.
    query(&mut c1, "COMMIT;");
    let rows = query(&mut c2, "SELECT COUNT(*) FROM txn_t;");
    assert_eq!(rows, vec![vec!["1".to_string()]]);
    let rows = query(&mut c1, "SELECT v FROM txn_t;");
    assert_eq!(rows, vec![vec!["uncommitted".to_string()]]);
}

#[test]
fn foreign_writes_blocked_while_transaction_held() {
    let port = start_server();
    let mut c1 = connect(port);
    query(&mut c1, "CREATE TABLE txn_t2 (id INTEGER);");

    query(&mut c1, "BEGIN;");
    query(&mut c1, "INSERT INTO txn_t2 VALUES (1);");

    // Foreign write is rejected while the owner holds the transaction.
    let mut c2 = connect(port);
    let err = query_result(&mut c2, "INSERT INTO txn_t2 VALUES (2);").unwrap_err();
    assert!(
        err.contains("single-writer constraint"),
        "expected single-writer rejection, got: {err}"
    );
    // Foreign reads still work on a committed-only snapshot.
    let rows = query(&mut c2, "SELECT COUNT(*) FROM txn_t2;");
    assert_eq!(rows, vec![vec!["0".to_string()]]);

    // Owner rolls back; foreign writer is unblocked.
    query(&mut c1, "ROLLBACK;");
    query(&mut c2, "INSERT INTO txn_t2 VALUES (2);");
    let rows = query(&mut c1, "SELECT COUNT(*) FROM txn_t2;");
    assert_eq!(rows, vec![vec!["1".to_string()]]);
}

#[test]
fn foreign_session_never_sees_uncommitted_owner_rows() {
    let port = start_server();
    let mut c1 = connect(port);
    query(&mut c1, "CREATE TABLE vis (id INTEGER, v TEXT);");
    query(&mut c1, "CREATE INDEX iv ON vis (v);");

    // Owner inserts, scans and index-looks-up its own uncommitted row.
    query(&mut c1, "BEGIN;");
    query(&mut c1, "INSERT INTO vis VALUES (1, 'secret');");
    let rows = query(&mut c1, "SELECT id FROM vis WHERE v = 'secret';");
    assert_eq!(
        rows,
        vec![vec!["1".to_string()]],
        "owner sees own uncommitted row"
    );

    // Foreign session: both scan and index reads must stay empty.
    let mut c2 = connect(port);
    let rows = query(&mut c2, "SELECT COUNT(*) FROM vis;");
    assert_eq!(rows, vec![vec!["0".to_string()]]);
    let rows = query(&mut c2, "SELECT id FROM vis WHERE v = 'secret';");
    assert!(rows.is_empty(), "foreign index read leaked uncommitted row");

    // Owner commits; the foreign session now observes the row.
    query(&mut c1, "COMMIT;");
    let rows = query(&mut c2, "SELECT COUNT(*) FROM vis;");
    assert_eq!(rows, vec![vec!["1".to_string()]]);
    let rows = query(&mut c2, "SELECT id FROM vis WHERE v = 'secret';");
    assert_eq!(rows, vec![vec!["1".to_string()]]);
}

#[test]
fn owner_snapshot_view_is_stable_while_transaction_held() {
    let port = start_server();
    let mut c1 = connect(port);
    query(&mut c1, "CREATE TABLE stab (id INTEGER);");
    query(&mut c1, "INSERT INTO stab VALUES (1), (2), (3);");

    query(&mut c1, "BEGIN;");
    let before = query(&mut c1, "SELECT COUNT(*) FROM stab;");
    assert_eq!(before, vec![vec!["3".to_string()]]);

    // A foreign writer is rejected under the single-writer constraint, so no
    // committed change can land between two reads: the owner's snapshot view
    // is stable exactly because the engine pins the BEGIN-time snapshot and
    // the writer gate prevents any interleaved commit.
    let mut c2 = connect(port);
    let err = query_result(&mut c2, "INSERT INTO stab VALUES (9);").unwrap_err();
    assert!(err.contains("single-writer constraint"), "{err}");

    let after = query(&mut c1, "SELECT COUNT(*) FROM stab;");
    assert_eq!(
        after, before,
        "owner's snapshot view must not change mid-transaction"
    );

    query(&mut c1, "COMMIT;");

    // A fresh transaction on the same session sees the unblocked world.
    query(&mut c2, "INSERT INTO stab VALUES (4);");
    query(&mut c1, "BEGIN;");
    let rows = query(&mut c1, "SELECT COUNT(*) FROM stab;");
    assert_eq!(rows, vec![vec!["4".to_string()]]);
    query(&mut c1, "COMMIT;");
}

#[test]
fn rolled_back_owner_rows_invisible_to_foreign_session() {
    let port = start_server();
    let mut c1 = connect(port);
    query(&mut c1, "CREATE TABLE rb (id INTEGER, v TEXT);");

    query(&mut c1, "BEGIN;");
    query(&mut c1, "INSERT INTO rb VALUES (1, 'doomed');");
    let mut c2 = connect(port);
    let rows = query(&mut c2, "SELECT COUNT(*) FROM rb;");
    assert_eq!(rows, vec![vec!["0".to_string()]]);

    query(&mut c1, "ROLLBACK;");
    let rows = query(&mut c2, "SELECT COUNT(*) FROM rb;");
    assert_eq!(
        rows,
        vec![vec!["0".to_string()]],
        "rollback hides the write"
    );
    let rows = query(&mut c1, "SELECT COUNT(*) FROM rb;");
    assert_eq!(rows, vec![vec!["0".to_string()]]);
}
