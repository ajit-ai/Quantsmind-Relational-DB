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
    let body = cstr(sql);
    let mut m = vec![b'Q'];
    m.extend_from_slice(&((body.len() as i32 + 4).to_be_bytes()));
    m.extend_from_slice(&body);
    s.write_all(&m).unwrap();

    let mut rows = Vec::new();
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
            b'E' => panic!("server error for {sql}"),
            b'Z' => return rows,
            _ => {}
        }
    }
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
