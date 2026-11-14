//! Host integration and assertion harnesses for IRIS embedded and daemon projects:
//! - `projects/learning_robot/uno_policy.iris` (host + interpreter assertions)
//! - `projects/realtime_embedded_actuator/controller.iris` + `test_controller.iris`
//!   (host assertions + ARM Cortex-M / ESP32 RISC-V allocation-free proofs)
//! - `projects/uno_controls/{simulate,uno_toggle,uno_modes,uno_pid}.iris`
//!   (host assertions + linking unmodified board entrypoints to a C BSP stub)
//! - `projects/intrusion_detection_system/server.iris` + `test_server.iris`
//!   (DPI/quarantine assertions + live loopback TCP client/server test)

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use iris::codegen::build::build_harness_binary;
use iris::codegen::embedded::{validate_embedded_module, EmbeddedTarget};
use iris::codegen::llvm_ir::HarnessCall;
use iris::compile_file_to_module;

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn c_compiler() -> Option<String> {
    if cfg!(target_os = "windows") {
        let mingw_gcc = r"C:\msys64\ucrt64\bin\gcc.exe";
        if Path::new(mingw_gcc).is_file() {
            return Some(mingw_gcc.to_owned());
        }
    }
    for cand in ["clang", "gcc", "cc"] {
        if Command::new(cand)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Some(cand.to_owned());
        }
    }
    None
}

fn compile_c_object(cc: &str, src: &Path, obj: &Path) {
    let mut cmd = Command::new(cc);
    if cfg!(target_os = "windows") && cc.contains("clang") {
        cmd.args(["-target", "x86_64-pc-windows-gnu"]);
    }
    let out = cmd
        .args([
            "-O2",
            "-c",
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ])
        .output()
        .expect("invoke C compiler");
    assert!(
        out.status.success(),
        "C compile failed for {}:\n{}",
        src.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn uno_policy_and_host_project_harnesses_run_natively_and_interpreted() {
    let cases = [
        ("projects/learning_robot/uno_policy.iris", None),
        (
            "projects/realtime_embedded_actuator/test_controller.iris",
            Some("realtime_embedded_actuator host harness ok"),
        ),
        (
            "projects/intrusion_detection_system/test_server.iris",
            Some("intrusion_detection_system server harness ok"),
        ),
        (
            "projects/uno_controls/simulate.iris",
            Some("uno controls: toggle, debounced modes and fixed-point PID checked"),
        ),
    ];

    let iris_bin = env!("CARGO_BIN_EXE_iris");
    for (rel, expected_line) in cases {
        let path = repo_path(rel);

        let native = Command::new(iris_bin)
            .args(["run", path.to_str().unwrap()])
            .output()
            .expect("run native iris");
        assert!(
            native.status.success(),
            "native run failed for {rel}:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&native.stdout),
            String::from_utf8_lossy(&native.stderr)
        );
        if let Some(needle) = expected_line {
            let stdout = String::from_utf8_lossy(&native.stdout);
            assert!(stdout.contains(needle), "{rel} native stdout: {stdout}");
        }

        let interp = Command::new(iris_bin)
            .args([path.to_str().unwrap(), "--emit", "eval"])
            .output()
            .expect("run interp iris");
        assert!(
            interp.status.success(),
            "interpreter run failed for {rel}:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&interp.stdout),
            String::from_utf8_lossy(&interp.stderr)
        );
        if let Some(needle) = expected_line {
            let stdout = String::from_utf8_lossy(&interp.stdout);
            assert!(stdout.contains(needle), "{rel} interp stdout: {stdout}");
        }
    }
}

#[test]
fn controller_remains_allocation_free_on_arm_and_riscv() {
    let module = compile_file_to_module(&repo_path(
        "projects/realtime_embedded_actuator/controller.iris",
    ))
    .expect("compile controller.iris");
    for target in [
        EmbeddedTarget::CortexM4F,
        EmbeddedTarget::CortexM33,
        EmbeddedTarget::Esp32C3,
        EmbeddedTarget::Esp32Xtensa,
    ] {
        let proof = validate_embedded_module(&module, target, "main")
            .unwrap_or_else(|e| panic!("embedded proof failed for {}: {e}", target.preset()));
        assert_eq!(proof.fixed_array_bytes, 32);
        assert!(!proof.fingerprint.is_empty());
        assert!(proof.reachable_functions.contains(&"main".to_string()));
    }
}

#[test]
fn uno_board_entrypoints_execute_with_simulated_host_bsp() {
    let Some(cc) = c_compiler() else {
        eprintln!("no C compiler found; skipping host BSP harness test");
        return;
    };

    let tmp = std::env::temp_dir().join(format!("iris_uno_bsp_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    // 1. uno_toggle.iris: 8 ticks checking rising-edge LED toggle on D13
    {
        let c_src = tmp.join("bsp_toggle.c");
        let obj = tmp.join("bsp_toggle.o");
        let exe = tmp.join(format!("uno_toggle_host{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(
            &c_src,
            r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static const int64_t BUTTON_SEQ[8] = {0, 1, 1, 0, 1, 0, 1, 0};
static const int64_t EXPECTED_LED[8] = {0, 1, 1, 1, 0, 0, 1, 1};
static int64_t step = 0;

int64_t iris_embedded_gpio_read(int64_t pin) {
    if (pin != 2 || step < 0 || step >= 8) { fprintf(stderr, "bad gpio read\n"); exit(2); }
    return BUTTON_SEQ[step];
}

int64_t iris_embedded_gpio_write(int64_t pin, int64_t value) {
    if (pin != 13 || step < 0 || step >= 8) { fprintf(stderr, "bad gpio write\n"); exit(3); }
    if (value != EXPECTED_LED[step]) {
        fprintf(stderr, "step %lld: got LED=%lld expected=%lld\n",
                (long long)step, (long long)value, (long long)EXPECTED_LED[step]);
        exit(4);
    }
    step++;
    if (step == 8) {
        printf("TOGGLE_BSP_OK\n");
        fflush(stdout);
    }
    return 0;
}
"#,
        )
        .unwrap();
        compile_c_object(&cc, &c_src, &obj);

        let mut ir = compile_file_to_module(&repo_path("projects/uno_controls/uno_toggle.iris"))
            .expect("compile uno_toggle.iris");
        let obj_str = obj.to_string_lossy().replace('\\', "/");
        for ext in &mut ir.extern_fns {
            ext.link_lib = Some(obj_str.clone());
        }
        let calls = vec![
            HarnessCall {
                function: "main".into(),
                args: Vec::new(),
            };
            8
        ];
        build_harness_binary(&ir, &calls, &exe).expect("link uno_toggle host BSP binary");
        let run = Command::new(&exe).output().expect("run uno_toggle host");
        assert!(
            run.status.success(),
            "uno_toggle BSP check failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(String::from_utf8_lossy(&run.stdout).contains("TOGGLE_BSP_OK"));
    }

    // 2. uno_modes.iris: bounce (15 ticks high + 45 low -> stays OFF) then press (45 high + 45 low -> turns ON)
    {
        let c_src = tmp.join("bsp_modes.c");
        let obj = tmp.join("bsp_modes.o");
        let exe = tmp.join(format!("uno_modes_host{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(
            &c_src,
            r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static int64_t adc_calls = 0;
static int64_t step = 0;

int64_t iris_embedded_adc_read(int64_t channel) {
    if (channel != 0) { exit(2); }
    adc_calls++;
    return 512;
}

int64_t iris_embedded_gpio_read(int64_t pin) {
    if (pin != 2) { exit(3); }
    // Steps 0..14: short 15-tick glitch (< DEBOUNCE_STEPS=40)
    if (step < 15) return 1;
    // Steps 15..59: 45 low
    if (step < 60) return 0;
    // Steps 60..104: 45 high (debounced press accepted)
    if (step < 105) return 1;
    // Steps 105..149: 45 low (debounced release advances OFF -> ON)
    return 0;
}

int64_t iris_embedded_gpio_write(int64_t pin, int64_t value) {
    if (pin != 13) { exit(4); }
    if (step < 60 && value != 0) {
        fprintf(stderr, "bounce leaked to D13 at step %lld\n", (long long)step);
        exit(5);
    }
    if (step == 149) {
        if (value != 1) {
            fprintf(stderr, "expected MODE_ON D13=1 after press+release, got %lld\n", (long long)value);
            exit(6);
        }
        if (adc_calls != 150) {
            fprintf(stderr, "expected 150 ADC ticks, got %lld\n", (long long)adc_calls);
            exit(7);
        }
        printf("MODES_BSP_OK\n");
        fflush(stdout);
    }
    step++;
    return 0;
}
"#,
        )
        .unwrap();
        compile_c_object(&cc, &c_src, &obj);

        let mut ir = compile_file_to_module(&repo_path("projects/uno_controls/uno_modes.iris"))
            .expect("compile uno_modes.iris");
        let obj_str = obj.to_string_lossy().replace('\\', "/");
        for ext in &mut ir.extern_fns {
            ext.link_lib = Some(obj_str.clone());
        }
        let calls = vec![
            HarnessCall {
                function: "main".into(),
                args: Vec::new(),
            };
            150
        ];
        build_harness_binary(&ir, &calls, &exe).expect("link uno_modes host BSP binary");
        let run = Command::new(&exe).output().expect("run uno_modes host");
        assert!(
            run.status.success(),
            "uno_modes BSP check failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(String::from_utf8_lossy(&run.stdout).contains("MODES_BSP_OK"));
    }

    // 3. uno_pid.iris: setpoint A1=600, plant A0 ramps 200 -> 600, verifies D9 PWM bounds & drop
    {
        let c_src = tmp.join("bsp_pid.c");
        let obj = tmp.join("bsp_pid.o");
        let exe = tmp.join(format!("uno_pid_host{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(
            &c_src,
            r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static const int64_t MEAS_SEQ[8] = {200, 300, 400, 500, 560, 590, 600, 600};
static int64_t step = 0;
static int64_t first_duty = -1;

int64_t iris_embedded_adc_read(int64_t channel) {
    if (step < 0 || step >= 8) { exit(2); }
    if (channel == 1) return 600;
    if (channel == 0) return MEAS_SEQ[step];
    exit(3);
}

int64_t iris_embedded_pwm_write(int64_t pin, int64_t duty) {
    if (pin != 9 || duty < 0 || duty > 255) {
        fprintf(stderr, "invalid pwm pin=%lld duty=%lld\n", (long long)pin, (long long)duty);
        exit(4);
    }
    if (step == 0) {
        first_duty = duty;
        if (duty != 255) {
            fprintf(stderr, "expected initial saturated duty=255, got %lld\n", (long long)duty);
            exit(5);
        }
    }
    if (step == 7) {
        if (duty >= first_duty) {
            fprintf(stderr, "expected settled duty < 255, got %lld\n", (long long)duty);
            exit(6);
        }
        printf("PID_BSP_OK\n");
        fflush(stdout);
    }
    step++;
    return 0;
}
"#,
        )
        .unwrap();
        compile_c_object(&cc, &c_src, &obj);

        let mut ir = compile_file_to_module(&repo_path("projects/uno_controls/uno_pid.iris"))
            .expect("compile uno_pid.iris");
        let obj_str = obj.to_string_lossy().replace('\\', "/");
        for ext in &mut ir.extern_fns {
            ext.link_lib = Some(obj_str.clone());
        }
        let calls = vec![
            HarnessCall {
                function: "main".into(),
                args: Vec::new(),
            };
            8
        ];
        build_harness_binary(&ir, &calls, &exe).expect("link uno_pid host BSP binary");
        let run = Command::new(&exe).output().expect("run uno_pid host");
        assert!(
            run.status.success(),
            "uno_pid BSP check failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(String::from_utf8_lossy(&run.stdout).contains("PID_BSP_OK"));
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

fn free_loopback_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().unwrap().port()
}

fn send_http_with_retry(port: u16, req: &str) -> String {
    let addr = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut stream = loop {
        match TcpStream::connect(&addr) {
            Ok(s) => break s,
            Err(e) => {
                assert!(
                    Instant::now() < deadline,
                    "timed out connecting to server on {addr}: {e}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();
    let _ = stream.shutdown(std::net::Shutdown::Write);

    let mut resp = String::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                resp.push_str(&String::from_utf8_lossy(&buf[..n]));
                if resp.contains("\r\n\r\n") && resp.contains('}') {
                    break;
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                if !resp.is_empty() {
                    break;
                }
                panic!("timed out reading HTTP response from {addr}");
            }
            Err(e) => panic!("read error from {addr}: {e}"),
        }
    }
    resp
}

#[test]
fn intrusion_detection_system_live_tcp_server_blocks_attacks_and_quarantines_ip() {
    let tmp = std::env::temp_dir().join(format!("iris_ids_live_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let server_exe = tmp.join(format!("aegis_server{}", std::env::consts::EXE_SUFFIX));

    // Build native binary first so startup timing is predictable.
    let build = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args([
            "-o",
            server_exe.to_str().unwrap(),
            "build",
            repo_path("projects/intrusion_detection_system/server.iris")
                .to_str()
                .unwrap(),
        ])
        .output()
        .expect("build server.iris");
    assert!(
        build.status.success(),
        "failed to build server.iris: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let port = free_loopback_port();
    let server = Command::new(&server_exe)
        .env("AEGIS_PORT", port.to_string())
        .env("AEGIS_MAX_REQUESTS", "4")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn aegis_server");

    let r1 = send_http_with_retry(
        port,
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Forwarded-For: 10.1.2.3\r\n\r\n",
    );
    assert!(r1.contains("HTTP/1.1 200 OK"), "r1={r1}");
    assert!(r1.contains("\"status\":\"HEALTHY\""), "r1={r1}");

    let r2 = send_http_with_retry(
        port,
        "GET /search?q=telemetry HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Forwarded-For: 10.1.2.3\r\n\r\n",
    );
    assert!(r2.contains("HTTP/1.1 200 OK"), "r2={r2}");
    assert!(r2.contains("asset_report.json"), "r2={r2}");

    let r3 = send_http_with_retry(
        port,
        "GET /%2e%2e%2f%2e%2e%2fetc%2fpasswd HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Forwarded-For: 198.51.100.7\r\n\r\n",
    );
    assert!(r3.contains("HTTP/1.1 403 Forbidden"), "r3={r3}");
    assert!(r3.contains("BLOCKED_BY_AEGIS_IDS"), "r3={r3}");
    assert!(r3.contains("CVE-2021-41773"), "r3={r3}");

    let r4 = send_http_with_retry(
        port,
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Forwarded-For: 198.51.100.7\r\n\r\n",
    );
    assert!(r4.contains("HTTP/1.1 403 Forbidden"), "r4={r4}");
    assert!(r4.contains("BLOCKED_QUARANTINED_IP"), "r4={r4}");

    let output = server.wait_with_output().expect("wait server exit");
    assert!(
        output.status.success(),
        "server exited abnormally: status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("gracefully stopped after 4 requests"),
        "stdout: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
