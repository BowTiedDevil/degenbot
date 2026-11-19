//! In-process OTLP/HTTP receiver shared by the telemetry export tests.
//!
//! Reads one request per connection, answers `200 OK` with an empty
//! `ExportTraceServiceResponse` (a valid protobuf message with every field
//! absent), and records each request target. `tests/support/` is not a test
//! target of its own: the export tests include this file with `#[path]`, so
//! the receiver has exactly one home.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Local OTLP/HTTP endpoint: the proof that a span reached the wire without an
/// external collector.
pub struct OtlpHttpReceiver {
    addr: SocketAddr,
    targets: Arc<Mutex<Vec<String>>>,
}

impl OtlpHttpReceiver {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind in-process receiver");
        let addr = listener.local_addr().expect("receiver address");
        let targets = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&targets);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                if let Ok(target) = serve_one(&mut stream) {
                    recorded.lock().expect("receiver log").push(target);
                }
            }
        });
        Self { addr, targets }
    }

    /// Collector base URL, i.e. the form `provider_from_endpoint` accepts
    /// before it appends the signal path.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Request targets received so far, in arrival order.
    pub fn request_targets(&self) -> Vec<String> {
        self.targets.lock().expect("receiver log").clone()
    }
}

fn serve_one(stream: &mut TcpStream) -> std::io::Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let headers_end = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos;
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(String::new());
        }
        buf.extend_from_slice(&chunk[..read]);
    };
    let headers = String::from_utf8_lossy(&buf[..headers_end]).to_string();
    let target = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string();
    // Drain the declared body before answering: a response written over unread
    // bytes resets the connection, and the exporter would report a failure even
    // though the request had arrived.
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let mut body_have = buf.len() - (headers_end + 4);
    while body_have < content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        body_have += read;
    }
    stream.write_all(
        b"HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    )?;
    stream.flush()?;
    Ok(target)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
