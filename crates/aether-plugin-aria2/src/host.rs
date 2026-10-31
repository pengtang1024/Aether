//! Tiny loopback HTTP source for `addTorrent` / `addMetalink` payloads.
//!
//! aria2 accepts torrent/metalink bytes inline; Aether's download plugins take
//! a URL. The bridge publishes the decoded bytes under a random token on an
//! ephemeral loopback port and hands that URL to the daemon. Entries expire;
//! the server is bounded to loopback and never lists its directory.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Long enough for a queued torrent to wait its turn; the daemon may hold a
// task behind the concurrency limit for hours.
const ENTRY_TTL: Duration = Duration::from_secs(24 * 3600);
const MAX_ENTRIES: usize = 128;

#[derive(Clone)]
pub struct TorrentHost {
    port: u16,
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

struct Entry {
    bytes: Vec<u8>,
    content_type: &'static str,
    expires: Instant,
}

impl TorrentHost {
    pub fn start() -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| format!("cannot bind the torrent source: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("cannot read the torrent source address: {error}"))?
            .port();
        let entries = Arc::new(Mutex::new(HashMap::new()));
        let worker = entries.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let entries = worker.clone();
                std::thread::spawn(move || serve(stream, &entries));
            }
        });
        Ok(Self { port, entries })
    }

    /// Publish `bytes` and return the URL the daemon should add.
    pub fn publish(
        &self,
        bytes: Vec<u8>,
        extension: &str,
        content_type: &'static str,
    ) -> Result<String, String> {
        let mut token = [0u8; 16];
        getrandom::fill(&mut token).map_err(|error| format!("no random source: {error}"))?;
        let token = token
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "torrent source lock poisoned".to_string())?;
        entries.retain(|_, entry| entry.expires > Instant::now());
        if entries.len() >= MAX_ENTRIES {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires)
                .map(|(token, _)| token.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            token.clone(),
            Entry {
                bytes,
                content_type,
                expires: Instant::now() + ENTRY_TTL,
            },
        );
        Ok(format!(
            "http://127.0.0.1:{}/{token}.{extension}",
            self.port
        ))
    }
}

fn serve(stream: TcpStream, entries: &Arc<Mutex<HashMap<String, Entry>>>) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut writer = stream;
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.len() > 8192 {
        return;
    }
    let Some(token) = line.split_whitespace().nth(1).and_then(|target| {
        let path = target.split("://").nth(1).unwrap_or(target);
        let path = path.find('/').map_or(path, |index| &path[index..]);
        path.trim_start_matches('/').split('.').next()
    }) else {
        let _ = write_response(&mut writer, "400 Bad Request", "text/plain", b"bad request");
        return;
    };
    let entry = entries.lock().ok().and_then(|entries| {
        entries
            .get(token)
            .map(|entry| (entry.bytes.clone(), entry.content_type))
    });
    let Some((bytes, content_type)) = entry else {
        let _ = write_response(&mut writer, "404 Not Found", "text/plain", b"not found");
        return;
    };
    let _ = write_response(&mut writer, "200 OK", content_type, &bytes);
}

fn write_response(
    writer: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write!(
        writer,
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    )?;
    writer.write_all(body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn publishes_and_serves_a_payload_on_loopback() {
        let host = TorrentHost::start().unwrap();
        let url = host
            .publish(
                b"d4:infod4:name4:teste".to_vec(),
                "torrent",
                "application/x-bittorrent",
            )
            .unwrap();
        assert!(url.starts_with("http://127.0.0.1:"));
        assert!(url.ends_with(".torrent"));

        let (address, path) = url.trim_start_matches("http://").split_once('/').unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(format!("GET /{path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        assert!(text.ends_with("d4:infod4:name4:teste"), "{text}");
    }
}
