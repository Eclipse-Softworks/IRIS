//! A package registry, end to end: `iris pkg publish` adds versions to a
//! registry directory (never replacing one), a project installs and builds
//! against it -- from the directory and over HTTP (`iris pkg serve`) --
//! resolving semver requirements, and `iris pkg search` finds the package.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn iris(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_iris"))
        .arg("pkg")
        .args(args)
        .current_dir(dir)
        .env_remove("IRIS_REGISTRY")
        .output()
        .expect("run iris")
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn library(dir: &Path, version: &str) {
    write(
        &dir.join("iris.toml"),
        &format!("[package]\nname = \"mathx\"\nversion = \"{version}\"\nentry = \"lib.iris\"\ndescription = \"Small math helpers\"\n"),
    );
    write(
        &dir.join("lib.iris"),
        "pub def triple(x: i64) -> i64 { x * 3 }\n",
    );
}

fn app(dir: &Path, registry: &str, req: &str) {
    write(
        &dir.join("iris.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"main.iris\"\nregistry = \"{}\"\n\n[dependencies]\nmathx = {{ registry = \"mathx\", version = \"{req}\" }}\n",
            registry.replace('\\', "/")
        ),
    );
    write(
        &dir.join("main.iris"),
        "bring \"mathx\"\ndef main() -> i64 {\n    assert(triple(4) == 12)\n    println(\"app ok\")\n    return 0\n}\n",
    );
}

#[test]
fn publish_install_and_serve_a_registry() {
    let root: PathBuf = std::env::temp_dir().join(format!("iris_registry_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let registry = root.join("registry");
    let lib = root.join("mathx");
    std::fs::create_dir_all(&registry).unwrap();
    let reg = registry.to_string_lossy().into_owned();

    // Publish 0.1.0; publishing it again is refused; 0.2.0 joins it.
    library(&lib, "0.1.0");
    let out = iris(&lib, &["publish", "--registry", &reg]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = iris(&lib, &["publish", "--registry", &reg]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("never replaced"));
    library(&lib, "0.2.0");
    assert!(iris(&lib, &["publish", "--registry", &reg])
        .status
        .success());

    let index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(registry.join("index.json")).unwrap())
            .unwrap();
    let versions = &index["packages"]["mathx"]["versions"];
    assert_eq!(versions["0.1.0"]["url"], "pkgs/mathx-0.1.0.tar.gz");
    assert!(versions["0.2.0"]["checksum"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(
        index["packages"]["mathx"]["description"],
        "Small math helpers"
    );

    // Install from the directory: ^0.1 is 0.1.0, built and run.
    let from_dir = root.join("app_dir");
    app(&from_dir, &reg, "^0.1");
    let out = iris(&from_dir, &["run"]);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("app ok"));
    assert!(from_dir.join(".iris/deps/mathx/lib.iris").exists());
    assert!(std::fs::read_to_string(from_dir.join("iris.lock"))
        .unwrap()
        .contains("mathx@0.1.0"));

    // Over HTTP: ^0.2 is 0.2.0.
    let mut server = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["pkg", "serve", &reg, "--port", "0"])
        .stderr(Stdio::piped())
        .spawn()
        .expect("serve");
    let mut line = String::new();
    BufReader::new(server.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line
        .trim()
        .rsplit(':')
        .next()
        .unwrap()
        .trim_end_matches('/')
        .to_string();
    let url = format!("http://127.0.0.1:{port}");
    let over_http = root.join("app_http");
    app(&over_http, &url, "^0.2");
    let out = iris(&over_http, &["install"]);
    let _ = server.kill();
    let _ = server.wait();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(std::fs::read_to_string(over_http.join("iris.lock"))
        .unwrap()
        .contains("mathx@0.2.0"));

    // Search.
    let out = iris(&root, &["search", "math", "--registry", &reg]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("mathx 0.2.0  Small math helpers"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_http_registry_is_read_only() {
    let root = std::env::temp_dir().join(format!("iris_registry_ro_{}", std::process::id()));
    library(&root, "1.0.0");
    let out = iris(
        &root,
        &["publish", "--registry", "https://example.org/registry"],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("read-only"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn vendor_offline_locked_verify_and_tree_for_transitive_deps() {
    let root: PathBuf =
        std::env::temp_dir().join(format!("iris_pkg_offline_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let registry = root.join("registry");
    std::fs::create_dir_all(&registry).unwrap();
    let reg = registry.to_string_lossy().into_owned();

    // Publish leaf 0.1.0 and mid 0.1.0 -> depends on leaf ^0.1.
    let leaf_dir = root.join("leaf");
    write(
        &leaf_dir.join("iris.toml"),
        "[package]\nname = \"leaf\"\nversion = \"0.1.0\"\nentry = \"lib.iris\"\n",
    );
    write(
        &leaf_dir.join("lib.iris"),
        "pub def leaf_val() -> i64 { 19 }\n",
    );
    assert!(iris(&leaf_dir, &["publish", "--registry", &reg])
        .status
        .success());

    let mid_dir = root.join("mid");
    write(
        &mid_dir.join("iris.toml"),
        "[package]\nname = \"mid\"\nversion = \"0.1.0\"\nentry = \"lib.iris\"\n\n[dependencies]\nleaf = { registry = \"leaf\", version = \"^0.1\" }\n",
    );
    write(
        &mid_dir.join("lib.iris"),
        "bring \"leaf\"\npub def mid_val() -> i64 { leaf_val() + 23 }\n",
    );
    assert!(iris(&mid_dir, &["publish", "--registry", &reg])
        .status
        .success());

    // Serve over HTTP, install and vendor, then stop HTTP server and delete registry dir.
    let mut server = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["pkg", "serve", &reg, "--port", "0"])
        .stderr(Stdio::piped())
        .spawn()
        .expect("serve");
    let mut line = String::new();
    BufReader::new(server.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line
        .trim()
        .rsplit(':')
        .next()
        .unwrap()
        .trim_end_matches('/')
        .to_string();
    let url = format!("http://127.0.0.1:{port}");

    let app_dir = root.join("consumer");
    write(
        &app_dir.join("iris.toml"),
        &format!(
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nentry = \"main.iris\"\nregistry = \"{url}\"\n\n[dependencies]\nmid = {{ registry = \"mid\", version = \"^0.1\" }}\n"
        ),
    );
    write(
        &app_dir.join("main.iris"),
        "bring \"mid\"\ndef main() -> i64 {\n    assert(mid_val() == 42)\n    println(\"transitive offline ok\")\n    return 0\n}\n",
    );

    let out = iris(&app_dir, &["install"]);
    assert!(
        out.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = iris(&app_dir, &["vendor"]);
    assert!(
        out.status.success(),
        "vendor failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(app_dir.join(".iris/vendor/mid/lib.iris").exists());
    assert!(
        app_dir.join(".iris/vendor/leaf/lib.iris").exists(),
        "transitive dep must be vendored"
    );

    // Tear down registry and server completely so offline build must rely on .iris/vendor/.
    let _ = server.kill();
    let _ = server.wait();
    std::fs::remove_dir_all(&registry).unwrap();

    // Unvendored missing deps check after `clean` vs `install --offline --locked`.
    assert!(iris(&app_dir, &["clean"]).status.success());
    assert!(!app_dir.join(".iris/deps").exists());
    assert!(!iris(&app_dir, &["check"]).status.success());

    let out = iris(&app_dir, &["install", "--offline", "--locked"]);
    assert!(
        out.status.success(),
        "offline locked install failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(iris(&app_dir, &["check"]).status.success());
    assert!(iris(&app_dir, &["verify"]).status.success());

    let tree = iris(&app_dir, &["tree"]);
    assert!(tree.status.success());
    let tree_stdout = String::from_utf8_lossy(&tree.stdout);
    assert!(tree_stdout.contains("mid v0.1.0"), "{tree_stdout}");
    assert!(tree_stdout.contains("leaf v0.1.0"), "{tree_stdout}");

    let run_out = iris(&app_dir, &["run", "--offline", "--locked"]);
    assert!(
        run_out.status.success(),
        "offline run failed: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
    assert!(String::from_utf8_lossy(&run_out.stdout).contains("transitive offline ok"));

    // Verify detects on-disk tampering in `.iris/deps/`.
    write(
        &app_dir.join(".iris/deps/leaf/lib.iris"),
        "pub def leaf_val() -> i64 { 999 }\n",
    );
    let verify_bad = iris(&app_dir, &["verify"]);
    assert!(!verify_bad.status.success());
    assert!(
        String::from_utf8_lossy(&verify_bad.stderr).contains("checksum mismatch"),
        "{}",
        String::from_utf8_lossy(&verify_bad.stderr)
    );

    // `--locked` refuses missing lockfile or tampered vendor checksum.
    write(
        &app_dir.join(".iris/vendor/leaf/lib.iris"),
        "pub def leaf_val() -> i64 { 999 }\n",
    );
    let locked_tampered = iris(&app_dir, &["install", "--offline", "--locked"]);
    assert!(!locked_tampered.status.success());
    assert!(
        String::from_utf8_lossy(&locked_tampered.stderr).contains("checksum mismatch"),
        "{}",
        String::from_utf8_lossy(&locked_tampered.stderr)
    );

    std::fs::remove_file(app_dir.join("iris.lock")).unwrap();
    let missing_lock = iris(&app_dir, &["install", "--offline", "--locked"]);
    assert!(!missing_lock.status.success());
    assert!(
        String::from_utf8_lossy(&missing_lock.stderr).contains("iris.lock not found"),
        "{}",
        String::from_utf8_lossy(&missing_lock.stderr)
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn transitive_semver_conflict_is_rejected() {
    let root = std::env::temp_dir().join(format!("iris_pkg_conflict_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let registry = root.join("registry");
    std::fs::create_dir_all(&registry).unwrap();
    let reg = registry.to_string_lossy().into_owned();

    let shared = root.join("shared");
    write(
        &shared.join("iris.toml"),
        "[package]\nname = \"shared\"\nversion = \"0.1.0\"\nentry = \"lib.iris\"\n",
    );
    write(&shared.join("lib.iris"), "pub def v() -> i64 { 1 }\n");
    assert!(iris(&shared, &["publish", "--registry", &reg])
        .status
        .success());

    write(
        &shared.join("iris.toml"),
        "[package]\nname = \"shared\"\nversion = \"2.0.0\"\nentry = \"lib.iris\"\n",
    );
    write(&shared.join("lib.iris"), "pub def v() -> i64 { 2 }\n");
    assert!(iris(&shared, &["publish", "--registry", &reg])
        .status
        .success());

    let pkg_a = root.join("pkg_a");
    write(
        &pkg_a.join("iris.toml"),
        "[package]\nname = \"pkg_a\"\nversion = \"0.1.0\"\nentry = \"lib.iris\"\n\n[dependencies]\nshared = { registry = \"shared\", version = \"^0.1\" }\n",
    );
    write(
        &pkg_a.join("lib.iris"),
        "bring \"shared\"\npub def a() -> i64 { v() }\n",
    );
    assert!(iris(&pkg_a, &["publish", "--registry", &reg])
        .status
        .success());

    let pkg_b = root.join("pkg_b");
    write(
        &pkg_b.join("iris.toml"),
        "[package]\nname = \"pkg_b\"\nversion = \"0.1.0\"\nentry = \"lib.iris\"\n\n[dependencies]\nshared = { registry = \"shared\", version = \"^2.0\" }\n",
    );
    write(
        &pkg_b.join("lib.iris"),
        "bring \"shared\"\npub def b() -> i64 { v() }\n",
    );
    assert!(iris(&pkg_b, &["publish", "--registry", &reg])
        .status
        .success());

    let app_dir = root.join("app");
    write(
        &app_dir.join("iris.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"main.iris\"\nregistry = \"{}\"\n\n[dependencies]\npkg_a = {{ registry = \"pkg_a\", version = \"*\" }}\npkg_b = {{ registry = \"pkg_b\", version = \"*\" }}\n",
            reg.replace('\\', "/")
        ),
    );
    write(&app_dir.join("main.iris"), "def main() -> i64 { 0 }\n");

    let out = iris(&app_dir, &["install"]);
    assert!(!out.status.success(), "expected semver conflict to fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("semver conflict"), "stderr: {stderr}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn starter_registry_packages_build_and_execute() {
    let reg_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("registry");
    let reg = reg_dir.to_string_lossy().into_owned();

    let search = iris(&reg_dir, &["search", "", "--registry", &reg]);
    assert!(search.status.success());
    let listing = String::from_utf8_lossy(&search.stdout);
    assert!(listing.contains("math_extra"), "{listing}");
    assert!(listing.contains("stats"), "{listing}");
    assert!(listing.contains("str_ext"), "{listing}");

    let root = std::env::temp_dir().join(format!("iris_starter_pkg_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    write(
        &root.join("iris.toml"),
        &format!(
            "[package]\nname = \"starter_smoke\"\nversion = \"0.1.0\"\nentry = \"main.iris\"\nregistry = \"{}\"\n\n[dependencies]\nstats = {{ registry = \"stats\", version = \"^0.1\" }}\nstr_ext = {{ registry = \"str_ext\", version = \"^0.1\" }}\n",
            reg.replace('\\', "/")
        ),
    );
    write(
        &root.join("main.iris"),
        "bring \"math_extra\"\nbring \"stats\"\nbring \"str_ext\"\ndef main() -> i64 {\n    assert(gcd_i64(48, 18) == 6)\n    val nums: list<i64> = list()\n    push(nums, 2); push(nums, 8); push(nums, 5)\n    assert(sum_i64(nums) == 15)\n    assert(min_i64(nums) == 2)\n    assert(max_i64(nums) == 8)\n    val xs: list<f64> = list()\n    push(xs, 2.0); push(xs, 4.0)\n    assert(approx_eq(mean_f64(xs), 3.0, 0.0001))\n    assert(pad_left(\"7\", 3, \"0\") == \"007\")\n    println(\"starter ok\")\n    return 0\n}\n",
    );

    let out = iris(&root, &["run"]);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("starter ok"));
    let _ = std::fs::remove_dir_all(&root);
}
