//! End-to-end verification of `iris build --gui`, `iris bundle`, icon/ZIP encoders,
//! Windows PE GUI subsystem headers, and standalone UI host loading outside the repository.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use iris::codegen::app_bundle::{
    encode_png_as_icns, encode_png_as_ico, encode_rgba_png, generate_default_iris_icon_rgba,
    write_zip_directory,
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn iris() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_iris"));
    c.env_remove("IRIS_FORCE_INTERP").env_remove("IRIS_DEBUG");
    c
}

fn scratch(name: &str) -> PathBuf {
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "iris_bundle_test_{name}_{}_{seq}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn output_text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).unwrap();
        }
    }
}

fn expected_ui_host_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "iris_ui_host.dll"
    } else if cfg!(target_os = "macos") {
        "libiris_ui_host.dylib"
    } else {
        "libiris_ui_host.so"
    }
}

#[test]
fn icon_and_zip_encoders_produce_valid_headers() {
    let rgba = generate_default_iris_icon_rgba(64);
    assert_eq!(rgba.len(), 64 * 64 * 4);

    let png = encode_rgba_png(64, 64, &rgba).expect("PNG encoding failed");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));

    let ico = encode_png_as_ico(&png, 64, 64);
    assert!(ico.len() > 22);
    assert_eq!(&ico[..6], &[0, 0, 1, 0, 1, 0]);

    let icns = encode_png_as_icns(&png);
    assert_eq!(&icns[..4], b"icns");
    let total_len = u32::from_be_bytes([icns[4], icns[5], icns[6], icns[7]]) as usize;
    assert_eq!(total_len, icns.len());

    let tmp = scratch("zip");
    let src_dir = tmp.join("payload");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("hello.txt"), b"iris-bundle").unwrap();
    let zip_path = tmp.join("out.zip");
    write_zip_directory(&src_dir, &zip_path).expect("ZIP encoding failed");

    let zip_bytes = fs::read(&zip_path).unwrap();
    assert!(zip_bytes.starts_with(b"PK\x03\x04"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn build_gui_sets_windows_subsystem_and_runs() {
    let tmp = scratch("build_gui");
    let src = tmp.join("hello_gui.iris");
    fs::write(&src, "def main() -> i64 {\n    return 0\n}\n").unwrap();

    let exe_out = tmp.join(format!("hello_gui{}", std::env::consts::EXE_SUFFIX));
    let out = iris()
        .current_dir(&tmp)
        .args(["build", "--gui"])
        .arg(&src)
        .arg("-o")
        .arg(&exe_out)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "iris build --gui failed:\n{}",
        output_text(&out)
    );
    assert!(exe_out.is_file(), "missing output binary {:?}", exe_out);

    #[cfg(target_os = "windows")]
    {
        let pe = fs::read(&exe_out).unwrap();
        assert!(pe.len() > 0x40);
        let lfanew = u32::from_le_bytes([pe[0x3C], pe[0x3D], pe[0x3E], pe[0x3F]]) as usize;
        assert_eq!(&pe[lfanew..lfanew + 4], b"PE\0\0");
        let subsys_off = lfanew + 24 + 68;
        let subsystem = u16::from_le_bytes([pe[subsys_off], pe[subsys_off + 1]]);
        assert_eq!(
            subsystem, 2,
            "expected IMAGE_SUBSYSTEM_WINDOWS_GUI (2), got {subsystem}"
        );
    }

    let run = Command::new(&exe_out).output().unwrap();
    assert!(run.status.success(), "built --gui binary failed to run");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn bundle_ui_app_runs_standalone_outside_repo_and_packages_formats_and_installs() {
    let tmp = scratch("bundle_e2e");
    let new_out = iris()
        .current_dir(&tmp)
        .args(["new", "ui", "desk_app"])
        .output()
        .unwrap();
    assert!(
        new_out.status.success(),
        "iris new ui failed:\n{}",
        output_text(&new_out)
    );
    let project_dir = tmp.join("desk_app");

    let bundle_dir = tmp.join("bundle_out");
    let b_out = iris()
        .current_dir(&project_dir)
        .args(["bundle", ".", "--console", "-o"])
        .arg(&bundle_dir)
        .output()
        .unwrap();
    assert!(
        b_out.status.success(),
        "iris bundle --console failed:\n{}",
        output_text(&b_out)
    );

    let exe_name = format!("desk_app{}", std::env::consts::EXE_SUFFIX);
    assert!(bundle_dir.join(&exe_name).is_file());
    assert!(bundle_dir.join("app.json").is_file());
    assert!(
        bundle_dir.join(expected_ui_host_name()).is_file(),
        "missing bundled UI host library in {:?}",
        bundle_dir
    );

    // Copy the bundle cleanly outside the project/repo and execute from an empty CWD
    // with `IRIS_UI_HOST_LIB` unset to verify pure standalone resolution.
    let isolated = tmp.join("isolated_app");
    copy_dir_recursive(&bundle_dir, &isolated);
    let empty_cwd = tmp.join("empty_cwd");
    fs::create_dir_all(&empty_cwd).unwrap();
    let snap_path = isolated.join("standalone_snap.png");
    let run_iso = Command::new(isolated.join(&exe_name))
        .current_dir(&empty_cwd)
        .env_remove("IRIS_UI_HOST_LIB")
        .args(["--headless-snapshot"])
        .arg(&snap_path)
        .output()
        .unwrap();
    assert!(
        run_iso.status.success(),
        "standalone bundled executable failed:\n{}",
        output_text(&run_iso)
    );
    let snap_bytes = fs::read(&snap_path).expect("missing standalone snapshot PNG");
    assert!(snap_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

    // ZIP format packaging
    let zip_target = tmp.join("archives").join("desk_app");
    let z_out = iris()
        .current_dir(&project_dir)
        .args(["bundle", ".", "--console", "--format", "zip", "-o"])
        .arg(&zip_target)
        .output()
        .unwrap();
    assert!(
        z_out.status.success(),
        "iris bundle --format zip failed:\n{}",
        output_text(&z_out)
    );
    let zip_file = tmp.join("archives").join("desk_app.zip");
    assert!(zip_file.is_file(), "missing zip artifact {:?}", zip_file);
    let zip_bytes = fs::read(&zip_file).unwrap();
    assert!(zip_bytes.starts_with(b"PK\x03\x04"));

    // Windows Inno Setup script generation
    #[cfg(target_os = "windows")]
    {
        let inno_dir = tmp.join("inno_out");
        let inno_target = inno_dir.join("desk_app");
        let i_out = iris()
            .current_dir(&project_dir)
            .args(["bundle", ".", "--console", "--format", "inno", "-o"])
            .arg(&inno_target)
            .output()
            .unwrap();
        assert!(
            i_out.status.success(),
            "iris bundle --format inno failed:\n{}",
            output_text(&i_out)
        );
        let iss_path = inno_dir.join("desk_app.iss");
        assert!(iss_path.is_file(), "missing .iss script {:?}", iss_path);
        let iss_src = fs::read_to_string(&iss_path).unwrap();
        assert!(iss_src.contains("[Files]"));
        assert!(iss_src.contains("desk_app.bundle_dir"));
        assert!(iss_src.contains(r"\*"));
        assert!(inno_dir
            .join("desk_app.bundle_dir")
            .join("iris_ui_host.dll")
            .is_file());
    }

    // Isolated install & uninstall without touching host OS shortcuts or registry
    let install_dir = tmp.join("installed_app");
    let inst_out = iris()
        .current_dir(&project_dir)
        .args([
            "bundle",
            ".",
            "--console",
            "--install",
            "--no-register",
            "--install-dir",
        ])
        .arg(&install_dir)
        .output()
        .unwrap();
    assert!(
        inst_out.status.success(),
        "iris bundle --install failed:\n{}",
        output_text(&inst_out)
    );
    let installed_exe = install_dir.join(&exe_name);
    assert!(installed_exe.is_file());

    let inst_snap = tmp.join("inst_snap.png");
    let run_inst = Command::new(&installed_exe)
        .current_dir(&empty_cwd)
        .env_remove("IRIS_UI_HOST_LIB")
        .args(["--headless-snapshot"])
        .arg(&inst_snap)
        .output()
        .unwrap();
    assert!(
        run_inst.status.success(),
        "installed app failed to run:\n{}",
        output_text(&run_inst)
    );
    assert!(fs::read(&inst_snap)
        .unwrap()
        .starts_with(b"\x89PNG\r\n\x1a\n"));

    let uninst_out = iris()
        .current_dir(&project_dir)
        .args([
            "bundle",
            ".",
            "--uninstall",
            "--no-register",
            "--install-dir",
        ])
        .arg(&install_dir)
        .output()
        .unwrap();
    assert!(
        uninst_out.status.success(),
        "iris bundle --uninstall failed:\n{}",
        output_text(&uninst_out)
    );
    assert!(
        !install_dir.exists(),
        "install dir {:?} still exists after --uninstall",
        install_dir
    );

    let _ = fs::remove_dir_all(&tmp);
}
