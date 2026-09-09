//! End-to-end typed HTTP client coverage against a local, deterministic server.

use iris::codegen::build::execute_binary_for_eval;
use iris::compile_file_to_module;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "windows")]
#[test]
fn typed_http_client_sends_headers_body_and_returns_status_natively() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP server");
    let port = listener.local_addr().expect("local address").port();
    listener
        .set_nonblocking(true)
        .expect("make local listener nonblocking");

    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "HTTP client never connected");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept local HTTP request: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set request read timeout");

        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let count = stream.read(&mut chunk).expect("read HTTP request");
            if count == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..count]);
            let text = String::from_utf8_lossy(&request);
            let Some(headers_end) = text.find("\r\n\r\n") else {
                continue;
            };
            let content_length = text[..headers_end]
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length: ")
                        .or_else(|| line.strip_prefix("content-length: "))
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if request.len() >= headers_end + 4 + content_length {
                break;
            }
        }

        let request = String::from_utf8(request).expect("request is UTF-8");
        assert!(request.starts_with("POST /v1/chat HTTP/1.1\r\n"));
        assert!(request.contains("Authorization: Bearer local-test-token\r\n"));
        assert!(request.ends_with("{\"prompt\":\"hello\"}"));

        let body = "{\"accepted\":true}";
        let response = format!(
            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write HTTP response");
    });

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("iris_http_e2e_{}_{}", std::process::id(), nonce));
    std::fs::create_dir_all(&dir).expect("create test source directory");
    let source_path = dir.join("http_e2e.iris");
    let source = format!(
        r#"bring std.http

def main() -> i64 effect net, alloc, throw {{
    val request = HttpRequest {{
        method: "POST",
        url: "http://127.0.0.1:{port}/v1/chat",
        headers: "Authorization: Bearer local-test-token\r\n",
        body: "{{\"prompt\":\"hello\"}}",
        timeout_ms: 5000,
    }}
    val response = http_send(request)
    if is_err(response) {{ return 10 }}
    val value = unwrap(response)
    if value.status != 201 {{ return 11 }}
    if value.body != "{{\"accepted\":true}}" {{ return 12 }}
    return 0
}}
"#
    );
    std::fs::write(&source_path, source).expect("write IRIS HTTP test");

    let module = compile_file_to_module(&source_path).expect("compile typed HTTP client");
    let output = execute_binary_for_eval(&module).expect("execute native typed HTTP client");
    assert_eq!(output.trim(), "0");
    server.join().expect("local HTTP server");

    let _ = std::fs::remove_dir_all(dir);
}
