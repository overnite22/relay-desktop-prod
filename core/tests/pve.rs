use relay_desktop_core::{DataSource, ServerSource};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

#[test]
fn console_ticket_uses_device_auth_and_encodes_token() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        for expected in ["GET /api/me ", "POST /api/entries/console/connect "] {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with(expected));
            let mut headers = String::new();
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h == "\r\n" {
                    break;
                }
                headers.push_str(&h);
            }
            assert!(headers.to_lowercase().contains("authorization: bearer test-device"));
            // Read the request body: closing with unread data resets the
            // connection before the client has read the response.
            let len = headers
                .lines()
                .find_map(|h| {
                    h.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            let mut body_in = vec![0; len];
            std::io::Read::read_exact(&mut reader, &mut body_in).unwrap();
            let body = if expected.starts_with("GET") {
                r#"{"user":"test"}"#
            } else {
                r#"{"token":"once+/=?","password":"temporary"}"#
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
    });
    let source = ServerSource::connect(&base, "test-device", "test").unwrap();
    let ticket = source.pve_console("console").unwrap();
    assert!(ticket.url.ends_with("/api/vnc-tunnel?token=once%2B%2F%3D%3F"));
    assert!(ticket.url.starts_with("ws://"));
    assert_eq!(ticket.password.as_str(), "temporary");
    assert_eq!(ticket.bearer.as_str(), "test-device");
    worker.join().unwrap();
}
