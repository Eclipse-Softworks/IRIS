//! `iris dap` with `"native": true`: the adapter builds the program with
//! debug info and relays the session to GDB's DAP mode. Driven here exactly
//! as an editor would: initialize, launch, breakpoints, configurationDone,
//! then inspect the stopped frame and step.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

const PROGRAM: &str = "def control(error: f64, rate: f64) -> f64 {
    val p = 2.0 * error;
    val d = 0.5 * rate;
    p - d
}

def main() -> i64 {
    var total = 0.0;
    for i in 0..3 {
        total = total + control(1.0, 0.5);
    };
    print(total);
    0
}
";

/// A native DAP adapter `iris dap` can use: GDB 14+, lldb-dap, or an explicit
/// IRIS_NATIVE_DAP.
fn native_adapter_available() -> bool {
    std::env::var_os("IRIS_NATIVE_DAP").is_some()
        || gdb_with_dap()
        || Command::new("lldb-dap")
            .arg("--help")
            .output()
            .is_ok_and(|out| out.status.success())
}

fn gdb_with_dap() -> bool {
    let mut candidates = vec!["gdb".to_owned()];
    if cfg!(windows) {
        candidates.push(r"C:\msys64\ucrt64\bin\gdb.exe".to_owned());
    }
    candidates.iter().any(|gdb| {
        Command::new(gdb)
            .arg("--version")
            .output()
            .ok()
            .and_then(|out| {
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                let version = text.lines().next()?.split_whitespace().last()?.to_owned();
                version.split('.').next()?.parse::<u32>().ok()
            })
            .is_some_and(|major| major >= 14)
    })
}

struct Client {
    stdin: ChildStdin,
    inbox: Receiver<Value>,
    events: Vec<Value>,
    seq: i64,
}

impl Client {
    fn send(&mut self, command: &str, arguments: Value) -> i64 {
        self.seq += 1;
        let body = json!({
            "seq": self.seq, "type": "request", "command": command, "arguments": arguments
        })
        .to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        self.stdin.flush().unwrap();
        self.seq
    }

    fn wait(&mut self, what: &str, mut matches: impl FnMut(&Value) -> bool) -> Value {
        if let Some(pos) = self.events.iter().position(&mut matches) {
            return self.events.remove(pos);
        }
        loop {
            let message = self
                .inbox
                .recv_timeout(Duration::from_secs(90))
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
            if matches(&message) {
                return message;
            }
            self.events.push(message);
        }
    }

    fn response_raw(&mut self, request: i64) -> Value {
        self.wait("response", |m| {
            m["type"] == "response" && m["request_seq"].as_i64() == Some(request)
        })
    }

    fn response(&mut self, request: i64) -> Value {
        let message = self.response_raw(request);
        assert_eq!(message["success"], true, "{message}");
        message
    }

    fn top_frame(&mut self, thread: &Value) -> Value {
        let request = self.send("stackTrace", json!({ "threadId": thread }));
        self.response(request)["body"]["stackFrames"][0].clone()
    }
}

fn spawn_iris_dap() -> (std::process::Child, Client) {
    let mut adapter = Command::new(env!("CARGO_BIN_EXE_iris"))
        .arg("dap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start iris dap");
    let stdout = adapter.stdout.take().unwrap();
    let (sender, inbox) = channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut length = 0usize;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 {
                    return;
                }
                let header = header.trim();
                if header.is_empty() {
                    break;
                }
                if let Some(value) = header.strip_prefix("Content-Length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            if sender.send(serde_json::from_slice(&body).unwrap()).is_err() {
                return;
            }
        }
    });
    let client = Client {
        stdin: adapter.stdin.take().unwrap(),
        inbox,
        events: Vec::new(),
        seq: 0,
    };
    (adapter, client)
}

#[test]
fn native_launch_breaks_on_iris_lines_through_the_iris_adapter() {
    if !native_adapter_available() {
        assert!(
            std::env::var_os("IRIS_REQUIRE_NATIVE_DEBUGGER").is_none(),
            "IRIS_REQUIRE_NATIVE_DEBUGGER is set but no native DAP adapter was found"
        );
        eprintln!("no native DAP adapter (GDB 14+ or lldb-dap); native DAP test skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!("iris_dap_native_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("control.iris");
    std::fs::write(&source, PROGRAM).unwrap();
    let source = source.to_string_lossy().into_owned();

    let (mut adapter, mut client) = spawn_iris_dap();

    let initialize = client.send(
        "initialize",
        json!({ "adapterID": "iris", "pathFormat": "path" }),
    );
    client.response(initialize);
    let launch = client.send("launch", json!({ "program": source, "native": true }));
    let breakpoints = client.send(
        "setBreakpoints",
        json!({ "source": { "path": source }, "breakpoints": [{ "line": 3 }] }),
    );
    client.response(breakpoints);
    let done = client.send("configurationDone", json!({}));
    client.response(done);
    client.response(launch);

    let caps = client.wait("capabilities", |m| m["event"] == "capabilities");
    let supports_step_back = caps["body"]["capabilities"]["supportsStepBack"]
        .as_bool()
        .expect("capabilities event must carry supportsStepBack boolean");

    let stopped = client.wait("stopped", |m| m["event"] == "stopped");
    let thread = stopped["body"]["threadId"].clone();
    let frame = client.top_frame(&thread);
    assert_eq!(frame["name"], "control", "{frame}");
    assert_eq!(frame["line"], 3, "{frame}");
    assert_eq!(frame["source"]["name"], "control.iris", "{frame}");

    let scopes = client.send("scopes", json!({ "frameId": frame["id"] }));
    let scopes = client.response(scopes)["body"]["scopes"].clone();
    // Adapters group variables differently (GDB: "Arguments"/"Locals",
    // lldb-dap: "Locals"), so look through every non-register scope.
    let mut pairs: Vec<(String, String)> = Vec::new();
    for scope in scopes.as_array().unwrap() {
        if scope["name"]
            .as_str()
            .is_some_and(|n| n.contains("Register"))
        {
            continue;
        }
        let request = client.send(
            "variables",
            json!({ "variablesReference": scope["variablesReference"] }),
        );
        let variables = client.response(request)["body"]["variables"].clone();
        for v in variables.as_array().unwrap() {
            pairs.push((
                v["name"].as_str().unwrap_or_default().into(),
                v["value"].as_str().unwrap_or_default().into(),
            ));
        }
    }
    assert!(pairs.contains(&("error".into(), "1".into())), "{pairs:?}");
    assert!(pairs.contains(&("rate".into(), "0.5".into())), "{pairs:?}");

    if !supports_step_back {
        let step_back = client.send("stepBack", json!({ "threadId": thread }));
        let step_back_resp = client.response_raw(step_back);
        assert_eq!(step_back_resp["success"], false, "{step_back_resp}");
        assert!(
            step_back_resp["message"]
                .as_str()
                .is_some_and(|m| m.contains("not supported")),
            "{step_back_resp}"
        );
        let rev_cont = client.send("reverseContinue", json!({ "threadId": thread }));
        let rev_cont_resp = client.response_raw(rev_cont);
        assert_eq!(rev_cont_resp["success"], false, "{rev_cont_resp}");
    }

    let next = client.send("next", json!({ "threadId": thread }));
    client.response(next);
    client.wait("step", |m| m["event"] == "stopped");
    assert_eq!(client.top_frame(&thread)["line"], 4);

    client.send("disconnect", json!({ "terminateDebuggee": true }));
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while adapter.try_wait().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "iris dap did not exit"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn native_attach_by_pid_connects_and_reads_threads() {
    if !native_adapter_available() {
        assert!(
            std::env::var_os("IRIS_REQUIRE_NATIVE_DEBUGGER").is_none(),
            "IRIS_REQUIRE_NATIVE_DEBUGGER is set but no native DAP adapter was found"
        );
        eprintln!("no native DAP adapter (GDB 14+ or lldb-dap); native attach test skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "iris_dap_native_attach_test_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("sleepy.iris");
    std::fs::write(
        &source,
        "def main() -> i64 {\n    for i in 0..400 {\n        sleep_ms(50);\n    };\n    0\n}\n",
    )
    .unwrap();
    let (module, map) = iris::compile_file_to_module_debug(&source).expect("debug compile");
    let exe = iris::codegen::build::build_binary_debug(
        &module,
        &map,
        &dir.join(format!("sleepy{}", std::env::consts::EXE_SUFFIX)),
    )
    .expect("debug build");

    let mut target = Command::new(&exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn target process");
    std::thread::sleep(Duration::from_millis(150));
    assert!(target.try_wait().unwrap().is_none(), "target exited early");

    let (mut adapter, mut client) = spawn_iris_dap();
    let initialize = client.send(
        "initialize",
        json!({ "adapterID": "iris", "pathFormat": "path" }),
    );
    client.response(initialize);
    let attach = client.send(
        "attach",
        json!({
            "native": true,
            "processId": target.id(),
            "program": exe.to_string_lossy(),
        }),
    );
    let done = client.send("configurationDone", json!({}));
    let attach_resp = client.response_raw(attach);
    let _ = client.response_raw(done);

    if cfg!(windows) {
        assert_eq!(attach_resp["success"], true, "{attach_resp}");
        let threads = client.send("threads", json!({}));
        let threads_resp = client.response(threads);
        assert!(
            threads_resp["body"]["threads"]
                .as_array()
                .is_some_and(|t| !t.is_empty()),
            "expected live thread after attach: {threads_resp}"
        );
    } else if attach_resp["success"] != true {
        // On Linux with Yama ptrace_scope > 0, sibling attach may fail with
        // ptrace EPERM; verify it attempted native attach cleanly without hanging.
        let msg = attach_resp["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains("ptrace") || msg.contains("permitted") || msg.contains("attach"),
            "{attach_resp}"
        );
    }

    client.send("disconnect", json!({ "terminateDebuggee": true }));
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while adapter.try_wait().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "iris dap did not exit"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = target.kill();
    let _ = target.wait();
    let _ = std::fs::remove_dir_all(dir);
}
