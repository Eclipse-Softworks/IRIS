//! Native debugging behind `iris dap`.
//!
//! The trace debugger in [`crate::debugger`] replays the interpreter. A launch
//! configuration with `"native": true` instead builds the program with DWARF
//! debug information (`iris build -g`) and hands the session to a native DAP
//! adapter -- `gdb -i dap` (GDB 14+) by default, or whatever `IRIS_NATIVE_DAP`
//! names (for example `lldb-dap`). The bridge then relays every message in
//! both directions, so the editor talks to the native debugger through the
//! same `iris` adapter: breakpoints on `.iris` lines, stepping, call stacks
//! with IRIS function names, and scalar function arguments.
//!
//! The client has already completed `initialize` with `iris dap` (and may
//! have sent breakpoints) before `launch` reveals that the session is native,
//! so the bridge replays that state to the native adapter under private
//! sequence numbers and hides the replies.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

/// Requests the bridge injects use sequence numbers from here up; replies to
/// them are swallowed rather than relayed to the client.
const BRIDGE_SEQ_BASE: i64 = 1_000_000_000;

pub(crate) struct NativeBridge {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    next_seq: i64,
    work_dir: PathBuf,
    relay: Option<std::thread::JoinHandle<()>>,
    supports_step_back: Arc<AtomicBool>,
    is_gdb: bool,
    pending_launch_args: Option<Value>,
}

impl Drop for NativeBridge {
    fn drop(&mut self) {
        self.shutdown_child(false);
    }
}

impl NativeBridge {
    fn spawn(
        work_dir: PathBuf,
        initialize_args: &Value,
        program: Option<&Path>,
    ) -> Result<(Self, String), String> {
        let command = native_adapter_command(program)?;
        let description = command.join(" ");
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                format!("cannot start native debug adapter '{description}': {error}")
            })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let supports_step_back = Arc::new(AtomicBool::new(false));
        let relay_step_back = Arc::clone(&supports_step_back);
        let relay = std::thread::spawn(move || relay_to_client(stdout, relay_step_back));

        let is_gdb = Path::new(&command[0])
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("gdb"));
        let mut bridge = Self {
            child: Some(child),
            stdin: Some(stdin),
            next_seq: BRIDGE_SEQ_BASE,
            work_dir,
            relay: Some(relay),
            supports_step_back,
            is_gdb,
            pending_launch_args: None,
        };
        bridge.inject("initialize", initialize_args.clone())?;
        Ok((bridge, description))
    }

    /// Build `source_path` with debug info, start the native adapter, replay
    /// the client's `initialize` and recorded configuration, and forward the
    /// `launch` request (keeping its sequence number, so the adapter's reply
    /// reaches the client directly).
    pub(crate) fn start(
        initialize_args: &Value,
        launch_request: &Value,
        recorded: &[(String, Value)],
    ) -> Result<(Self, String), String> {
        let arguments = &launch_request["arguments"];
        let raw_source_path = PathBuf::from(arguments["program"].as_str().unwrap_or_default());
        let source_path = canonicalize_source_path(&raw_source_path);
        let stem = source_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("main")
            .to_owned();

        let work_dir = canonicalize_source_path(&std::env::temp_dir()).join(format!(
            "iris_dap_native_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&work_dir)
            .map_err(|error| format!("cannot create build directory: {error}"))?;
        let exe = work_dir.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX));
        let (module, source_map) = crate::compile_file_to_module_debug(&source_path)
            .map_err(|error| format!("compile failed: {error}"))?;
        let exe = crate::codegen::build::build_binary_debug(&module, &source_map, &exe)
            .map_err(|error| format!("native debug build failed: {error}"))?;

        let (mut bridge, description) = Self::spawn(work_dir, initialize_args, Some(&exe))?;

        let mut native_args = json!({
            "cwd": arguments["cwd"].as_str().map(str::to_owned).unwrap_or_else(|| {
                source_path
                    .parent()
                    .map(|dir| dir.to_string_lossy().into_owned())
                    .unwrap_or_default()
            }),
        });
        if !bridge.is_gdb {
            native_args["program"] = json!(exe.to_string_lossy());
        }
        if let Some(args) = arguments.get("args") {
            native_args["args"] = args.clone();
        }
        if arguments["stopOnEntry"].as_bool().unwrap_or(false) {
            // GDB's spelling; other adapters ignore unknown launch fields.
            native_args["stopAtBeginningOfMainSubprogram"] = json!(true);
            native_args["stopOnEntry"] = json!(true);
        }

        for (command, arguments) in recorded {
            bridge.inject(command, arguments.clone())?;
        }

        if bridge.is_gdb {
            // Defer sending `launch` to GDB until `configurationDone` so that
            // any `setBreakpoints` requests sent between `launch` and
            // `configurationDone` execute while GDB is still stopped (with
            // symbols already loaded from the `gdb -i dap <exe>` command line).
            // On GDB 14–15, `launch` immediately runs the inferior rather than
            // deferring to `configurationDone`.
            bridge.pending_launch_args = Some(native_args);
            let response = json!({
                "type": "response",
                "request_seq": launch_request["seq"],
                "success": true,
                "command": "launch",
            });
            let out = std::io::stdout();
            let mut lock = out.lock();
            write_frame(&mut lock, &response).map_err(|error| format!("stdout: {error}"))?;
        } else {
            let mut launch = launch_request.clone();
            launch["arguments"] = native_args;
            bridge.forward(&launch)?;
        }
        Ok((bridge, format!("{} (built {})", description, exe.display())))
    }

    /// Attach a native DAP adapter (`gdb -i dap` / `lldb-dap`) to an already-running process.
    pub(crate) fn attach(
        initialize_args: &Value,
        attach_request: &Value,
        recorded: &[(String, Value)],
    ) -> Result<(Self, String), String> {
        let arguments = &attach_request["arguments"];
        let mut native_args = json!({});
        if let Some(raw_pid) = arguments.get("processId").or_else(|| arguments.get("pid")) {
            let pid = raw_pid
                .as_u64()
                .or_else(|| raw_pid.as_str().and_then(|s| s.trim().parse::<u64>().ok()));
            if let Some(pid) = pid {
                native_args["pid"] = json!(pid);
            } else {
                native_args["pid"] = raw_pid.clone();
            }
        }
        if let Some(target) = arguments.get("target") {
            native_args["target"] = target.clone();
        }
        if native_args.get("pid").is_none() && native_args.get("target").is_none() {
            return Err(
                "native attach requires 'processId' / 'pid' (integer) or 'target' (e.g. 'localhost:3333')"
                    .into(),
            );
        }
        if let Some(program) = arguments.get("program") {
            native_args["program"] = program.clone();
        }
        if let Some(cwd) = arguments.get("cwd") {
            native_args["cwd"] = cwd.clone();
        }

        let work_dir = canonicalize_source_path(&std::env::temp_dir()).join(format!(
            "iris_dap_native_attach_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::create_dir_all(&work_dir);

        let attach_program = arguments["program"].as_str().map(Path::new);
        let (mut bridge, description) = Self::spawn(work_dir, initialize_args, attach_program)?;

        let mut attach = attach_request.clone();
        attach["arguments"] = native_args;
        bridge.forward(&attach)?;

        for (command, arguments) in recorded {
            bridge.inject(command, arguments.clone())?;
        }
        Ok((bridge, format!("{description} (attach)")))
    }

    /// Whether the underlying native adapter reported `supportsStepBack: true`.
    pub(crate) fn supports_step_back(&self) -> bool {
        self.supports_step_back.load(Ordering::Relaxed)
    }

    /// Relay a client message, normalizing breakpoint source paths to match DWARF `!DIFile`.
    pub(crate) fn forward(&mut self, message: &Value) -> Result<(), String> {
        if message["type"].as_str() == Some("request")
            && message["command"].as_str() == Some("configurationDone")
        {
            if let Some(launch_args) = self.pending_launch_args.take() {
                self.inject("launch", launch_args)?;
            }
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return Err("native adapter stdin is closed".into());
        };
        if message["type"].as_str() == Some("request")
            && message["command"].as_str() == Some("setBreakpoints")
        {
            let mut normalized = message.clone();
            normalize_dap_arguments("setBreakpoints", &mut normalized["arguments"]);
            return write_frame(stdin, &normalized)
                .map_err(|error| format!("native adapter: {error}"));
        }
        write_frame(stdin, message).map_err(|error| format!("native adapter: {error}"))
    }

    fn inject(&mut self, command: &str, mut arguments: Value) -> Result<(), String> {
        normalize_dap_arguments(command, &mut arguments);
        let seq = self.next_seq;
        self.next_seq += 1;
        self.forward(&json!({
            "seq": seq, "type": "request", "command": command, "arguments": arguments
        }))
    }

    /// After forwarding `disconnect`: wait for the adapter to finish its
    /// replies, then clean up.
    pub(crate) fn finish(mut self) {
        self.shutdown_child(true);
    }

    fn shutdown_child(&mut self, wait_gracefully: bool) {
        drop(self.stdin.take());
        if let Some(mut child) = self.child.take() {
            if wait_gracefully {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if std::time::Instant::now() < deadline => {
                            std::thread::sleep(std::time::Duration::from_millis(20))
                        }
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                    }
                }
            } else {
                match child.try_wait() {
                    Ok(Some(_)) => {}
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
            }
        }
        if let Some(relay) = self.relay.take() {
            let _ = relay.join();
        }
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

/// Canonicalize a source path and strip Windows `\\?\` verbatim prefixes so
/// paths sent in DAP `setBreakpoints` requests match DWARF `!DIFile` entries
/// produced by `compile_file_to_module_debug` (including on Windows runners
/// where `TEMP` uses 8.3 short names such as `RUNNER~1`).
fn canonicalize_source_path(path: &Path) -> PathBuf {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        let s = canonical.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            if !stripped.starts_with(r"UNC\") {
                return PathBuf::from(stripped);
            }
        }
    }
    canonical
}

fn normalize_dap_arguments(command: &str, arguments: &mut Value) {
    if command == "setBreakpoints" {
        if let Some(path_str) = arguments
            .pointer("/source/path")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let canonical = canonicalize_source_path(Path::new(&path_str));
            arguments["source"]["path"] = json!(canonical.to_string_lossy());
        }
    }
}

/// Probe the active native debug adapter for diagnostics such as `iris doctor`.
///
/// Returns a human-readable adapter/version summary on success, or an error
/// description paired with an actionable remediation hint when unavailable.
pub(crate) fn probe_native_debugger() -> Result<String, (String, &'static str)> {
    match native_adapter_command(None) {
        Ok(cmd) => {
            let desc = cmd.join(" ");
            let version = Command::new(&cmd[0])
                .arg("--version")
                .output()
                .ok()
                .and_then(|out| {
                    let text = if out.stdout.is_empty() {
                        out.stderr
                    } else {
                        out.stdout
                    };
                    String::from_utf8_lossy(&text)
                        .lines()
                        .next()
                        .map(|line| line.trim().to_string())
                        .filter(|line| !line.is_empty())
                });
            Ok(match version {
                Some(v) => format!("{v} ({desc})"),
                None => desc,
            })
        }
        Err(msg) => {
            let hint = if msg.contains("Python DLL") {
                "install GDB 14+ (MSYS2: `pacman -S mingw-w64-ucrt-x86_64-gdb`) or add LLDB's Python directory to PATH"
            } else {
                "install gdb (MSYS2: `pacman -S mingw-w64-ucrt-x86_64-gdb`) to debug native programs from VS Code"
            };
            Err((msg, hint))
        }
    }
}

/// The native DAP adapter to run: `IRIS_NATIVE_DAP` (a whitespace-separated
/// command line) if set, else GDB's built-in DAP mode, else LLDB's adapter.
///
/// When `program` is provided and GDB is selected, the executable path is
/// passed on the GDB command line (`gdb -i dap <exe>`) so GDB loads the symbol
/// table immediately at startup. Without this, GDB 14–15 rejects
/// `setBreakpoints` requests sent between `launch` and `configurationDone`
/// (`No symbol table is loaded. Use the "file" command.`) because `launch` in
/// `gdb/dap/launch.py` is a `DeferredRequest` that defers `file_command` until
/// `configurationDone`.
fn native_adapter_command(program: Option<&Path>) -> Result<Vec<String>, String> {
    if let Ok(command) = std::env::var("IRIS_NATIVE_DAP") {
        let mut parts: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
        if !parts.is_empty() {
            if let Some(exe) = program {
                let is_gdb_dap = Path::new(&parts[0])
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case("gdb"))
                    && parts.iter().any(|p| p == "dap");
                if is_gdb_dap {
                    parts.push(exe.to_string_lossy().into_owned());
                }
            }
            return Ok(parts);
        }
    }
    let mut gdb_candidates = vec![PathBuf::from("gdb")];
    if cfg!(windows) {
        gdb_candidates.push(PathBuf::from(r"C:\msys64\ucrt64\bin\gdb.exe"));
        gdb_candidates.push(PathBuf::from(r"C:\msys64\mingw64\bin\gdb.exe"));
    }
    for gdb in gdb_candidates {
        if gdb_supports_dap(&gdb) {
            let mut cmd = vec![
                gdb.to_string_lossy().into_owned(),
                "-i".into(),
                "dap".into(),
            ];
            if let Some(exe) = program {
                cmd.push(exe.to_string_lossy().into_owned());
            }
            return Ok(cmd);
        }
    }
    let mut lldb_candidates: Vec<PathBuf> = vec![
        PathBuf::from("lldb-dap"),
        PathBuf::from("lldb-dap-23"),
        PathBuf::from("lldb-vscode"),
    ];
    if cfg!(windows) {
        lldb_candidates.push(PathBuf::from(r"C:\llvm-23.1.1\bin\lldb-dap.exe"));
        lldb_candidates.push(PathBuf::from(r"C:\Program Files\LLVM\bin\lldb-dap.exe"));
        lldb_candidates.push(PathBuf::from(r"C:\msys64\ucrt64\bin\lldb-dap.exe"));
    } else {
        lldb_candidates.push(PathBuf::from("/usr/lib/llvm-23/bin/lldb-dap"));
    }
    let mut lldb_dll_missing = false;
    for lldb in lldb_candidates {
        if let Ok(output) = Command::new(&lldb)
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            if output.success() {
                return Ok(vec![lldb.to_string_lossy().into_owned()]);
            }
            // On Windows, LLVM LLDB builds linked against Python exit with
            // STATUS_DLL_NOT_FOUND (0xC0000135 / -1073741515) if Python is absent.
            if cfg!(windows) && output.code() == Some(-1073741515) {
                lldb_dll_missing = true;
            }
        }
    }
    if lldb_dll_missing {
        return Err("no usable native DAP adapter found: lldb-dap failed to start because its Python DLL was not found on PATH; install GDB 14+ (recommended on Windows) or add LLDB's Python directory to PATH".into());
    }
    Err("no native DAP adapter found: install GDB 14 or newer, or set IRIS_NATIVE_DAP (for example to 'lldb-dap')".into())
}

/// GDB gained `-i dap` in version 14.
fn gdb_supports_dap(gdb: &Path) -> bool {
    let Ok(output) = Command::new(gdb).arg("--version").output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .next()
        .and_then(|line| line.split_whitespace().last())
        .and_then(|version| version.split('.').next())
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 14)
}

pub(crate) fn write_frame(writer: &mut impl Write, message: &Value) -> std::io::Result<()> {
    let text = serde_json::to_string(message).unwrap_or_default();
    let frame = format!("Content-Length: {}\r\n\r\n{}", text.len(), text);
    writer.write_all(frame.as_bytes())?;
    writer.flush()
}

fn read_frame(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(str::trim)
        {
            length = value.parse::<usize>().ok();
        }
    }
    let mut body = vec![0u8; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Copy the adapter's messages to our stdout, minus replies to injected
/// requests and its own `initialized` event (the client already received
/// ours and must not configure the session twice).
fn relay_to_client(stdout: impl Read, supports_step_back: Arc<AtomicBool>) {
    let mut reader = BufReader::new(stdout);
    while let Some(message) = read_frame(&mut reader) {
        let injected_reply = message["type"] == "response"
            && message["request_seq"].as_i64().unwrap_or(0) >= BRIDGE_SEQ_BASE;
        if injected_reply {
            if message["command"] == "initialize" && message["success"] == true {
                let can_step_back = message["body"]["supportsStepBack"]
                    .as_bool()
                    .unwrap_or(false);
                supports_step_back.store(can_step_back, Ordering::Relaxed);
                let cap_event = json!({
                    "seq": message["seq"],
                    "type": "event",
                    "event": "capabilities",
                    "body": {
                        "capabilities": {
                            "supportsStepBack": can_step_back,
                        }
                    }
                });
                let out = std::io::stdout();
                let mut lock = out.lock();
                if write_frame(&mut lock, &cap_event).is_err() {
                    return;
                }
            }
            continue;
        }
        let duplicate_initialized = message["type"] == "event" && message["event"] == "initialized";
        if duplicate_initialized {
            continue;
        }
        let mut message = message;
        if message["type"] == "response"
            && message["command"] == "configurationDone"
            && message["message"] == "notStopped"
        {
            message["success"] = Value::Bool(true);
            if let Some(obj) = message.as_object_mut() {
                obj.remove("message");
            }
        }
        show_iris_names(&mut message);
        let out = std::io::stdout();
        let mut lock = out.lock();
        if write_frame(&mut lock, &message).is_err() {
            return;
        }
    }
}

/// Stack frames as the programmer named the functions: native code calls an
/// IRIS function `iris.<name>` (so it cannot collide with a C symbol), which
/// the debugger would otherwise show.
fn show_iris_names(message: &mut serde_json::Value) {
    let prefix = crate::codegen::llvm_ir::USER_SYMBOL_PREFIX;
    if let Some(frames) = message
        .get_mut("body")
        .and_then(|b| b.get_mut("stackFrames"))
        .and_then(|f| f.as_array_mut())
    {
        for frame in frames {
            if let Some(name) = frame.get("name").and_then(|n| n.as_str()).map(String::from) {
                if let Some(short) = name.strip_prefix(prefix) {
                    frame["name"] = serde_json::Value::String(short.to_string());
                }
            }
        }
    }
}
