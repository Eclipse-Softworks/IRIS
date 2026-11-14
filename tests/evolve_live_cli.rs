//! `iris evolve-live` on projects/evolving_cart_controller: a cart keeps
//! running while its controller is replaced under the seven gates.

use std::path::{Path, PathBuf};
use std::process::Command;

use iris::codegen::llvm_orc::is_orc_jit_available;
use iris::evolution::audit::{read_verified_records, EvolutionEventKind};
use iris::evolution::sha256_hex;

fn project() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("projects/evolving_cart_controller")
}

fn run(name: &str, extra: &[&str]) -> (String, PathBuf) {
    let dir = std::env::temp_dir().join(format!("iris_evolve_live_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = project();
    let constitution = p.join("constitution.txt");
    let hash = sha256_hex(&std::fs::read(&constitution).unwrap());
    let output = Command::new(env!("CARGO_BIN_EXE_iris"))
        .arg("evolve-live")
        .arg("--plant")
        .arg(p.join("plant.iris"))
        .arg("--baseline")
        .arg(p.join("baseline.iris"))
        .arg("--cases")
        .arg(p.join("cases.json"))
        .arg("--constitution")
        .arg(&constitution)
        .arg("--constitution-sha256")
        .arg(&hash)
        .arg("--audit")
        .arg(dir.join("audit.jsonl"))
        .arg("--audit-head")
        .arg(dir.join("audit.head.json"))
        .args([
            "--min-output=-10",
            "--max-output",
            "10",
            "--setpoints",
            "1,5,-2,3",
        ])
        .args([
            "--hold",
            "8",
            "--offer-every",
            "4",
            "--require-allocation-free",
        ])
        .args(extra)
        .output()
        .expect("run iris evolve-live");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{text}");
    (text, dir)
}

#[test]
fn controller_is_replaced_rejected_and_rolled_back_while_the_cart_runs() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; evolve-live test skipped");
        return;
    }
    let p = project();
    let candidates: Vec<String> = ["pd_with_history", "lqr_unclamped", "lqr_saturated"]
        .iter()
        .flat_map(|name| {
            [
                "--candidate".to_owned(),
                p.join(format!("candidates/{name}.iris"))
                    .display()
                    .to_string(),
            ]
        })
        .collect();
    let args: Vec<&str> = candidates.iter().map(String::as_str).collect();
    let (out, dir) = run("schedule", &args);

    let line = |needle: &str| {
        out.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle:?} in:\n{out}"))
            .to_owned()
    };
    // The allocating law is refused by the IR-level proof.
    assert!(
        line("rejected        pd_with_history").contains("allocation-free proof failed"),
        "{out}"
    );
    // The unsaturated optimal law passes every gate, then breaks the rating
    // at the 5 m step and is rolled back in the same step.
    assert!(
        line("promoted        lqr_unclamped").contains("generation 2 after 7/7 gates"),
        "{out}"
    );
    assert!(
        line("rolled back     generation 2").contains("generation 1 restored"),
        "{out}"
    );
    // The saturated law holds to the end.
    assert!(
        line("promoted        lqr_saturated").contains("generation 3"),
        "{out}"
    );
    assert!(
        out.contains("2 promoted, 1 rejected, 1 rolled back; active generation 3"),
        "{out}"
    );
    // Once generation 3 drives, every later step settles.
    for target in ["-2.000", "3.000"] {
        let row = out
            .lines()
            .find(|l| l.trim_start().starts_with(target))
            .unwrap_or_else(|| panic!("no segment row for {target} in:\n{out}"));
        assert!(row.contains("after"), "{row}");
        assert!(row.trim_end().ends_with(" 3"), "{row}");
    }
    assert!(out.contains("audit chain verified"), "{out}");

    let records = read_verified_records(&dir.join("audit.jsonl")).unwrap();
    let kinds: Vec<EvolutionEventKind> = records.iter().map(|r| r.event.kind).collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == EvolutionEventKind::Promoted)
            .count(),
        2
    );
    assert!(kinds.contains(&EvolutionEventKind::RolledBack), "{kinds:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_search_beside_the_loop_rediscovers_the_optimal_law() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; evolve-live search test skipped");
        return;
    }
    let (out, dir) = run("search", &["--search", "--seed", "7"]);
    let found = out
        .lines()
        .find(|l| l.contains("search finished"))
        .unwrap_or_else(|| panic!("the search never finished:\n{out}"));
    // reference.iris's LQR gains, recovered from the 25 canary cases.
    assert!(
        found.contains("4.3401766") && found.contains("3.465343"),
        "{found}"
    );
    assert!(
        out.contains("promoted        searched: generation 2 after 7/7 gates"),
        "{out}"
    );
    // The search fitted the law near the operating point, unsaturated: the
    // 5 m step at t = 8 s asks for more than the rating, and it is rolled back.
    let rollback = out
        .lines()
        .find(|l| l.contains("rolled back     generation 2"))
        .unwrap_or_else(|| panic!("no rollback in:\n{out}"));
    assert!(rollback.contains("[t=   8.00s]"), "{rollback}");
    let _ = std::fs::remove_dir_all(dir);
}
