//! Debug Adapter Protocol (DAP) server for IRIS.
//!
//! [`run_dap_server`] implements the DAP JSON-RPC protocol over stdin/stdout,
//! allowing any DAP-compatible editor (VSCode, Neovim with nvim-dap, etc.) to
//! debug IRIS programs interactively.
//!
//! The server uses [`crate::debugger::DebugSession`] for trace-based debugging
//! with source-level breakpoints.
//!
//! ## Supported DAP capabilities
//!
//! - Breakpoints with conditions, hit counts, and log messages
//! - Step forward / backward (time-travel)
//! - Step over, step into, step out
//! - Watch expressions and hover evaluation
//! - Variable inspection and mutation (`setVariable`)
//! - Debug-console completions
//! - Exception breakpoint filters
//! - Stop-on-entry
//! - Loaded-sources request

use std::io::{Read, Write};

use crate::debugger::{BreakpointInfo, DebugSession};

/// Runs the DAP server, reading JSON-RPC messages from stdin and writing
/// responses/events to stdout. Blocks until the client sends `disconnect`.
///
/// Use `iris dap` to start this server; configure your editor to use
/// `iris dap` as the debug adapter command with adapter type `"iris"`.
pub fn run_dap_server() -> std::io::Result<()> {
    // Do not lock stdout for the lifetime of the server: `NativeBridge` relays
    // adapter messages to stdout from a background thread, and locking stdout
    // permanently across blocking stdin reads deadlocks native DAP sessions.
    run_dap_server_with_io(std::io::stdin().lock(), std::io::stdout())
}

/// Runs the DAP server over arbitrary [`Read`] / [`Write`] streams.
pub fn run_dap_server_with_io<R: Read, W: Write>(reader: R, writer: W) -> std::io::Result<()> {
    let mut reader = std::io::BufReader::new(reader);
    let writer = std::cell::RefCell::new(writer);
    let mut session = DebugSession::new();
    let mut seq = 1i64;
    let mut source_path = String::new();
    let mut source_content = String::new();
    let mut stop_on_entry = false;
    let mut entry_function: Option<String> = None;
    // Native sessions (`"native": true` in the launch configuration) are
    // relayed to a native DAP adapter; see `crate::dap_native`.
    let mut initialize_args = serde_json::Value::Null;
    let mut recorded_config: Vec<(String, serde_json::Value)> = Vec::new();
    let mut native: Option<crate::dap_native::NativeBridge> = None;

    loop {
        // Read Content-Length header.
        let mut content_length: usize = 0;
        loop {
            let mut byte = [0u8];
            let mut chars = String::new();
            loop {
                match reader.read_exact(&mut byte) {
                    Ok(()) => {}
                    // A closed stdin ends the session. Propagating the EOF made
                    // `main` print an error and exit 1, which a debug adapter
                    // client reads as a crash. Same defect as the LSP server had
                    // -- see known-issues #58.
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        if let Some(bridge) = native.take() {
                            bridge.finish();
                        }
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                }
                if byte[0] == b'\r' {
                    continue;
                }
                if byte[0] == b'\n' {
                    break;
                }
                chars.push(byte[0] as char);
            }
            if chars.is_empty() {
                break;
            }
            if chars.to_lowercase().starts_with("content-length:") {
                let val = chars["content-length:".len()..].trim();
                content_length = val.parse().unwrap_or(0);
            }
        }
        if content_length == 0 {
            continue;
        }

        let mut body = vec![0u8; content_length];
        match reader.read_exact(&mut body) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                if let Some(bridge) = native.take() {
                    bridge.finish();
                }
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        let body_str = String::from_utf8_lossy(&body);

        let msg: serde_json::Value = match serde_json::from_str(&body_str) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let request_seq = msg["seq"].as_i64().unwrap_or(0);
        let command = msg["command"].as_str().unwrap_or("");
        let arguments = msg
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let send = |body: serde_json::Value| -> std::io::Result<()> {
            let mut w = writer.borrow_mut();
            crate::dap_native::write_frame(&mut *w, &body)
        };

        if let Some(bridge) = native.as_mut() {
            match command {
                // IRIS's exception filter ("all" panics) has no native
                // equivalent; acknowledge it rather than fail the request.
                "setExceptionBreakpoints" => {
                    send(serde_json::json!({
                        "seq": seq, "type": "response", "request_seq": request_seq,
                        "success": true, "command": command, "body": {}
                    }))?;
                    seq += 1;
                }
                "stepBack" | "reverseContinue" if !bridge.supports_step_back() => {
                    send(serde_json::json!({
                        "seq": seq,
                        "type": "response",
                        "request_seq": request_seq,
                        "success": false,
                        "command": command,
                        "message": format!(
                            "{command} is not supported by this native debug adapter (reverse execution requires a target/adapter with supportsStepBack, e.g. rr or trace-based interpreter mode)"
                        )
                    }))?;
                    seq += 1;
                }
                "disconnect" => {
                    let _ = bridge.forward(&msg);
                    if let Some(bridge) = native.take() {
                        bridge.finish();
                    }
                    return Ok(());
                }
                _ => {
                    if let Err(error) = bridge.forward(&msg) {
                        send(serde_json::json!({
                            "seq": seq, "type": "response", "request_seq": request_seq,
                            "success": false, "command": command, "message": error
                        }))?;
                        seq += 1;
                    }
                }
            }
            continue;
        }

        match command {
            "initialize" => {
                initialize_args = arguments.clone();
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command,
                    "body": {
                        "supportsConfigurationDoneRequest": true,
                        "supportsEvaluateForHovers": true,
                        "supportsStepBack": true,
                        "supportsExceptionInfoRequest": true,
                        "supportsSetVariable": true,
                        "supportsValueFormattingOptions": false,
                        "supportsTerminateRequest": true,
                        "supportsRestartRequest": true,
                        "supportsCompletionsRequest": true,
                        "supportsModulesRequest": false,
                        "supportsLoadedSourcesRequest": true,
                        "supportsLogPoints": true,
                        "supportsConditionalBreakpoints": true,
                        "supportsHitConditionalBreakpoints": true,
                        "supportsBreakpointLocationsRequest": false,
                        "supportsStepInTargetsRequest": false,
                        "supportsSingleThreadExecutionRequests": false,
                        "exceptionBreakpointFilters": [
                            {
                                "filter": "all",
                                "label": "All Exceptions",
                                "description": "Break on all panics and runtime errors",
                                "default": true,
                                "supportsCondition": false,
                            }
                        ],
                    }
                }))?;
                seq += 1;
                // Send initialized event.
                send(serde_json::json!({
                    "seq": seq, "type": "event", "event": "initialized"
                }))?;
                seq += 1;
            }
            "launch" if arguments["native"].as_bool().unwrap_or(false) => {
                match crate::dap_native::NativeBridge::start(
                    &initialize_args,
                    &msg,
                    &recorded_config,
                ) {
                    Ok((bridge, description)) => {
                        // The native adapter answers this launch request.
                        native = Some(bridge);
                        send(serde_json::json!({
                            "seq": seq, "type": "event", "event": "output",
                            "body": {
                                "category": "console",
                                "output": format!("Native debugging via {description}\n")
                            }
                        }))?;
                        seq += 1;
                    }
                    Err(error) => {
                        send(serde_json::json!({
                            "seq": seq, "type": "response", "request_seq": request_seq,
                            "success": false, "command": command, "message": error
                        }))?;
                        seq += 1;
                    }
                }
            }
            "attach"
                if arguments["native"].as_bool().unwrap_or_else(|| {
                    arguments.get("processId").is_some()
                        || arguments.get("pid").is_some()
                        || arguments.get("target").is_some()
                }) =>
            {
                match crate::dap_native::NativeBridge::attach(
                    &initialize_args,
                    &msg,
                    &recorded_config,
                ) {
                    Ok((bridge, description)) => {
                        native = Some(bridge);
                        send(serde_json::json!({
                            "seq": seq, "type": "event", "event": "output",
                            "body": {
                                "category": "console",
                                "output": format!("Native debugging via {description}\n")
                            }
                        }))?;
                        seq += 1;
                    }
                    Err(error) => {
                        send(serde_json::json!({
                            "seq": seq, "type": "response", "request_seq": request_seq,
                            "success": false, "command": command, "message": error
                        }))?;
                        seq += 1;
                    }
                }
            }
            "attach" => {
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": false, "command": command,
                    "message": "attach requires native mode (set \"native\": true with \"processId\"/\"pid\" or \"target\"); trace-based interpreter DAP only supports \"launch\""
                }))?;
                seq += 1;
            }
            "launch" => {
                source_path = arguments["program"].as_str().unwrap_or("").to_owned();
                stop_on_entry = arguments["stopOnEntry"].as_bool().unwrap_or(false);
                entry_function = arguments["entryFunction"].as_str().map(str::to_owned);
                session.set_entry_function(entry_function.as_deref());
                if !source_path.is_empty() {
                    if let Ok(src) = std::fs::read_to_string(&source_path) {
                        source_content = src.clone();
                        session.set_source(&src);
                        session.set_entry_function(entry_function.as_deref());
                    }
                }
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command, "body": {}
                }))?;
                seq += 1;
                // Send output event for program start
                send(serde_json::json!({
                    "seq": seq, "type": "event", "event": "output",
                    "body": {
                        "category": "console",
                        "output": format!("Debugging: {}\n", source_path)
                    }
                }))?;
                seq += 1;
            }
            "setBreakpoints" => {
                recorded_config.push((command.to_owned(), arguments.clone()));
                // If the program has already been traced, only update breakpoints —
                // resetting the session would discard the execution trace and make
                // step/continue non-functional for the rest of the session.
                if session.trace_len() == 0 {
                    let old_source = source_content.clone();
                    session = DebugSession::new();
                    if !old_source.is_empty() {
                        session.set_source(&old_source);
                        session.set_entry_function(entry_function.as_deref());
                    }
                } else {
                    session.clear_breakpoints();
                }
                let bps = arguments["breakpoints"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut verified = Vec::new();
                for bp in &bps {
                    let line = bp["line"].as_u64().unwrap_or(0) as u32;
                    let condition = bp["condition"].as_str().map(|s| s.to_owned());
                    let hit_condition = bp["hitCondition"].as_str().map(|s| s.to_owned());
                    let log_message = bp["logMessage"].as_str().map(|s| s.to_owned());
                    let is_logpoint = log_message.is_some();
                    let info = BreakpointInfo {
                        condition,
                        hit_condition,
                        log_message,
                        hit_count: 0,
                    };
                    session.set_breakpoint(line, Some(info));
                    let mut bp_json = serde_json::json!({ "verified": true, "line": line });
                    if is_logpoint {
                        bp_json["message"] = serde_json::json!("Log point");
                    }
                    verified.push(bp_json);
                }
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command,
                    "body": { "breakpoints": verified }
                }))?;
                seq += 1;
            }
            "setExceptionBreakpoints" => {
                // Set break_on_exception based on whether "all" filter is active.
                let filters = arguments["filters"].as_array().cloned().unwrap_or_default();
                session.break_on_exception = filters.iter().any(|f| f.as_str() == Some("all"));
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command,
                    "body": {}
                }))?;
                seq += 1;
            }
            "configurationDone" => {
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command, "body": {}
                }))?;
                seq += 1;
                // Start execution.
                match session.start() {
                    Ok(()) => {
                        // Emit any log-point output first.
                        for log_msg in session.pending_logs.drain(..) {
                            send(serde_json::json!({
                                "seq": seq, "type": "event", "event": "output",
                                "body": { "category": "console", "output": format!("{}\n", log_msg) }
                            }))?;
                            seq += 1;
                        }

                        // Stop-on-entry: pause at the very first trace entry.
                        if let Some(frame) =
                            stop_on_entry.then(|| session.current_frame()).flatten()
                        {
                            send(serde_json::json!({
                                "seq": seq, "type": "event", "event": "stopped",
                                "body": {
                                    "reason": "entry",
                                    "threadId": 1,
                                    "allThreadsStopped": true,
                                    "line": frame.line,
                                    "description": "Stopped on entry",
                                    "text": format!("Paused at entry in {}", frame.func_name),
                                }
                            }))?;
                            seq += 1;
                        } else {
                            let hit = session.continue_to_breakpoint().is_some();
                            // Flush any log-point messages accumulated during continue.
                            let logs: Vec<String> = std::mem::take(&mut session.pending_logs);
                            for log_msg in logs {
                                send(serde_json::json!({
                                    "seq": seq, "type": "event", "event": "output",
                                    "body": { "category": "console", "output": format!("{}\n", log_msg) }
                                }))?;
                                seq += 1;
                            }
                            if hit {
                                let frame = session.current_frame().expect("current_frame present: guaranteed by is_some() check above or hit flag");
                                send(serde_json::json!({
                                    "seq": seq, "type": "event", "event": "stopped",
                                    "body": {
                                        "reason": "breakpoint",
                                        "threadId": 1,
                                        "allThreadsStopped": true,
                                        "line": frame.line,
                                        "description": format!("Breakpoint hit at line {}", frame.line),
                                        "text": format!("Paused in {}", frame.func_name),
                                    }
                                }))?;
                                seq += 1;
                            } else {
                                send(serde_json::json!({
                                    "seq": seq, "type": "event", "event": "output",
                                    "body": {
                                        "category": "console",
                                        "output": "Program completed without hitting any breakpoints.\n"
                                    }
                                }))?;
                                seq += 1;
                                send(serde_json::json!({
                                    "seq": seq, "type": "event", "event": "terminated"
                                }))?;
                                seq += 1;
                            }
                        }
                    }
                    Err(e) => {
                        // Use the rich error renderer for debug output
                        let error_msg = if !source_content.is_empty() {
                            crate::diagnostics::render_error(&source_content, &e)
                        } else {
                            format!("error: {}\n", e)
                        };
                        send(serde_json::json!({
                            "seq": seq, "type": "event", "event": "output",
                            "body": { "category": "stderr", "output": error_msg }
                        }))?;
                        seq += 1;
                        send(serde_json::json!({
                            "seq": seq, "type": "event", "event": "terminated"
                        }))?;
                        seq += 1;
                    }
                }
            }
            "continue" | "reverseContinue" => {
                let is_reverse = command == "reverseContinue";
                let body = if is_reverse {
                    serde_json::json!({})
                } else {
                    serde_json::json!({ "allThreadsContinued": true })
                };
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command, "body": body
                }))?;
                seq += 1;
                let hit = if is_reverse {
                    session.reverse_continue().is_some()
                } else {
                    session.continue_to_breakpoint().is_some()
                };
                let logs: Vec<String> = std::mem::take(&mut session.pending_logs);
                for log_msg in logs {
                    send(serde_json::json!({
                        "seq": seq, "type": "event", "event": "output",
                        "body": { "category": "console", "output": format!("{}\n", log_msg) }
                    }))?;
                    seq += 1;
                }
                if hit {
                    let frame = session.current_frame().expect(
                        "current_frame present: guaranteed by is_some() check above or hit flag",
                    );
                    send(serde_json::json!({
                        "seq": seq, "type": "event", "event": "stopped",
                        "body": {
                            "reason": "breakpoint",
                            "threadId": 1,
                            "line": frame.line,
                            "description": format!("Breakpoint hit at line {}", frame.line),
                        }
                    }))?;
                    seq += 1;
                } else if is_reverse {
                    let line = session.current_frame().map(|f| f.line).unwrap_or(0);
                    send(serde_json::json!({
                        "seq": seq, "type": "event", "event": "output",
                        "body": { "category": "console", "output": "Reached program start.\n" }
                    }))?;
                    seq += 1;
                    send(serde_json::json!({
                        "seq": seq, "type": "event", "event": "stopped",
                        "body": {
                            "reason": "entry",
                            "threadId": 1,
                            "line": line,
                            "description": "Already at program start",
                        }
                    }))?;
                    seq += 1;
                } else {
                    send(serde_json::json!({
                        "seq": seq, "type": "event", "event": "output",
                        "body": { "category": "console", "output": "Program finished.\n" }
                    }))?;
                    seq += 1;
                    send(serde_json::json!({
                        "seq": seq, "type": "event", "event": "terminated"
                    }))?;
                    seq += 1;
                }
            }
            "next" => {
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command, "body": {}
                }))?;
                seq += 1;
                let advanced = session.step_over();
                send_step_result(&send, &mut seq, &session, advanced)?;
            }
            "stepIn" => {
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command, "body": {}
                }))?;
                seq += 1;
                let advanced = session.step_into();
                send_step_result(&send, &mut seq, &session, advanced)?;
            }
            "stepOut" => {
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command, "body": {}
                }))?;
                seq += 1;
                let advanced = session.step_out();
                send_step_result(&send, &mut seq, &session, advanced)?;
            }
            "stepBack" => {
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command, "body": {}
                }))?;
                seq += 1;
                if session.step_back() {
                    let line = session.current_frame().map(|f| f.line).unwrap_or(0);
                    send(serde_json::json!({
                        "seq": seq, "type": "event", "event": "stopped",
                        "body": {
                            "reason": "step",
                            "threadId": 1,
                            "line": line,
                            "description": "Stepped back",
                        }
                    }))?;
                    seq += 1;
                } else {
                    send(serde_json::json!({
                        "seq": seq, "type": "event", "event": "stopped",
                        "body": { "reason": "step", "threadId": 1, "description": "Already at program start" }
                    }))?;
                    seq += 1;
                }
            }
            "stackTrace" => {
                let frames: Vec<serde_json::Value> = session
                    .all_visible_frames()
                    .into_iter()
                    .enumerate()
                    .map(|(idx, f)| {
                        let mut frame = serde_json::json!({
                            "id": idx,
                            "name": f.func_name,
                            "line": f.line,
                            "column": f.column,
                        });
                        if !source_path.is_empty() {
                            frame["source"] = serde_json::json!({
                                "name": std::path::Path::new(&source_path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| source_path.clone()),
                                "path": &source_path,
                            });
                        }
                        frame
                    })
                    .collect();
                let total = frames.len();
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command,
                    "body": { "stackFrames": frames, "totalFrames": total }
                }))?;
                seq += 1;
            }
            "scopes" => {
                let frame_id = arguments["frameId"].as_i64().unwrap_or(0);
                let var_ref = frame_id + 1;
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command,
                    "body": { "scopes": [
                        { "name": "Locals", "variablesReference": var_ref, "expensive": false },
                    ] }
                }))?;
                seq += 1;
            }
            "variables" => {
                let var_ref = arguments["variablesReference"].as_i64().unwrap_or(1);
                let frame_idx = (var_ref - 1) as usize;
                let frames = session.all_visible_frames();
                let vars: Vec<serde_json::Value> = frames
                    .get(frame_idx)
                    .map(|f| {
                        f.variables
                            .iter()
                            .map(|(name, val)| {
                                // Try to determine the type from the value string
                                let ty = if val.parse::<i64>().is_ok() {
                                    "i64"
                                } else if val.parse::<f64>().is_ok() {
                                    "f64"
                                } else if val == "true" || val == "false" {
                                    "bool"
                                } else {
                                    "str"
                                };
                                serde_json::json!({
                                    "name": name,
                                    "value": val,
                                    "type": ty,
                                    "variablesReference": 0,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command,
                    "body": { "variables": vars }
                }))?;
                seq += 1;
            }
            "threads" => {
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command,
                    "body": { "threads": [{ "id": 1, "name": "main" }] }
                }))?;
                seq += 1;
            }
            "evaluate" => {
                let expr = arguments["expression"].as_str().unwrap_or("").to_owned();
                let context = arguments["context"].as_str().unwrap_or("repl");
                // Build an eval source from the current debug frame variables + expression.
                let ctx_vars: Vec<(String, String)> = session
                    .current_frame()
                    .map(|f| f.variables.clone())
                    .unwrap_or_default();
                let result = evaluate_in_context(&ctx_vars, &expr);

                // For hover context, return a cleaner result
                let display_result = if context == "hover" && result.starts_with("(cannot evaluate")
                {
                    // Don't show error on hover
                    String::new()
                } else {
                    result
                };

                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": !display_result.is_empty(),
                    "command": command,
                    "body": { "result": display_result, "variablesReference": 0 }
                }))?;
                seq += 1;
            }
            "loadedSources" => {
                let mut sources = Vec::new();
                if !source_path.is_empty() {
                    sources.push(serde_json::json!({
                        "name": std::path::Path::new(&source_path)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| source_path.clone()),
                        "path": &source_path,
                    }));
                }
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command,
                    "body": { "sources": sources }
                }))?;
                seq += 1;
            }
            "exceptionInfo" => {
                let desc = session
                    .exception_message
                    .clone()
                    .unwrap_or_else(|| "IRIS runtime panic".to_owned());
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command,
                    "body": {
                        "exceptionId": "panic",
                        "description": desc,
                        "breakMode": if session.break_on_exception { "always" } else { "never" }
                    }
                }))?;
                seq += 1;
            }
            "setVariable" => {
                let name = arguments["name"].as_str().unwrap_or("");
                let value = arguments["value"].as_str().unwrap_or("");
                let ok = session.set_variable(name, value);
                if ok {
                    send(serde_json::json!({
                        "seq": seq, "type": "response", "request_seq": request_seq,
                        "success": true, "command": command,
                        "body": { "value": value }
                    }))?;
                } else {
                    send(serde_json::json!({
                        "seq": seq, "type": "response", "request_seq": request_seq,
                        "success": false, "command": command,
                        "message": format!("Variable '{}' not found in current scope", name),
                        "body": {}
                    }))?;
                }
                seq += 1;
            }
            "completions" => {
                let text = arguments["text"].as_str().unwrap_or("");
                let frame_vars = session.completions();
                let mut all_items: Vec<(String, &'static str)> = frame_vars
                    .into_iter()
                    .map(|name| (name, "variable"))
                    .collect();
                all_items.extend(
                    DEBUG_STATIC_COMPLETIONS
                        .iter()
                        .map(|(label, kind)| ((*label).to_owned(), *kind)),
                );
                all_items.sort_by(|a, b| a.0.cmp(&b.0));
                all_items.dedup_by(|a, b| a.0 == b.0);
                let items: Vec<serde_json::Value> = all_items
                    .iter()
                    .filter(|(n, _)| text.is_empty() || n.starts_with(text))
                    .map(|n| {
                        serde_json::json!({
                            "label": n.0,
                            "type": n.1,
                        })
                    })
                    .collect();
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command,
                    "body": { "targets": items }
                }))?;
                seq += 1;
            }
            "restart" => {
                // Re-read the file in case it changed on disk.
                if !source_path.is_empty() {
                    if let Ok(src) = std::fs::read_to_string(&source_path) {
                        source_content = src.clone();
                        session.set_source(&src);
                        session.set_entry_function(entry_function.as_deref());
                    }
                }
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command, "body": {}
                }))?;
                seq += 1;
                // Re-run.
                match session.start() {
                    Ok(()) => {
                        if stop_on_entry && session.current_frame().is_some() {
                            let frame = session.current_frame().expect("current_frame present: guaranteed by is_some() check above or hit flag");
                            send(serde_json::json!({
                                "seq": seq, "type": "event", "event": "stopped",
                                "body": { "reason": "entry", "threadId": 1, "line": frame.line }
                            }))?;
                        } else {
                            let hit = session.continue_to_breakpoint().is_some();
                            let logs: Vec<String> = std::mem::take(&mut session.pending_logs);
                            for log_msg in logs {
                                send(serde_json::json!({
                                    "seq": seq, "type": "event", "event": "output",
                                    "body": { "category": "console", "output": format!("{}\n", log_msg) }
                                }))?;
                                seq += 1;
                            }
                            if hit {
                                let frame = session.current_frame().expect("current_frame present: guaranteed by is_some() check above or hit flag");
                                send(serde_json::json!({
                                    "seq": seq, "type": "event", "event": "stopped",
                                    "body": { "reason": "breakpoint", "threadId": 1, "line": frame.line }
                                }))?;
                            } else {
                                send(serde_json::json!({
                                    "seq": seq, "type": "event", "event": "terminated"
                                }))?;
                            }
                        }
                        seq += 1;
                    }
                    Err(e) => {
                        let error_msg = if !source_content.is_empty() {
                            crate::diagnostics::render_error(&source_content, &e)
                        } else {
                            format!("error: {}\n", e)
                        };
                        send(serde_json::json!({
                            "seq": seq, "type": "event", "event": "output",
                            "body": { "category": "stderr", "output": error_msg }
                        }))?;
                        seq += 1;
                        send(serde_json::json!({
                            "seq": seq, "type": "event", "event": "terminated"
                        }))?;
                        seq += 1;
                    }
                }
            }
            "pause" => {
                // Trace-based debugger: "pause" stops at the current cursor.
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command, "body": {}
                }))?;
                seq += 1;
                if let Some(frame) = session.current_frame() {
                    send(serde_json::json!({
                        "seq": seq, "type": "event", "event": "stopped",
                        "body": { "reason": "pause", "threadId": 1, "line": frame.line }
                    }))?;
                    seq += 1;
                }
            }
            "disconnect" | "terminate" => {
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command, "body": {}
                }))?;
                break;
            }
            _ => {
                // Unknown command — respond with null body to avoid client stalling.
                send(serde_json::json!({
                    "seq": seq, "type": "response", "request_seq": request_seq,
                    "success": true, "command": command, "body": {}
                }))?;
                seq += 1;
            }
        }
    }
    Ok(())
}

const DEBUG_STATIC_COMPLETIONS: &[(&str, &str)] = &[
    ("print", "function"),
    ("to_str", "function"),
    ("len", "function"),
    ("concat", "function"),
    ("sqrt", "function"),
    ("abs", "function"),
    ("min", "function"),
    ("max", "function"),
    ("some", "function"),
    ("none", "value"),
    ("ok", "function"),
    ("err", "function"),
    ("is_some", "function"),
    ("is_ok", "function"),
    ("unwrap", "function"),
    ("list", "function"),
    ("list_len", "function"),
    ("list_get", "function"),
    ("map", "function"),
    ("map_get", "function"),
    ("channel", "function"),
    ("send", "function"),
    ("recv", "function"),
    ("true", "value"),
    ("false", "value"),
    ("val", "keyword"),
    ("var", "keyword"),
    ("if", "keyword"),
    ("else", "keyword"),
    ("when", "keyword"),
];

/// Evaluates an expression in the context of the current debug frame's variables.
/// Constructs a synthetic IRIS source with the variable bindings, then compiles and runs it.
fn evaluate_in_context(vars: &[(String, String)], expr: &str) -> String {
    if expr.trim().is_empty() {
        return String::new();
    }

    // Build val bindings from variable snapshot.
    let mut bindings = String::new();
    for (name, val_str) in vars {
        // Skip names that aren't valid identifiers.
        if name.chars().all(|c| c.is_alphanumeric() || c == '_') && !name.is_empty() {
            // Heuristic: if the value looks like a number or bool, use it directly; else wrap as str.
            let v = if val_str.parse::<i64>().is_ok()
                || val_str.parse::<f64>().is_ok()
                || val_str == "true"
                || val_str == "false"
            {
                val_str.clone()
            } else {
                format!("\"{}\"", val_str.replace('"', "\\\""))
            };
            bindings.push_str(&format!("    val {} = {};\n", name, v));
        }
    }

    // Try each return type.
    for ret_ty in &["i64", "f64", "bool", "str"] {
        let src = format!(
            "def __dbg_eval__() -> {} {{\n{}\n    {}\n}}\n",
            ret_ty, bindings, expr
        );
        if let Ok(result) = crate::compile(&src, "__dbg__", crate::EmitKind::Eval) {
            return result.trim_end_matches('\n').to_owned();
        }
    }

    format!("(cannot evaluate: {})", expr)
}

/// Sends the appropriate stopped/terminated event after a step operation.
fn send_step_result(
    send: &impl Fn(serde_json::Value) -> std::io::Result<()>,
    seq: &mut i64,
    session: &DebugSession,
    advanced: bool,
) -> std::io::Result<()> {
    if advanced {
        let line = session.current_frame().map(|f| f.line).unwrap_or(0);
        let func = session
            .current_frame()
            .map(|f| f.func_name.clone())
            .unwrap_or_default();
        send(serde_json::json!({
            "seq": *seq, "type": "event", "event": "stopped",
            "body": {
                "reason": "step",
                "threadId": 1,
                "line": line,
                "description": format!("Stepped to line {} in {}", line, func),
            }
        }))?;
        *seq += 1;
    } else {
        send(serde_json::json!({
            "seq": *seq, "type": "event", "event": "output",
            "body": { "category": "console", "output": "Program finished.\n" }
        }))?;
        *seq += 1;
        send(serde_json::json!({
            "seq": *seq, "type": "event", "event": "terminated"
        }))?;
        *seq += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frame(msg: serde_json::Value) -> Vec<u8> {
        let s = serde_json::to_string(&msg).unwrap();
        format!("Content-Length: {}\r\n\r\n{}", s.len(), s).into_bytes()
    }

    fn parse_frames(raw: &[u8]) -> Vec<serde_json::Value> {
        let mut msgs = Vec::new();
        let mut rest = raw;
        while !rest.is_empty() {
            let Some(pos) = rest.windows(4).position(|w| w == b"\r\n\r\n") else {
                break;
            };
            let header = std::str::from_utf8(&rest[..pos]).unwrap();
            let len: usize = header
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse().ok())
                })
                .unwrap();
            let body_start = pos + 4;
            let body = &rest[body_start..body_start + len];
            msgs.push(serde_json::from_slice(body).unwrap());
            rest = &rest[body_start + len..];
        }
        msgs
    }

    #[test]
    fn dap_initialize_launch_breakpoints_inspect_step_and_disconnect() {
        let dir = std::env::temp_dir().join("iris_dap_unit_test");
        std::fs::create_dir_all(&dir).unwrap();
        let src_path = dir.join("prog.iris");
        std::fs::write(
            &src_path,
            "def add_one(n: i64) -> i64 {\n    val r = n + 1;\n    r\n}\ndef main() -> i64 {\n    val x = 41;\n    val y = add_one(x);\n    y\n}\n",
        )
        .unwrap();

        let mut input = Vec::new();
        input.extend(frame(serde_json::json!({
            "seq": 1, "type": "request", "command": "initialize", "arguments": {}
        })));
        input.extend(frame(serde_json::json!({
            "seq": 2, "type": "request", "command": "launch",
            "arguments": { "program": src_path.to_string_lossy(), "stopOnEntry": true }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 3, "type": "request", "command": "setBreakpoints",
            "arguments": {
                "source": { "path": src_path.to_string_lossy() },
                "breakpoints": [{ "line": 7, "condition": "x == 41" }]
            }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 4, "type": "request", "command": "configurationDone"
        })));
        input.extend(frame(serde_json::json!({
            "seq": 5, "type": "request", "command": "threads"
        })));
        input.extend(frame(serde_json::json!({
            "seq": 6, "type": "request", "command": "stackTrace", "arguments": { "threadId": 1 }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 7, "type": "request", "command": "scopes", "arguments": { "frameId": 0 }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 8, "type": "request", "command": "variables", "arguments": { "variablesReference": 1 }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 9, "type": "request", "command": "evaluate", "arguments": { "expression": "1 + 2" }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 10, "type": "request", "command": "next", "arguments": { "threadId": 1 }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 11, "type": "request", "command": "stepIn", "arguments": { "threadId": 1 }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 12, "type": "request", "command": "continue", "arguments": { "threadId": 1 }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 13, "type": "request", "command": "disconnect"
        })));

        let mut output = Vec::new();
        run_dap_server_with_io(Cursor::new(input), &mut output).unwrap();

        let msgs = parse_frames(&output);
        let resp = |cmd: &str| {
            msgs.iter()
                .find(|m| m["type"] == "response" && m["command"] == cmd)
                .unwrap_or_else(|| panic!("missing response for {cmd}: {msgs:?}"))
        };

        assert_eq!(resp("initialize")["success"], true);
        assert_eq!(
            resp("initialize")["body"]["supportsEvaluateForHovers"],
            true
        );
        assert_eq!(resp("launch")["success"], true);
        assert_eq!(
            resp("setBreakpoints")["body"]["breakpoints"][0]["verified"],
            true
        );
        assert_eq!(resp("threads")["body"]["threads"][0]["id"], 1);
        assert!(resp("stackTrace")["body"]["stackFrames"]
            .as_array()
            .is_some());
        assert!(resp("scopes")["body"]["scopes"].as_array().is_some());
        assert_eq!(resp("evaluate")["body"]["result"], "3");
        assert_eq!(resp("next")["success"], true);
        assert_eq!(resp("stepIn")["success"], true);
        assert_eq!(resp("continue")["success"], true);
        assert_eq!(resp("disconnect")["success"], true);
        assert!(msgs
            .iter()
            .any(|m| m["type"] == "event" && m["event"] == "initialized"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dap_trace_reverse_continue_and_rejects_non_native_attach() {
        let dir = std::env::temp_dir().join("iris_dap_reverse_unit_test");
        std::fs::create_dir_all(&dir).unwrap();
        let src_path = dir.join("rev.iris");
        std::fs::write(
            &src_path,
            "def main() -> i64 {\n    var x = 1;\n    x = x + 1;\n    x = x + 2;\n    x = x + 3;\n    x\n}\n",
        )
        .unwrap();

        let mut input = Vec::new();
        input.extend(frame(serde_json::json!({
            "seq": 1, "type": "request", "command": "initialize", "arguments": {}
        })));
        input.extend(frame(serde_json::json!({
            "seq": 2, "type": "request", "command": "launch",
            "arguments": { "program": src_path.to_string_lossy(), "stopOnEntry": true }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 3, "type": "request", "command": "setBreakpoints",
            "arguments": {
                "source": { "path": src_path.to_string_lossy() },
                "breakpoints": [{ "line": 3 }, { "line": 5 }]
            }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 4, "type": "request", "command": "configurationDone"
        })));
        // Continue to line 3, then line 5.
        input.extend(frame(serde_json::json!({
            "seq": 5, "type": "request", "command": "continue", "arguments": { "threadId": 1 }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 6, "type": "request", "command": "continue", "arguments": { "threadId": 1 }
        })));
        // Step back one frame, then reverseContinue back to line 3, then to program start.
        input.extend(frame(serde_json::json!({
            "seq": 7, "type": "request", "command": "stepBack", "arguments": { "threadId": 1 }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 8, "type": "request", "command": "reverseContinue", "arguments": { "threadId": 1 }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 9, "type": "request", "command": "reverseContinue", "arguments": { "threadId": 1 }
        })));
        // Explicit non-native attach must fail cleanly rather than returning {}.
        input.extend(frame(serde_json::json!({
            "seq": 10, "type": "request", "command": "attach",
            "arguments": { "native": false, "processId": 1 }
        })));
        input.extend(frame(serde_json::json!({
            "seq": 11, "type": "request", "command": "disconnect"
        })));

        let mut output = Vec::new();
        run_dap_server_with_io(Cursor::new(input), &mut output).unwrap();
        let msgs = parse_frames(&output);

        let rev_responses: Vec<&serde_json::Value> = msgs
            .iter()
            .filter(|m| m["type"] == "response" && m["command"] == "reverseContinue")
            .collect();
        assert_eq!(rev_responses.len(), 2, "{msgs:?}");
        assert!(rev_responses.iter().all(|r| r["success"] == true));

        let stopped_reasons: Vec<(&str, i64)> = msgs
            .iter()
            .filter(|m| m["type"] == "event" && m["event"] == "stopped")
            .map(|m| {
                (
                    m["body"]["reason"].as_str().unwrap_or_default(),
                    m["body"]["line"].as_i64().unwrap_or(-1),
                )
            })
            .collect();
        assert!(
            stopped_reasons.contains(&("entry", 1)) || stopped_reasons.contains(&("entry", 2)),
            "expected reverseContinue to stop at program entry: {stopped_reasons:?}"
        );
        assert!(
            stopped_reasons.contains(&("breakpoint", 3)),
            "expected breakpoint hit on line 3: {stopped_reasons:?}"
        );

        let attach_resp = msgs
            .iter()
            .find(|m| m["type"] == "response" && m["command"] == "attach")
            .expect("attach response");
        assert_eq!(attach_resp["success"], false);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
