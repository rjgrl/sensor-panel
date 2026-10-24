//! Minimal HTTP/1.1 GET client for LibreHardwareMonitor's local web server.
//! Plain TCP, no proxy, no TLS — this is always a localhost/LAN request.
//! Handles Content-Length, chunked and close-delimited bodies.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const MAX_BODY: usize = 64 * 1024 * 1024;

pub fn get(host: &str, port: u16, path: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {host}: {e}"))?
        .collect();
    let mut last_err = String::from("no address");
    let mut stream = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    let mut s = stream.ok_or(last_err)?;
    let _ = s.set_read_timeout(Some(timeout));
    let _ = s.set_write_timeout(Some(timeout));
    let _ = s.set_nodelay(true);
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: application/json\r\nUser-Agent: DS916Tray\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;

    let mut buf = Vec::with_capacity(256 * 1024);
    let mut chunk = [0u8; 32 * 1024];
    let mut header_end = None;
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    loop {
        let n = match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                // A timeout after a complete Content-Length body is fine
                if let (Some(he), Some(cl)) = (header_end, content_length) {
                    if buf.len() >= he + cl {
                        break;
                    }
                }
                return Err(e.to_string());
            }
        };
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_BODY {
            return Err("response too large".into());
        }
        if header_end.is_none() {
            if let Some(pos) = find(&buf, b"\r\n\r\n") {
                header_end = Some(pos + 4);
                let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                let mut lines = head.split("\r\n");
                let status = lines.next().unwrap_or("");
                let code: u16 = status.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
                if code != 200 {
                    return Err(format!("HTTP status {}", status.trim()));
                }
                for l in lines {
                    if let Some((k, v)) = l.split_once(':') {
                        let k = k.trim().to_ascii_lowercase();
                        let v = v.trim();
                        if k == "content-length" {
                            content_length = v.parse().ok();
                        } else if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                            chunked = true;
                        }
                    }
                }
            }
        }
        if let (Some(he), Some(cl), false) = (header_end, content_length, chunked) {
            if buf.len() >= he + cl {
                break;
            }
        }
    }
    let he = header_end.ok_or("incomplete HTTP response")?;
    let body = buf.split_off(he);
    if chunked {
        return dechunk(&body);
    }
    if let Some(cl) = content_length {
        if body.len() < cl {
            return Err("truncated response".into());
        }
        let mut body = body;
        body.truncate(cl);
        return Ok(body);
    }
    Ok(body)
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(data.len());
    loop {
        let line_end = find(data, b"\r\n").ok_or("bad chunk")?;
        let size_str = String::from_utf8_lossy(&data[..line_end]);
        let size_str = size_str.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16).map_err(|_| "bad chunk size")?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size..];
        if data.starts_with(b"\r\n") {
            data = &data[2..];
        }
    }
}
