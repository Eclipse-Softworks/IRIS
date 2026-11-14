//! Developer commands: `iris check`, `iris watch`, `iris doctor`,
//! `iris new` and `iris bug-report`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

// ── iris check ─────────────────────────────────────────────────────────────

/// Type-check files without building: every diagnostic, no binary. A
/// directory checks every `.iris` file under it. Returns the exit code.
pub fn check(paths: &[PathBuf], json: bool, colored: bool) -> i32 {
    let roots: Vec<PathBuf> = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.to_vec()
    };
    let mut files = Vec::new();
    for root in &roots {
        if root.is_dir() {
            collect_iris_files(root, &mut files);
        } else {
            files.push(root.clone());
        }
    }
    files.sort();
    let started = Instant::now();
    let mut failed = 0usize;
    for file in &files {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", file.display());
                failed += 1;
                continue;
            }
        };
        if let Err(err) = crate::compile_file_to_module(file) {
            failed += 1;
            let name = file.display().to_string();
            if json {
                for diag in crate::diagnostics::error_to_diagnostics(&err) {
                    eprintln!("{}", diag.render_json_in(&source, Some(&name)));
                }
            } else {
                for diag in crate::diagnostics::error_to_diagnostics(&err) {
                    eprint!("{}", diag.render_in(&source, Some(&name), colored));
                }
            }
        }
    }
    if !json {
        let ms = started.elapsed().as_millis();
        let noun = if files.len() == 1 { "file" } else { "files" };
        if failed == 0 {
            eprintln!("checked {} {noun} in {ms} ms: no errors", files.len());
        } else {
            eprintln!(
                "checked {} {noun} in {ms} ms: {failed} with errors",
                files.len()
            );
        }
    }
    i32::from(failed > 0)
}

fn collect_iris_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | ".git" | "node_modules" | ".iris")
                && !name.starts_with('.')
            {
                collect_iris_files(&path, out);
            }
        } else if name.ends_with(".iris") {
            out.push(path);
        }
    }
}

// ── iris watch ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchMode {
    Run,
    Test,
    Check,
}

/// Rerun `iris run|test|check <target>` whenever a `.iris` file next to it
/// changes. A program still running from the previous change is stopped.
pub fn watch(target: &Path, mode: WatchMode) -> i32 {
    let dir = if target.is_dir() {
        target.to_path_buf()
    } else {
        target
            .parent()
            .map(Path::to_path_buf)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: cannot find the iris executable: {e}");
            return 1;
        }
    };
    let verb = match mode {
        WatchMode::Run => "run",
        WatchMode::Test => "test",
        WatchMode::Check => "check",
    };
    eprintln!(
        "watching {} -- `iris {verb} {}` on every change (Ctrl+C to stop)",
        dir.display(),
        target.display()
    );
    let mut seen = snapshot(&dir);
    let mut child: Option<Child> = None;
    let mut first = true;
    loop {
        let now = snapshot(&dir);
        if first || now != seen {
            let changed: Vec<String> = now
                .iter()
                .filter(|(p, t)| {
                    seen.iter()
                        .find(|(q, _)| q == p)
                        .map(|(_, u)| u != t)
                        .unwrap_or(true)
                })
                .map(|(p, _)| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect();
            seen = now;
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            let stamp = clock();
            if first {
                eprintln!("\n── {stamp} ──");
            } else {
                eprintln!("\n── {stamp}: {} changed ──", changed.join(", "));
            }
            first = false;
            match Command::new(&exe)
                .arg(verb)
                .arg(target)
                .stdin(Stdio::null())
                .spawn()
            {
                Ok(c) => child = Some(c),
                Err(e) => eprintln!("error: cannot start iris: {e}"),
            }
        }
        if let Some(c) = child.as_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                let ok = status.success();
                eprintln!(
                    "── {} ── waiting for changes",
                    if ok { "done" } else { "failed" }
                );
                child = None;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn snapshot(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    let mut files = Vec::new();
    collect_iris_files(dir, &mut files);
    let mut out: Vec<(PathBuf, SystemTime)> = files
        .into_iter()
        .filter_map(|f| {
            std::fs::metadata(&f)
                .and_then(|m| m.modified())
                .ok()
                .map(|t| (f, t))
        })
        .collect();
    out.sort();
    out
}

fn clock() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let s = secs % 86_400;
    format!("{:02}:{:02}:{:02} UTC", s / 3600, (s / 60) % 60, s % 60)
}

// ── iris doctor ────────────────────────────────────────────────────────────

struct Report {
    colored: bool,
    failures: usize,
}

impl Report {
    fn ok(&self, what: &str, detail: &str) {
        let mark = if self.colored {
            "\x1b[1;32m✔\x1b[0m"
        } else {
            "ok  "
        };
        println!("{mark} {what}: {detail}");
    }
    fn warn(&self, what: &str, detail: &str, fix: &str) {
        let mark = if self.colored {
            "\x1b[1;33m!\x1b[0m"
        } else {
            "warn"
        };
        println!("{mark} {what}: {detail}");
        if !fix.is_empty() {
            println!("    → {fix}");
        }
    }
    fn fail(&mut self, what: &str, detail: &str, fix: &str) {
        self.failures += 1;
        let mark = if self.colored {
            "\x1b[1;31m✘\x1b[0m"
        } else {
            "FAIL"
        };
        println!("{mark} {what}: {detail}");
        if !fix.is_empty() {
            println!("    → {fix}");
        }
    }
}

fn first_line(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    let text = if out.stdout.is_empty() {
        out.stderr
    } else {
        out.stdout
    };
    String::from_utf8_lossy(&text)
        .lines()
        .next()
        .map(|l| l.trim().to_string())
}

/// Check everything IRIS needs, end to end, and say how to fix what is
/// missing. Returns 1 when something required fails.
pub fn doctor(colored: bool) -> i32 {
    let mut r = Report {
        colored,
        failures: 0,
    };
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    println!("IRIS {} ({})", env!("CARGO_PKG_VERSION"), exe);
    let triple = crate::codegen::native_target_triple();
    println!("target {triple}\n");

    // Native toolchain.
    let tc = crate::codegen::build::toolchain_report();
    if tc.prebuilt_runtime {
        r.ok(
            "runtime",
            "prebuilt for this target (no C compiler needed for ordinary builds)",
        );
    } else {
        r.warn(
            "runtime",
            "no prebuilt runtime for this target; it is compiled with clang on first build",
            "",
        );
    }
    match first_line(&tc.clang, &["--version"]) {
        Some(v) => r.ok("clang", &format!("{v} ({})", tc.clang)),
        None if tc.prebuilt_runtime => r.warn(
            "clang",
            "not found",
            "needed only for `-g` debug builds, sanitizers and targets without a prebuilt runtime; `iris setup` installs one, or set IRIS_CLANG",
        ),
        None => r.fail("clang", "not found", "`iris setup` downloads a toolchain, or set IRIS_CLANG to clang's path"),
    }
    match &tc.lld {
        Some(lld) => r.ok("linker", &format!("ld.lld ({lld})")),
        None => r.warn("linker", "ld.lld not found; clang links instead", ""),
    }
    match crate::codegen::llvm_c_api::loaded_llvm_version() {
        Ok((a, b, c)) => r.ok("LLVM-C", &format!("{a}.{b}.{c}")),
        Err(e) => r.warn(
            "LLVM-C",
            &crate::codegen::build::strip_ansi(&e.to_string()).chars().take(160).collect::<String>(),
            "needed for `evolve-live`, `evolve-serve` and the JIT; set IRIS_LLVM_C_API to the LLVM shared library",
        ),
    }
    if crate::codegen::llvm_orc::is_orc_jit_available() {
        r.ok("ORC JIT", "available");
    } else {
        r.warn(
            "ORC JIT",
            "unavailable",
            "install LLVM (the shared library) to use `iris evolve-live` and `evolve-serve`",
        );
    }

    // End to end: build and run a program, natively and in the interpreter.
    let dir = std::env::temp_dir().join(format!("iris-doctor-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let src = dir.join("hello.iris");
    let _ = std::fs::write(
        &src,
        "def main() -> i64 {\n    println(\"hello from iris\");\n    return 0\n}\n",
    );
    let started = Instant::now();
    let native = Command::new(std::env::current_exe().unwrap_or_else(|_| "iris".into()))
        .args(["--emit", "eval"])
        .arg(&src)
        .env_remove("IRIS_FORCE_INTERP")
        .env_remove("IRIS_INTERP_FALLBACK")
        .output();
    let ms = started.elapsed().as_millis();
    match native {
        Ok(o)
            if o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains("hello from iris") =>
        {
            r.ok(
                "native build",
                &format!("hello world built and ran in {ms} ms"),
            );
        }
        Ok(o) => r.fail(
            "native build",
            &crate::codegen::build::strip_ansi(
                String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .last()
                    .unwrap_or("failed"),
            ),
            "the lines above name what is missing",
        ),
        Err(e) => r.fail("native build", &e.to_string(), ""),
    }
    let interp = Command::new(std::env::current_exe().unwrap_or_else(|_| "iris".into()))
        .args(["--emit", "eval"])
        .arg(&src)
        .env("IRIS_FORCE_INTERP", "1")
        .output();
    match interp {
        Ok(o)
            if o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains("hello from iris") =>
        {
            r.ok("interpreter", "hello world ran");
        }
        _ => r.fail(
            "interpreter",
            "hello world did not run",
            "run `iris bug-report` on any program and report it",
        ),
    }
    let _ = std::fs::remove_dir_all(&dir);

    // Debugger for `iris dap` / VS Code.
    match crate::dap_native::probe_native_debugger() {
        Ok(v) => r.ok("debugger", &v),
        Err((msg, hint)) => r.warn("debugger", &msg, hint),
    }

    // ROS 2, only when it looks intended.
    ros2_checks(&mut r);

    println!();
    if r.failures == 0 {
        println!("everything IRIS needs is in place");
        0
    } else {
        println!(
            "{} problem{} to fix (see → above)",
            r.failures,
            if r.failures == 1 { "" } else { "s" }
        );
        1
    }
}

fn ros2_checks(r: &mut Report) {
    let ros_dir = std::env::var_os("ROS2_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(if cfg!(windows) {
                r"C:\dev\ros2_humble\ros2-windows"
            } else {
                "/opt/ros/humble"
            })
        });
    let bridge_name = if cfg!(windows) {
        "iris_ros2.dll"
    } else {
        "libiris_ros2.so"
    };
    let bridge = std::env::var_os("IRIS_ROS2_LIB")
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .or_else(|| {
            let mut c = vec![PathBuf::from(bridge_name)];
            if let Ok(exe) = std::env::current_exe() {
                if let Some(d) = exe.parent() {
                    c.push(d.join(bridge_name));
                }
            }
            c.into_iter().find(|p| p.exists())
        });
    if !ros_dir.exists() && bridge.is_none() {
        return;
    }
    println!();
    println!("ROS 2");
    if ros_dir.exists() {
        r.ok("ROS 2 install", &ros_dir.display().to_string());
    } else {
        r.warn(
            "ROS 2 install",
            &format!("{} not found", ros_dir.display()),
            "set ROS2_DIR to the ROS 2 install",
        );
    }
    match &bridge {
        Some(b) => r.ok("bridge", &b.display().to_string()),
        None => r.warn(
            "bridge",
            &format!("{bridge_name} not built"),
            "`python build_ros2_bridge.py` builds it (docs/ros2-build.md)",
        ),
    }
    match std::env::var("RMW_IMPLEMENTATION") {
        Ok(v) => r.ok("RMW", &v),
        Err(_) => r.warn(
            "RMW",
            "RMW_IMPLEMENTATION unset; ROS 2 picks its default",
            if cfg!(windows) {
                "on Windows the validated choice is rmw_cyclonedds_cpp"
            } else {
                ""
            },
        ),
    }
    if cfg!(windows) && ros_dir.exists() {
        let plugins = ros_dir.join("bin").join("platforms");
        if std::env::var_os("QT_QPA_PLATFORM_PLUGIN_PATH").is_none() && plugins.exists() {
            r.warn(
                "Qt (turtlesim, rviz2)",
                "QT_QPA_PLATFORM_PLUGIN_PATH unset; GUI nodes exit at start",
                &format!("set QT_QPA_PLATFORM_PLUGIN_PATH={}", plugins.display()),
            );
        }
        if std::env::var_os("AMENT_PREFIX_PATH").is_none() {
            r.warn(
                "AMENT_PREFIX_PATH",
                "unset; ROS 2 executables started outside a sourced shell cannot find their resources",
                &format!("set AMENT_PREFIX_PATH={}, or call local_setup.bat first", ros_dir.display()),
            );
        }
    }
}

// ── iris bug-report ────────────────────────────────────────────────────────

/// Collect what is needed to report a compiler bug: the program (and the
/// local files it brings), the compiler's full output with internal detail,
/// and the environment. Nothing is sent anywhere.
pub fn bug_report(file: &Path, out: Option<&Path>, run: bool) -> i32 {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", file.display());
            return 1;
        }
    };
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(format!("iris-bug-report-{stamp}")));
    if let Err(e) = std::fs::create_dir_all(dir.join("source")) {
        eprintln!("error: cannot create {}: {e}", dir.display());
        return 1;
    }
    // The program and the local files it brings.
    let mut copied = Vec::new();
    copy_with_brings(file, &dir.join("source"), &mut copied);

    let exe = std::env::current_exe().unwrap_or_else(|_| "iris".into());
    let capture = |args: &[&str], interp: bool| -> String {
        let mut c = Command::new(&exe);
        c.args(args)
            .arg(file)
            .env("IRIS_DEBUG", "1")
            .env("IRIS_VERBOSE", "1");
        if interp {
            c.env("IRIS_FORCE_INTERP", "1");
        }
        match c.output() {
            Ok(o) => format!(
                "$ iris {} {}\nexit: {}\n--- stdout\n{}\n--- stderr\n{}\n",
                args.join(" "),
                file.display(),
                o.status,
                String::from_utf8_lossy(&o.stdout),
                crate::codegen::build::strip_ansi(&String::from_utf8_lossy(&o.stderr))
            ),
            Err(e) => format!("could not run iris: {e}\n"),
        }
    };
    let mut sections = vec![("compile.txt", capture(&["--emit", "ir"], false))];
    if run {
        sections.push(("native-run.txt", capture(&["--emit", "eval"], false)));
        sections.push(("interpreter-run.txt", capture(&["--emit", "eval"], true)));
    }
    let doctor = Command::new(&exe)
        .arg("doctor")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    sections.push((
        "environment.txt",
        format!(
            "iris {}\nos: {} {}\ntarget: {}\n\n{}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            crate::codegen::native_target_triple(),
            crate::codegen::build::strip_ansi(&doctor)
        ),
    ));
    for (name, text) in &sections {
        let _ = std::fs::write(dir.join(name), text);
    }
    let first_error = sections[0]
        .1
        .lines()
        .find(|l| l.starts_with("error"))
        .unwrap_or("(no error while compiling)")
        .to_string();
    let report = format!(
        "# IRIS bug report\n\n\
         - IRIS {} on {} ({})\n\
         - Program: `{}` ({} lines){}\n\
         - First error: `{}`\n\n\
         ## What happened\n\n(Describe what you expected and what happened instead.)\n\n\
         ## Files\n\n\
         - `source/`: the program and the local files it brings\n\
         - `compile.txt`: the compiler's output with internal detail (`IRIS_DEBUG=1`)\n{}\
         - `environment.txt`: versions and `iris doctor`\n\n\
         This folder contains your source code. Review it before sharing, then attach it to an issue at {}/issues.\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        crate::codegen::native_target_triple(),
        file.display(),
        source.lines().count(),
        if copied.len() > 1 { format!(", bringing {} more file(s)", copied.len() - 1) } else { String::new() },
        first_error,
        if run { "- `native-run.txt`, `interpreter-run.txt`: the program run both ways\n" } else { "" },
        env!("CARGO_PKG_REPOSITORY"),
    );
    let _ = std::fs::write(dir.join("REPORT.md"), report);
    println!("wrote {}", dir.display());
    println!(
        "It contains your source code: review it, then attach it to an issue at {}/issues",
        env!("CARGO_PKG_REPOSITORY")
    );
    if !run {
        println!("(add --run to also run the program natively and in the interpreter)");
    }
    0
}

fn copy_with_brings(file: &Path, dest: &Path, copied: &mut Vec<PathBuf>) {
    let Ok(canon) = std::fs::canonicalize(file) else {
        return;
    };
    if copied.contains(&canon) {
        return;
    }
    copied.push(canon.clone());
    let Some(name) = file.file_name() else { return };
    let _ = std::fs::copy(file, dest.join(name));
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let base = file.parent().unwrap_or(Path::new("."));
    for line in text.lines() {
        let line = line.trim_start();
        let Some(rest) = line
            .strip_prefix("bring \"")
            .or_else(|| line.strip_prefix("pub bring \""))
        else {
            continue;
        };
        let Some(path) = rest.split('"').next() else {
            continue;
        };
        let target = base.join(path);
        if target.exists() {
            copy_with_brings(&target, dest, copied);
        }
    }
}

// ── iris new ───────────────────────────────────────────────────────────────

/// The templates `iris new` knows, with a line on each.
pub const TEMPLATES: &[(&str, &str)] = &[
    ("app", "a program with tests (the default)"),
    (
        "cli",
        "a command-line tool: arguments parsed into a record, tested without a shell",
    ),
    (
        "server",
        "a TCP service on async sockets, with an end-to-end test",
    ),
    (
        "ros2",
        "a ROS 2 node: a timer, a publisher, a subscription and a parameter",
    ),
    (
        "controller",
        "a controller that `iris evolve-live` replaces while it runs",
    ),
];

const APP_MAIN: &str = include_str!("templates/app/main.iris");
const CLI_MAIN: &str = include_str!("templates/cli/main.iris");
const SERVER_MAIN: &str = include_str!("templates/server/main.iris");
const ROS2_MAIN: &str = include_str!("templates/ros2/main.iris");
const CART: &[(&str, &str)] = &[
    (
        "plant.iris",
        include_str!("../projects/evolving_cart_controller/plant.iris"),
    ),
    (
        "reference.iris",
        include_str!("../projects/evolving_cart_controller/reference.iris"),
    ),
    (
        "baseline.iris",
        include_str!("../projects/evolving_cart_controller/baseline.iris"),
    ),
    (
        "candidates/pd_with_history.iris",
        include_str!("../projects/evolving_cart_controller/candidates/pd_with_history.iris"),
    ),
    (
        "candidates/lqr_unclamped.iris",
        include_str!("../projects/evolving_cart_controller/candidates/lqr_unclamped.iris"),
    ),
    (
        "candidates/lqr_saturated.iris",
        include_str!("../projects/evolving_cart_controller/candidates/lqr_saturated.iris"),
    ),
    (
        "constitution.txt",
        include_str!("../projects/evolving_cart_controller/constitution.txt"),
    ),
    (
        "cases.json",
        include_str!("../projects/evolving_cart_controller/cases.json"),
    ),
];

const CONTROLLER_RUN_SH: &str = r#"#!/usr/bin/env bash
# Run the cart while its controller evolves.
set -euo pipefail
cd "$(dirname "$0")"
rm -f audit.jsonl audit.head.json
iris evolve-live --plant plant.iris --baseline baseline.iris \
  --candidate candidates/pd_with_history.iris \
  --candidate candidates/lqr_unclamped.iris \
  --candidate candidates/lqr_saturated.iris \
  --cases cases.json --constitution constitution.txt \
  --constitution-sha256 "$(sha256sum constitution.txt | cut -d' ' -f1)" \
  --audit audit.jsonl --audit-head audit.head.json \
  --min-output=-10 --max-output 10 --setpoints 1,5,-2,3 --hold 8 --offer-every 4 \
  --require-allocation-free
"#;

const CONTROLLER_RUN_PS1: &str = r#"# Run the cart while its controller evolves.
Set-Location $PSScriptRoot
Remove-Item -ErrorAction SilentlyContinue audit.jsonl, audit.head.json
$h = (Get-FileHash constitution.txt -Algorithm SHA256).Hash.ToLower()
iris evolve-live --plant plant.iris --baseline baseline.iris `
  --candidate candidates/pd_with_history.iris `
  --candidate candidates/lqr_unclamped.iris `
  --candidate candidates/lqr_saturated.iris `
  --cases cases.json --constitution constitution.txt --constitution-sha256 $h `
  --audit audit.jsonl --audit-head audit.head.json `
  --min-output=-10 --max-output 10 --setpoints 1,5,-2,3 --hold 8 --offer-every 4 `
  --require-allocation-free
"#;

/// Create `name/` from `template`. Returns the exit code.
pub fn new_project(template: &str, name: &str) -> i32 {
    let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        eprintln!("error: `{name}` is not a valid project name: use letters, digits and `_`, starting with a letter");
        return 2;
    }
    let dir = PathBuf::from(name);
    if dir.exists() {
        eprintln!("error: {} already exists", dir.display());
        return 1;
    }
    let fill = |text: &str| text.replace("{{name}}", name);
    let mut files: Vec<(String, String)> = Vec::new();
    let (entry, run, test) = match template {
        "app" => {
            files.push(("main.iris".into(), fill(APP_MAIN)));
            ("main.iris", "iris run main.iris", "iris test")
        }
        "cli" => {
            files.push(("main.iris".into(), fill(CLI_MAIN)));
            (
                "main.iris",
                "iris run main.iris -- --name Ada --times 2",
                "iris test",
            )
        }
        "server" => {
            files.push(("main.iris".into(), fill(SERVER_MAIN)));
            ("main.iris", "iris run main.iris", "iris test")
        }
        "ros2" => {
            files.push(("main.iris".into(), fill(ROS2_MAIN)));
            (
                "main.iris",
                "iris run main.iris    # needs ROS 2 and the IRIS bridge; `iris doctor` checks",
                "iris test",
            )
        }
        "controller" => {
            for (path, text) in CART {
                files.push((path.to_string(), text.to_string()));
            }
            files.push(("run.sh".into(), CONTROLLER_RUN_SH.into()));
            files.push(("run.ps1".into(), CONTROLLER_RUN_PS1.into()));
            (
                "reference.iris",
                "./run.sh    # or run.ps1 on Windows",
                "iris run reference.iris",
            )
        }
        other => {
            eprintln!("error: no template `{other}`; the templates are:");
            for (t, what) in TEMPLATES {
                eprintln!("  {t:<11} {what}");
            }
            return 2;
        }
    };
    files.push((
        "iris.toml".into(),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nentry = \"{entry}\"\n\n[dependencies]\n"),
    ));
    files.push((
        ".gitignore".into(),
        "*.exe\n*.o\ntarget/\naudit.jsonl\naudit.head.json\n".into(),
    ));
    let about = TEMPLATES
        .iter()
        .find(|(t, _)| *t == template)
        .map(|(_, w)| *w)
        .unwrap_or("");
    files.push((
        "README.md".into(),
        format!(
            "# {name}\n\n{}; made with `iris new {template}`.\n\n```\n{run}\n{test}\niris watch {entry}    # run again on every save\niris check            # errors without building\n```\n",
            capitalize(about)
        ),
    ));
    for (path, text) in &files {
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&full, text) {
            eprintln!("error: cannot write {}: {e}", full.display());
            return 1;
        }
    }
    println!("created {}/ from the `{template}` template:", dir.display());
    for (path, _) in &files {
        println!("  {path}");
    }
    println!("\nnext:\n  cd {name}\n  {run}\n  {test}");
    0
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
