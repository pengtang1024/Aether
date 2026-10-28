//! End-to-end check for the custom DNS resolver: a client built from a
//! `NetworkConfig` must resolve names through the configured servers only.

use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::sync::Arc;
use std::thread;

use aether_plugin_http::dns::custom_resolver;
use aether_plugin_sdk::NetworkConfig;

/// One HTTP response served to every connection.
fn spawn_http_stub() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut drain = [0_u8; 4096];
            let _ = stream.read(&mut drain);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabet",
            );
        }
    });
    port
}

/// Answers every A query with 127.0.0.1 (and every AAAA with ::1); records
/// how many queries arrived.
fn spawn_dns_stub() -> (u16, Arc<std::sync::Mutex<usize>>) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    let hits = Arc::new(std::sync::Mutex::new(0_usize));
    let counter = hits.clone();
    thread::spawn(move || {
        let mut buffer = [0_u8; 1500];
        while let Ok((size, source)) = socket.recv_from(&mut buffer) {
            *counter.lock().unwrap() += 1;
            let query = &buffer[..size];
            let mut index = 12;
            while index < size && buffer[index] != 0 {
                index += 1 + buffer[index] as usize;
            }
            let question_end = index + 1 + 4;
            if question_end > size {
                continue;
            }
            let qtype = u16::from_be_bytes([buffer[index + 1], buffer[index + 2]]);
            let (record_type, rdata): (u16, Vec<u8>) = if qtype == 28 {
                (28, vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1])
            } else {
                (1, vec![127, 0, 0, 1])
            };
            let mut response = Vec::with_capacity(question_end + 16);
            response.extend_from_slice(&query[0..2]);
            response.extend_from_slice(&[0x81, 0x80]);
            response.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 0]);
            response.extend_from_slice(&query[12..question_end]);
            response.extend_from_slice(&[0xC0, 0x0C]);
            response.extend_from_slice(&record_type.to_be_bytes());
            response.extend_from_slice(&[0, 1, 0, 0, 0, 60, 0, rdata.len() as u8]);
            response.extend_from_slice(&rdata);
            let _ = socket.send_to(&response, source);
        }
    });
    (port, hits)
}

fn error_chain(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(&format!("\n  caused by: {cause}"));
        source = cause.source();
    }
    message
}

#[test]
fn resolves_through_the_configured_servers() {
    let http_port = spawn_http_stub();
    let (dns_port, dns_hits) = spawn_dns_stub();

    let network = NetworkConfig {
        dns: Some(vec![format!("127.0.0.1:{dns_port}")]),
        ..NetworkConfig::default()
    };
    let resolver = custom_resolver(&network).unwrap();
    let client = reqwest::blocking::Client::builder()
        .dns_resolver(Arc::new(resolver))
        .build()
        .unwrap();

    // `.test` is reserved for testing (RFC 6761): real resolvers have no
    // answers for it, but it is not a special-purpose zone for the resolver
    // itself (unlike `.invalid`, which is answered locally without any
    // network query), so the query must reach the configured server.
    let url = format!("http://aether-dnstest.test:{http_port}/x.bin");
    match client.get(&url).send() {
        Ok(response) => {
            let body = response.bytes().unwrap();
            assert_eq!(&body[..], b"abet");
            assert!(*dns_hits.lock().unwrap() > 0, "the DNS stub saw no queries");
        }
        Err(error) => {
            panic!(
                "request through the custom resolver failed (dns hits: {}): {}",
                dns_hits.lock().unwrap(),
                error_chain(&error)
            );
        }
    }
}
