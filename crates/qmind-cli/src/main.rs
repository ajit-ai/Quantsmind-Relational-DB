//! qmind-cli — minimal psql-like shell over the wire protocol.
use std::io::{BufRead, Write};
use std::net::TcpStream;

fn main() {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:5432".into());
    let mut s = TcpStream::connect(&addr).expect("connect");
    // startup: len + 196608 + "user\0qmind\0" + \0
    let params = b"user\0qmind\0";
    let len = (4 + 4 + params.len() + 1) as i32;
    s.write_all(&len.to_be_bytes()).unwrap();
    s.write_all(&196608i32.to_be_bytes()).unwrap();
    s.write_all(params).unwrap();
    s.write_all(&[0]).unwrap();
    read_until_ready(&mut s);

    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        print!("qmind> ");
        std::io::stdout().flush().unwrap();
        line.clear();
        if stdin.lock().read_line(&mut line).unwrap() == 0 {
            break;
        }
        let sql = line.trim().trim_end_matches(';');
        if sql.is_empty() {
            continue;
        }
        if sql == "\\q" || sql.eq_ignore_ascii_case("exit") {
            send_terminate(&mut s);
            break;
        }
        query(&mut s, sql);
        read_until_ready(&mut s);
    }
}

fn msg_header(s: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut t = [0u8; 1];
    if std::io::Read::read_exact(s, &mut t).is_err() {
        return None;
    }
    let mut l = [0u8; 4];
    std::io::Read::read_exact(s, &mut l).ok()?;
    let n = i32::from_be_bytes(l) as usize;
    if n < 4 {
        return None;
    }
    let mut b = vec![0u8; n - 4];
    std::io::Read::read_exact(s, &mut b).ok()?;
    Some((t[0], b))
}

fn read_until_ready(s: &mut TcpStream) {
    while let Some((t, b)) = msg_header(s) {
        match t {
            b'T' => {
                let ncols = i16::from_be_bytes(b[0..2].try_into().unwrap());
                let mut names = Vec::new();
                let mut pos = 2;
                for _ in 0..ncols {
                    let end = b[pos..].iter().position(|&x| x == 0).unwrap() + pos;
                    names.push(String::from_utf8_lossy(&b[pos..end]).to_string());
                    pos = end + 1 + 4 + 2 + 4 + 2 + 4 + 2;
                }
                println!("{}", names.join(" | "));
                println!(
                    "{}",
                    "-".repeat(names.iter().map(|n| n.len() + 3).sum::<usize>().max(8))
                );
            }
            b'D' => {
                let nf = i16::from_be_bytes(b[0..2].try_into().unwrap());
                let mut pos = 2;
                let mut cells = Vec::new();
                for _ in 0..nf {
                    let l = i32::from_be_bytes(b[pos..pos + 4].try_into().unwrap());
                    pos += 4;
                    if l < 0 {
                        cells.push("NULL".to_string());
                        continue;
                    }
                    cells.push(String::from_utf8_lossy(&b[pos..pos + l as usize]).to_string());
                    pos += l as usize;
                }
                println!("{}", cells.join(" | "));
            }
            b'E' => {
                println!("ERROR");
            }
            b'Z' => return,
            _ => {}
        }
    }
}

fn query(s: &mut TcpStream, sql: &str) {
    let body = cstr(sql);
    let mut m = vec![b'Q'];
    m.extend_from_slice(&((body.len() as i32 + 4).to_be_bytes()));
    m.extend_from_slice(&body);
    s.write_all(&m).unwrap();
}

fn send_terminate(s: &mut TcpStream) {
    s.write_all(&[b'X', 0, 0, 0, 4]).unwrap();
}

fn cstr(v: &str) -> Vec<u8> {
    let mut b = v.as_bytes().to_vec();
    b.push(0);
    b
}
