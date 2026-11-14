//! `projects/llm_agent`: the agent over real HTTP, against a mock server that
//! speaks the OpenAI chat-completions protocol, on both backends. The mock
//! scripts the model's turns and records every request, so the test checks
//! what the agent sent: the tool table, its own turns, and tool results tied
//! to their call ids.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

fn turn(i: usize) -> String {
    let message = match i {
        0 => serde_json::json!({"role": "assistant", "content": null, "tool_calls": [
            {"id": "call_a", "type": "function",
             "function": {"name": "stock_level", "arguments": "{\"sku\":\"A-100\"}"}}]}),
        1 => serde_json::json!({"role": "assistant", "content": null, "tool_calls": [
            {"id": "call_b", "type": "function",
             "function": {"name": "reorder_stock", "arguments": "{\"sku\":\"A-100\",\"quantity\":50}"}}]}),
        _ => {
            serde_json::json!({"role": "assistant", "content": "Ordered 50 of A-100 as R-1; it arrives in 3 days."})
        }
    };
    serde_json::json!({"id": format!("chatcmpl-{i}"), "object": "chat.completion",
                       "choices": [{"index": 0, "message": message, "finish_reason": "stop"}]})
    .to_string()
}

/// Serve scripted turns until three requests have been answered; returns
/// the port and the request bodies as they arrive.
fn mock_model() -> (
    u16,
    Arc<Mutex<Vec<serde_json::Value>>>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let server = std::thread::spawn(move || {
        for i in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut chunk = [0u8; 8192];
            let body = loop {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0, "the client closed before its request ended");
                raw.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&raw).into_owned();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break body[..length].to_owned();
                    }
                }
            };
            seen.lock()
                .unwrap()
                .push(serde_json::from_str(&body).expect("the agent sent JSON"));
            let reply = turn(i);
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        }
    });
    (port, requests, server)
}

fn check(requests: &[serde_json::Value], stdout: &str, backend: &str) {
    assert_eq!(requests.len(), 3, "{backend}");
    let first = &requests[0];
    assert_eq!(first["model"], "mock-model", "{backend}");
    let tools: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        tools,
        ["stock_level", "reorder_stock", "shipping_quote"],
        "{backend}"
    );
    assert_eq!(
        first["tools"][1]["function"]["parameters"]["required"],
        serde_json::json!(["sku", "quantity"])
    );
    assert_eq!(first["messages"][1]["role"], "user");

    // Second request: the model's own turn, then the typed stock result for call_a.
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_eq!(messages[2]["role"], "assistant", "{backend}");
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call_a", "{backend}");
    assert_eq!(messages[3]["role"], "tool", "{backend}");
    assert_eq!(messages[3]["tool_call_id"], "call_a", "{backend}");
    let stock: serde_json::Value =
        serde_json::from_str(messages[3]["content"].as_str().unwrap()).unwrap();
    assert_eq!(stock["on_hand"], 12, "{backend}");

    // Third request: the receipt for call_b.
    let messages = requests[2]["messages"].as_array().unwrap();
    assert_eq!(messages[5]["tool_call_id"], "call_b", "{backend}");
    let receipt: serde_json::Value =
        serde_json::from_str(messages[5]["content"].as_str().unwrap()).unwrap();
    assert_eq!(receipt["order_id"], "R-1", "{backend}");
    assert_eq!(receipt["quantity"], 50, "{backend}");

    assert!(
        stdout
            .contains("answered after 3 turns: Ordered 50 of A-100 as R-1; it arrives in 3 days."),
        "{backend}: {stdout}"
    );
}

#[test]
fn the_agent_speaks_the_tool_calling_protocol_over_http_on_both_backends() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main = root.join("projects/llm_agent/main.iris");
    let question = "Check A-100 and reorder if we have fewer than 20 on hand.";

    let (port, requests, server) = mock_model();
    let run = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--emit", "eval"])
        .arg(&main)
        .args(["--", question])
        .env("IRIS_FORCE_INTERP", "1")
        .env(
            "IRIS_CHAT_URL",
            format!("http://127.0.0.1:{port}/v1/chat/completions"),
        )
        .env("IRIS_MODEL", "mock-model")
        .env_remove("IRIS_LLM_KEY_ENV")
        .output()
        .expect("run the agent");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    assert!(
        run.status.success(),
        "interpreter: {stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
    server.join().unwrap();
    check(&requests.lock().unwrap(), &stdout, "interpreter");

    let exe = std::env::temp_dir().join(format!(
        "iris_llm_agent_{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let build = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--emit", "binary", "-o"])
        .arg(&exe)
        .arg(&main)
        .output()
        .expect("build the agent");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let (port, requests, server) = mock_model();
    let run = Command::new(&exe)
        .arg(question)
        .env(
            "IRIS_CHAT_URL",
            format!("http://127.0.0.1:{port}/v1/chat/completions"),
        )
        .env("IRIS_MODEL", "mock-model")
        .env_remove("IRIS_LLM_KEY_ENV")
        .output()
        .expect("run the native agent");
    let _ = std::fs::remove_file(&exe);
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    assert!(
        run.status.success(),
        "native: {stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
    server.join().unwrap();
    check(&requests.lock().unwrap(), &stdout, "native");
}
