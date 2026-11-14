//! The developer commands: `iris new` (every template builds and passes its
//! own tests), `iris check`, `iris bug-report`, `iris doctor`, and help that
//! is generated from the command definitions.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn iris() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_iris"));
    c.env_remove("IRIS_FORCE_INTERP").env_remove("IRIS_DEBUG");
    c
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iris_dev_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn new_project(dir: &Path, template: &str, name: &str) -> PathBuf {
    let o = iris()
        .current_dir(dir)
        .args(["new", template, name])
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "iris new {template} failed:\n{}",
        text(&o)
    );
    dir.join(name)
}

fn checks_and_tests(project: &Path, test_target: Option<&str>) {
    let o = iris().current_dir(project).arg("check").output().unwrap();
    assert!(
        o.status.success(),
        "iris check failed in {}:\n{}",
        project.display(),
        text(&o)
    );
    assert!(text(&o).contains("no errors"), "{}", text(&o));
    if let Some(file) = test_target {
        let o = iris()
            .current_dir(project)
            .args(["test", file])
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "iris test failed in {}:\n{}",
            project.display(),
            text(&o)
        );
    }
}

fn runs_on_both_backends(project: &Path, expected: &str) {
    for interp in [false, true] {
        let mut c = iris();
        c.current_dir(project).args(["--emit", "eval", "main.iris"]);
        if interp {
            c.env("IRIS_FORCE_INTERP", "1");
        }
        let o = c.output().unwrap();
        assert!(
            o.status.success(),
            "main.iris ({}) failed:\n{}",
            if interp { "interpreter" } else { "native" },
            text(&o)
        );
        assert!(
            text(&o).contains(expected),
            "main.iris ({}) printed:\n{}",
            if interp { "interpreter" } else { "native" },
            text(&o)
        );
    }
}

#[test]
fn every_template_checks_and_passes_its_tests() {
    let dir = scratch("new");
    let app = new_project(&dir, "app", "hello");
    checks_and_tests(&app, Some("main.iris"));
    runs_on_both_backends(&app, "count 3, total 16, largest 9");

    let cli = new_project(&dir, "cli", "greeter");
    checks_and_tests(&cli, Some("main.iris"));
    runs_on_both_backends(&cli, "hello, world");

    let server = new_project(&dir, "server", "echo_service");
    checks_and_tests(&server, Some("main.iris"));

    let ros2 = new_project(&dir, "ros2", "rover");
    checks_and_tests(&ros2, Some("main.iris"));

    let controller = new_project(&dir, "controller", "cart");
    checks_and_tests(&controller, None);
    let o = iris()
        .current_dir(&controller)
        .args(["--emit", "eval", "reference.iris"])
        .output()
        .unwrap();
    assert!(
        o.status.success() && text(&o).contains("reference controller derived"),
        "{}",
        text(&o)
    );

    // `iris new NAME` is the app template; a bad name or an existing
    // directory is refused.
    let o = iris()
        .current_dir(&dir)
        .args(["new", "plain"])
        .output()
        .unwrap();
    assert!(
        o.status.success() && dir.join("plain").join("main.iris").exists(),
        "{}",
        text(&o)
    );
    let o = iris()
        .current_dir(&dir)
        .args(["new", "app", "plain"])
        .output()
        .unwrap();
    assert!(
        !o.status.success() && text(&o).contains("already exists"),
        "{}",
        text(&o)
    );
    let o = iris()
        .current_dir(&dir)
        .args(["new", "app", "9lives"])
        .output()
        .unwrap();
    assert!(
        !o.status.success() && text(&o).contains("not a valid project name"),
        "{}",
        text(&o)
    );
    let o = iris()
        .current_dir(&dir)
        .args(["new", "website", "site"])
        .output()
        .unwrap();
    assert!(
        !o.status.success() && text(&o).contains("no template `website`"),
        "{}",
        text(&o)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_reports_every_error_and_sets_the_exit_code() {
    let dir = scratch("check");
    std::fs::write(dir.join("good.iris"), "def main() -> i64 { 0 }\n").unwrap();
    std::fs::write(
        dir.join("bad.iris"),
        "def a() -> i64 { pritnln(\"x\"); 0 }\ndef b() -> i64 { totl }\ndef main() -> i64 { 0 }\n",
    )
    .unwrap();
    let o = iris()
        .args(["check"])
        .arg(dir.join("good.iris"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let o = iris().args(["check"]).arg(&dir).output().unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let out = text(&o);
    // Both functions' errors, not only the first.
    assert!(
        out.contains("cannot find `pritnln`") && out.contains("cannot find `totl`"),
        "{out}"
    );
    assert!(out.contains("checked 2 files"), "{out}");
    let o = iris()
        .args(["check", "--error-format", "json"])
        .arg(dir.join("bad.iris"))
        .output()
        .unwrap();
    let json_lines = String::from_utf8_lossy(&o.stderr)
        .lines()
        .filter(|l| l.starts_with('{'))
        .count();
    assert_eq!(json_lines, 2, "{}", text(&o));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bug_report_collects_the_program_and_the_compiler_output() {
    let dir = scratch("bug");
    std::fs::write(dir.join("helper.iris"), "pub def two() -> i64 { 2 }\n").unwrap();
    std::fs::write(
        dir.join("main.iris"),
        "bring \"helper.iris\"\ndef main() -> i64 { two() + undefined_thing }\n",
    )
    .unwrap();
    let out_dir = dir.join("report");
    let o = iris()
        .current_dir(&dir)
        .args(["bug-report", "main.iris", "-o"])
        .arg(&out_dir)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    for f in [
        "REPORT.md",
        "compile.txt",
        "environment.txt",
        "source/main.iris",
        "source/helper.iris",
    ] {
        assert!(out_dir.join(f).exists(), "missing {f}");
    }
    let report = std::fs::read_to_string(out_dir.join("REPORT.md")).unwrap();
    assert!(report.contains("cannot find `undefined_thing`"), "{report}");
    assert!(report.contains("contains your source code"), "{report}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn doctor_runs_end_to_end() {
    let o = iris().arg("doctor").output().unwrap();
    let out = text(&o);
    assert!(
        out.contains("IRIS ") && out.contains("native build") && out.contains("interpreter"),
        "{out}"
    );
}

#[test]
fn help_lists_every_command_and_each_command_has_its_own() {
    let o = iris().arg("--help").output().unwrap();
    let out = text(&o);
    for cmd in [
        "run",
        "test",
        "check",
        "watch",
        "doctor",
        "new",
        "bug-report",
        "evolve-live",
        "evolve-serve",
        "explain",
    ] {
        assert!(
            out.contains(cmd),
            "`iris --help` does not list `{cmd}`:\n{out}"
        );
    }
    let o = iris().args(["watch", "--help"]).output().unwrap();
    assert!(
        o.status.success() && text(&o).contains("--test"),
        "{}",
        text(&o)
    );
    let o = iris()
        .args(["frobnicate.iris", "--no-such-flag"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
}
