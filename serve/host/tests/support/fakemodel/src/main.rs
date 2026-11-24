//! A fake Anthropic Messages API: `kith-fakemodel --script turns.json [--log requests.jsonl] [--addr 127.0.0.1:0]`.
//! Prints the bound address on stdout, then serves until killed. See README.md for the script format.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use kith_fakemodel::{Server, handle};

fn main() {
    let mut script = None;
    let mut log = None;
    let mut addr = "127.0.0.1:0".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let v = args.next();
        match (a.as_str(), v) {
            ("--script", Some(v)) => script = Some(v),
            ("--log", Some(v)) => log = Some(v),
            ("--addr", Some(v)) => addr = v,
            _ => {
                eprintln!("usage: kith-fakemodel --script FILE [--log FILE] [--addr HOST:PORT]");
                std::process::exit(2);
            }
        }
    }
    let script = script.map(|p| std::fs::read_to_string(&p).unwrap_or_else(|e| die(&format!("{p}: {e}"))));
    let server = Server::new(script.as_deref(), log.map(Into::into)).unwrap_or_else(|e| die(&e));
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| die(&format!("bind {addr}: {e}")));
    println!("{}", listener.local_addr().unwrap_or_else(|e| die(&e.to_string())));
    let _ = std::io::stdout().flush();
    let server = Arc::new(Mutex::new(server));
    for conn in listener.incoming().flatten() {
        let server = Arc::clone(&server);
        // One thread per connection: Claude Code sends side requests while the main one streams.
        std::thread::spawn(move || serve_conn(conn, &server));
    }
}

fn die(msg: &str) -> ! {
    eprintln!("kith-fakemodel: {msg}");
    std::process::exit(1)
}

fn serve_conn(conn: TcpStream, server: &Mutex<Server>) {
    let Ok(mut out) = conn.try_clone() else { return };
    let mut input = BufReader::new(conn);
    while let Some((method, path, body, close)) = read_request(&mut input) {
        let (reply, keep) = handle(server, &method, &path, &body);
        if out.write_all(&reply).is_err() || out.flush().is_err() || !keep || close {
            return;
        }
    }
}

/// One HTTP/1.1 request: method, path, body. Handles Content-Length and chunked bodies.
fn read_request(input: &mut BufReader<TcpStream>) -> Option<(String, String, Vec<u8>, bool)> {
    let mut line = String::new();
    if input.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let (mut len, mut chunked, mut close) = (0usize, false, false);
    loop {
        let mut h = String::new();
        input.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "content-length" {
                len = v.parse().ok()?;
            } else if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if k == "connection" && v.eq_ignore_ascii_case("close") {
                close = true;
            }
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            input.read_line(&mut size).ok()?;
            let size = usize::from_str_radix(size.trim().split(';').next()?, 16).ok()?;
            if size == 0 {
                // Trailers, then the final blank line.
                loop {
                    let mut t = String::new();
                    input.read_line(&mut t).ok()?;
                    if t.trim().is_empty() {
                        break;
                    }
                }
                break;
            }
            let mut chunk = vec![0; size];
            input.read_exact(&mut chunk).ok()?;
            body.extend(chunk);
            let mut crlf = String::new();
            input.read_line(&mut crlf).ok()?;
        }
    } else {
        body = vec![0; len];
        input.read_exact(&mut body).ok()?;
    }
    Some((method, path, body, close))
}
