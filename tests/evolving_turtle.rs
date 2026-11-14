//! The flagship run: a robot in a ROS 2 simulator whose controller is
//! replaced while it drives (projects/evolving_turtle). turtlesim is the
//! plant; `iris evolve-live` refuses an allocating candidate, promotes an
//! unclamped one and rolls it back when it exceeds the speed limit, promotes
//! the saturated one, and verifies the audit chain.
//!
//! Needs turtlesim, the IRIS ROS 2 bridge (iris_ros2.dll / libiris_ros2.so in
//! the repository root) and LLVM ORC; without them the test says so and
//! passes. ROS2_DIR names the ROS 2 install (Windows default
//! C:\dev\ros2_humble\ros2-windows, Linux default /opt/ros/humble).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ros_dir() -> PathBuf {
    std::env::var_os("ROS2_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(if cfg!(windows) {
                r"C:\dev\ros2_humble\ros2-windows"
            } else {
                "/opt/ros/humble"
            })
        })
}

fn turtlesim(ros: &Path) -> PathBuf {
    ros.join("lib").join("turtlesim").join(if cfg!(windows) {
        "turtlesim_node.exe"
    } else {
        "turtlesim_node"
    })
}

fn bridge() -> Option<PathBuf> {
    ["iris_ros2.dll", "libiris_ros2.so"]
        .iter()
        .map(|n| repo().join(n))
        .find(|p| p.exists())
}

/// Runs a ROS 2 executable with the environment both processes share.
fn ros_command(program: &Path, ros: &Path) -> Command {
    let mut command = Command::new(program);
    let project = repo().join("projects").join("evolving_turtle");
    if cfg!(windows) {
        command
            .env("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp")
            .env("AMENT_PREFIX_PATH", ros)
            .env(
                "QT_QPA_PLATFORM_PLUGIN_PATH",
                ros.join("bin").join("platforms"),
            );
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![ros.join("bin")];
        paths.extend(std::env::split_paths(&path));
        command.env("PATH", std::env::join_paths(paths).unwrap());
    } else {
        command.env("QT_QPA_PLATFORM", "offscreen");
        if let Some(lib) = bridge() {
            command.env("IRIS_ROS2_LIB", lib);
        }
    }
    if cfg!(windows) || std::env::var("RMW_IMPLEMENTATION").as_deref() == Ok("rmw_cyclonedds_cpp") {
        let config = project.join("cyclonedds_local.xml");
        command.env(
            "CYCLONEDDS_URI",
            format!("file://{}", config.display().to_string().replace('\\', "/")),
        );
    }
    command
}

struct Killed(Child);
impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_ros2_robot_keeps_driving_while_its_controller_evolves() {
    let ros = ros_dir();
    let sim = turtlesim(&ros);
    if !sim.exists() {
        eprintln!("skipped -- no turtlesim at {}", sim.display());
        return;
    }
    if bridge().is_none() {
        eprintln!("skipped -- no ROS 2 bridge built (run `python build_ros2_bridge.py`)");
        return;
    }
    if !iris::codegen::llvm_orc::is_orc_jit_available() {
        eprintln!("skipped -- LLVM ORC is unavailable");
        return;
    }
    let _sim = Killed(
        ros_command(&sim, &ros)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start turtlesim"),
    );
    std::thread::sleep(std::time::Duration::from_secs(2));

    let project = repo().join("projects").join("evolving_turtle");
    let audit = std::env::temp_dir().join(format!("iris_evolving_turtle_{}", std::process::id()));
    std::fs::create_dir_all(&audit).unwrap();
    let constitution = std::fs::read(project.join("constitution.txt")).unwrap();
    let hash = sha256_hex(&constitution);
    let output = ros_command(Path::new(env!("CARGO_BIN_EXE_iris")), &ros)
        .arg("evolve-live")
        .arg("--plant")
        .arg(project.join("ros2_plant.iris"))
        .arg("--baseline")
        .arg(project.join("baseline.iris"))
        .arg("--candidate")
        .arg(project.join("candidates").join("with_history.iris"))
        .arg("--candidate")
        .arg(project.join("candidates").join("lqr_unclamped.iris"))
        .arg("--candidate")
        .arg(project.join("candidates").join("lqr_saturated.iris"))
        .arg("--cases")
        .arg(project.join("cases.json"))
        .arg("--constitution")
        .arg(project.join("constitution.txt"))
        .arg("--constitution-sha256")
        .arg(&hash)
        .arg("--audit")
        .arg(audit.join("audit.jsonl"))
        .arg("--audit-head")
        .arg(audit.join("audit.head.json"))
        .args([
            "--min-output=-2",
            "--max-output",
            "2",
            "--setpoints",
            "2,8,3,6",
            "--hold",
            "6",
        ])
        .args([
            "--dt",
            "0.032",
            "--offer-every",
            "4",
            "--require-allocation-free",
        ])
        .output()
        .expect("run iris evolve-live");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "evolve-live failed:\n{text}");
    for line in [
        "rejected        with_history",
        "promoted        lqr_unclamped",
        "rolled back     generation 2",
        "promoted        lqr_saturated",
        "audit chain verified",
        "2 promoted, 1 rejected, 1 rolled back; active generation 3",
    ] {
        assert!(
            text.contains(line),
            "the run did not show `{line}`:\n{text}"
        );
    }
    // The saturated law settles the last two moves on the real simulator.
    let settled = text
        .lines()
        .filter(|l| l.contains("after") && l.trim_end().ends_with('3'))
        .count();
    assert!(
        settled >= 2,
        "the last setpoints did not settle under generation 3:\n{text}"
    );
}

/// SHA-256 as lower-case hex (FIPS 180-4), for the constitution pin.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    let bits = (data.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());
    for chunk in message.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[test]
fn sha256_matches_the_fips_180_example() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
