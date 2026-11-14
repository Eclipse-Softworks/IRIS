#[cfg(windows)]
use iris::sandbox::current_process_is_appcontainer;
use iris::sandbox::{
    CommandSpec, LimitViolation, NativeSandbox, ResourceLimits, ResourceSupervisor,
    SandboxAvailability, SandboxError,
};
use std::path::PathBuf;
use std::time::Duration;

/// Whether an enforced backend exists. CI sets IRIS_REQUIRE_NATIVE_SANDBOX=1
/// on platforms that must have one, so a broken backend fails instead of
/// silently skipping every enforcement test.
fn enforced_or_skip() -> bool {
    let availability = NativeSandbox::detect().availability().clone();
    if availability.is_enforced() {
        return true;
    }
    assert!(
        std::env::var_os("IRIS_REQUIRE_NATIVE_SANDBOX").is_none(),
        "IRIS_REQUIRE_NATIVE_SANDBOX is set but no backend is enforced: {availability:?}"
    );
    eprintln!("no enforced native sandbox here ({availability:?}); skipped");
    false
}

#[test]
fn resource_policy_rejects_disabled_or_excessive_limits() {
    let limits = ResourceLimits {
        memory_bytes: 0,
        ..ResourceLimits::default()
    };
    assert!(limits.validate().is_err());

    let limits = ResourceLimits {
        process_count: 65,
        ..ResourceLimits::default()
    };
    assert!(limits.validate().is_err());
}

#[test]
fn supervisor_reports_wall_and_combined_output_violations() {
    let limits = ResourceLimits {
        wall_time_ms: 10,
        output_bytes: 8,
        ..ResourceLimits::default()
    };
    let mut supervisor = ResourceSupervisor::start(limits).unwrap();
    assert!(supervisor.check_elapsed(Duration::from_millis(10)).is_ok());
    assert!(matches!(
        supervisor
            .check_elapsed(Duration::from_millis(11))
            .unwrap_err(),
        LimitViolation::WallTime { .. }
    ));

    assert_eq!(supervisor.account_output(4).unwrap(), 4);
    assert_eq!(supervisor.account_output(4).unwrap(), 8);
    assert!(matches!(
        supervisor.account_output(1).unwrap_err(),
        LimitViolation::Output { .. }
    ));
}

#[test]
fn incomplete_platform_boundary_is_never_accepted_as_sandboxed() {
    let sandbox = NativeSandbox::detect();
    match sandbox.availability() {
        SandboxAvailability::Enforced {
            capability_isolation,
            resource_limits,
        } => {
            assert!(!capability_isolation.is_empty());
            assert!(!resource_limits.is_empty());
        }
        SandboxAvailability::Unavailable { reason } => {
            assert!(!reason.is_empty());
            assert!(!sandbox.availability().is_enforced());
        }
    }
}

#[test]
fn launch_preparation_fails_closed_when_secure_backend_is_unavailable() {
    let absolute = std::env::current_exe().unwrap_or_else(|_| {
        let mut path = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        path.push("System32");
        path.push("cmd.exe");
        path
    });
    let command = CommandSpec::new(absolute, Vec::new()).unwrap();
    let sandbox = NativeSandbox::detect();
    let result = sandbox.prepare_launch(&command, ResourceLimits::default());
    if sandbox.availability().is_enforced() {
        assert!(result.is_ok());
    } else {
        assert!(matches!(result, Err(SandboxError::Unavailable(_))));
    }
}

#[test]
fn command_spec_rejects_relative_paths_and_nul_arguments() {
    assert!(matches!(
        CommandSpec::new("worker.exe", Vec::new()).unwrap_err(),
        SandboxError::InvalidCommand(_)
    ));

    let absolute = std::env::current_dir().unwrap().join("worker.exe");
    assert!(matches!(
        CommandSpec::new(absolute, vec!["bad\0argument".into()]).unwrap_err(),
        SandboxError::InvalidCommand(_)
    ));
}

#[cfg(windows)]
#[test]
fn appcontainer_job_executes_and_captures_a_native_worker() {
    let worker = std::env::current_exe().unwrap();
    let command = CommandSpec::new(
        worker,
        vec![
            "sandbox_worker_reports_appcontainer".into(),
            "--ignored".into(),
            "--exact".into(),
            "--nocapture".into(),
        ],
    )
    .unwrap();
    let sandbox = NativeSandbox::detect();
    let launch = sandbox
        .prepare_launch(&command, ResourceLimits::default())
        .unwrap();
    let output = launch.run().unwrap();

    assert_eq!(
        output.exit_code,
        0,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok"));
    assert!(output.stderr.is_empty());
    assert!(output.output_bytes >= output.stdout.len() as u64);
    assert!(output.peak_memory_bytes <= ResourceLimits::default().memory_bytes);
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn appcontainer_job_terminates_output_limit_violations() {
    if !enforced_or_skip() {
        return;
    }
    let command = CommandSpec::new(
        std::env::current_exe().unwrap(),
        vec![
            "sandbox_worker_emits_output".into(),
            "--ignored".into(),
            "--exact".into(),
            "--nocapture".into(),
        ],
    )
    .unwrap();
    let limits = ResourceLimits {
        output_bytes: 128,
        ..ResourceLimits::default()
    };
    let error = NativeSandbox::detect()
        .prepare_launch(&command, limits)
        .unwrap()
        .run()
        .unwrap_err();
    assert!(matches!(
        error,
        SandboxError::LimitExceeded(LimitViolation::Output { .. })
    ));
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn appcontainer_job_terminates_wall_time_violations() {
    if !enforced_or_skip() {
        return;
    }
    let command = CommandSpec::new(
        std::env::current_exe().unwrap(),
        vec![
            "sandbox_worker_spins_forever".into(),
            "--ignored".into(),
            "--exact".into(),
            "--nocapture".into(),
        ],
    )
    .unwrap();
    let limits = ResourceLimits {
        wall_time_ms: 100,
        cpu_time_ms: 5_000,
        ..ResourceLimits::default()
    };
    let error = NativeSandbox::detect()
        .prepare_launch(&command, limits)
        .unwrap()
        .run()
        .unwrap_err();
    assert!(matches!(
        error,
        SandboxError::LimitExceeded(LimitViolation::WallTime { .. })
    ));
}

#[cfg(windows)]
#[test]
#[ignore = "sandbox worker entrypoint; invoked only inside AppContainer"]
fn sandbox_worker_reports_appcontainer() {
    assert!(current_process_is_appcontainer().unwrap());
    assert_eq!(std::env::var("IRIS_NATIVE_SANDBOX").unwrap(), "1");
    assert!(std::env::var_os("USERPROFILE").is_none());
    assert!(std::env::var_os("AWS_SECRET_ACCESS_KEY").is_none());
    assert!(
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).is_err()
    );
    let panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let network = std::panic::catch_unwind(|| std::net::TcpListener::bind("127.0.0.1:0"));
    std::panic::set_hook(panic_hook);
    assert!(network.is_err() || network.unwrap().is_err());
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
#[ignore = "sandbox worker entrypoint; invoked only inside AppContainer"]
fn sandbox_worker_emits_output() {
    println!("{}", "x".repeat(16 * 1024));
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
#[ignore = "sandbox worker entrypoint; invoked only inside AppContainer"]
fn sandbox_worker_spins_forever() {
    loop {
        std::hint::spin_loop();
    }
}

/// The worker below probes every boundary from inside the sandbox; each
/// probe prints `name=ok` or `name=blocked`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unix_sandbox_confines_files_network_and_processes() {
    if !enforced_or_skip() {
        return;
    }
    let command = CommandSpec::new(
        std::env::current_exe().unwrap(),
        vec![
            "sandbox_worker_probes_isolation".into(),
            "--ignored".into(),
            "--exact".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ],
    )
    .unwrap();
    let output = NativeSandbox::detect()
        .prepare_launch(&command, ResourceLimits::default())
        .unwrap()
        .run()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.exit_code, 0, "stdout={stdout} stderr={stderr}");
    for expected in [
        "read_outside=blocked",
        "write_outside=blocked",
        "tcp_listen=blocked",
        "spawn_process=blocked",
        "write_scratch=ok",
        "thread=ok",
        "home_env=absent",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected}: {stdout} {stderr}"
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
#[ignore = "sandbox worker entrypoint; invoked only inside the sandbox"]
fn sandbox_worker_probes_isolation() {
    let verdict = |ok: bool| if ok { "ok" } else { "blocked" };
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    println!("read_outside={}", verdict(std::fs::read(&manifest).is_ok()));
    let outside = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sandbox-escape");
    println!(
        "write_outside={}",
        verdict(std::fs::write(&outside, b"x").is_ok())
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0");
    println!("tcp_listen={}", verdict(listener.is_ok()));
    let child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("true")
        .status();
    println!("spawn_process={}", verdict(child.is_ok()));
    println!(
        "write_scratch={}",
        verdict(std::fs::write("scratch.txt", b"x").is_ok())
    );
    let worker = std::thread::spawn(|| 21 * 2).join();
    println!(
        "thread={}",
        verdict(worker.map(|v| v == 42).unwrap_or(false))
    );
    println!(
        "home_env={}",
        if std::env::var_os("HOME").is_none() {
            "absent"
        } else {
            "present"
        }
    );
}
