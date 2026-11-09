//! Std-only HTTP/1.1 mock server on 127.0.0.1 for tests. Sequential, `Connection: close`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Req {
    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
    pub fn body_str(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub delay: Duration,
}

impl Reply {
    pub fn new(status: u16, body: &str) -> Self {
        Reply { status, headers: vec![("Content-Type".into(), "application/json".into())], body: body.into(), delay: Duration::ZERO }
    }
    pub fn with(mut self, k: &str, v: &str) -> Self {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(k));
        self.headers.push((k.into(), v.into()));
        self
    }
    pub fn delay(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }
}

pub fn ok(body: &str) -> Reply {
    Reply::new(200, body)
}

pub fn err(status: u16, body: &str) -> Reply {
    Reply::new(status, body)
}

/// `fixtures/stimmwerk/llm/<kind>/<status>[_tag]` as a reply: status from the file name, body from `.json`, extra
/// headers from a sibling `.headers` (e.g. `retry-after: 0`).
pub fn fixture(rel: &str) -> Reply {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/llm/");
    let file = rel.rsplit('/').next().unwrap_or(rel);
    let status: u16 = file.split('_').next().and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("fixture {rel}: name must start with the HTTP status"));
    let body = std::fs::read_to_string(format!("{dir}{rel}.json")).unwrap_or_else(|e| panic!("fixture {rel}.json: {e}"));
    let mut r = Reply::new(status, &body);
    if let Ok(h) = std::fs::read_to_string(format!("{dir}{rel}.headers")) {
        for (k, v) in h.lines().filter_map(|l| l.split_once(':')) {
            r = r.with(k.trim(), v.trim());
        }
    }
    r
}

pub struct Mock {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    reqs: Arc<Mutex<Vec<Req>>>,
    stop: Arc<AtomicBool>,
    th: Option<JoinHandle<()>>,
}

impl Mock {
    /// i-th request gets `replies[min(i, len-1)]`.
    pub fn start(replies: Vec<Reply>) -> Self {
        assert!(!replies.is_empty(), "Mock needs >=1 reply");
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let addr = l.local_addr().unwrap();
        let (hits, reqs, stop) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![])), Arc::new(AtomicBool::new(false)));
        let (h, r, s) = (hits.clone(), reqs.clone(), stop.clone());
        let th = thread::spawn(move || {
            while !s.load(SeqCst) {
                match l.accept() {
                    Ok((c, _)) => {
                        c.set_nonblocking(false).ok();
                        c.set_read_timeout(Some(Duration::from_secs(10))).ok();
                        let mut br = BufReader::new(c);
                        let Some(req) = read_req(&mut br) else { continue };
                        let rep = replies[h.fetch_add(1, SeqCst).min(replies.len() - 1)].clone();
                        r.lock().unwrap().push(req);
                        thread::sleep(rep.delay);
                        write_reply(br.into_inner(), &rep);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(2)),
                    // a failed accept must not kill the server (later requests would hang)
                    Err(_) => thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Mock { addr, hits, reqs, stop, th: Some(th) }
    }

    /// `http://127.0.0.1:PORT` (no path; append e.g. `/v1`).
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    pub fn hits(&self) -> usize {
        self.hits.load(SeqCst)
    }
    pub fn reqs(&self) -> Vec<Req> {
        self.reqs.lock().unwrap().clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(t) = self.th.take() {
            let _ = t.join();
        }
    }
}

fn read_req(br: &mut BufReader<TcpStream>) -> Option<Req> {
    let mut line = String::new();
    br.read_line(&mut line).ok()?;
    let mut p = line.trim_end().splitn(3, ' ');
    let (method, path) = (p.next()?.to_string(), p.next()?.to_string());
    let mut headers = vec![];
    loop {
        let mut h = String::new();
        br.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0; len];
    br.read_exact(&mut body).ok()?;
    Some(Req { method, path, headers, body })
}

fn write_reply(mut c: TcpStream, r: &Reply) {
    let mut out = format!("HTTP/1.1 {} \r\n", r.status);
    for (k, v) in &r.headers {
        out += &format!("{k}: {v}\r\n");
    }
    out += &format!("Content-Length: {}\r\nConnection: close\r\n\r\n{}", r.body.len(), r.body);
    let _ = c.write_all(out.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Raw HTTP exchange: write `raw`, read until server closes.
    fn send(url: &str, raw: &str) -> String {
        let mut c = TcpStream::connect(url.trim_start_matches("http://")).unwrap();
        c.write_all(raw.as_bytes()).unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        out
    }

    const POST: &str = "POST /v1/x HTTP/1.1\r\nHost: h\r\nx-api-key: k\r\nContent-Length: 2\r\n\r\nhi";

    #[test]
    fn status_header_body_and_record() {
        let m = Mock::start(vec![Reply::new(429, "{\"e\":1}").with("retry-after", "0")]);
        let res = send(&m.url(), POST);
        assert!(res.starts_with("HTTP/1.1 429 \r\n"), "{res}");
        assert!(res.contains("retry-after: 0\r\n"), "{res}");
        assert!(res.contains("Content-Type: application/json\r\n"), "{res}");
        assert!(res.ends_with("{\"e\":1}"), "{res}");
        let r = m.reqs();
        assert_eq!((r.len(), r[0].method.as_str(), r[0].path.as_str()), (1, "POST", "/v1/x"));
        assert_eq!(r[0].header("X-Api-Key"), Some("k"));
        assert_eq!(r[0].body_str(), "hi");
    }

    #[test]
    fn delay_applied() {
        let m = Mock::start(vec![ok("").delay(Duration::from_millis(150))]);
        let t = Instant::now();
        send(&m.url(), POST);
        assert!(t.elapsed() >= Duration::from_millis(150));
    }

    #[test]
    fn sequence_and_counter() {
        let m = Mock::start(vec![err(500, "a"), err(502, "b"), ok("c")]);
        let st: Vec<_> = (0..4).map(|_| send(&m.url(), POST).split(' ').nth(1).unwrap().to_string()).collect();
        assert_eq!(st, ["500", "502", "200", "200"]);
        assert_eq!(m.hits(), 4);
        assert_eq!(m.reqs().len(), 4);
    }

    #[test]
    fn fixtures_load() {
        let r = fixture("anthropic/429");
        assert_eq!((r.status, r.headers.iter().find(|(k, _)| k == "retry-after").map(|(_, v)| v.as_str())), (429, Some("0")));
        assert!(r.body.contains("rate_limit_error"));
        assert_eq!(fixture("openai/200_malformed_content").status, 200);
        assert_eq!(fixture("anthropic/529").headers.len(), 1);
    }

    #[test]
    fn drop_shuts_down() {
        let url = {
            let m = Mock::start(vec![ok("")]);
            send(&m.url(), POST);
            m.url()
        };
        assert!(TcpStream::connect(url.trim_start_matches("http://")).is_err());
    }
}
