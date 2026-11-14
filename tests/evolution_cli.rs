use std::process::Command;

use iris::codegen::llvm_orc::is_orc_jit_available;
use iris::evolution::audit::{read_verified_records, EvolutionEventKind};
use iris::evolution::sha256_hex;

#[test]
fn evolve_command_promotes_the_public_lab_through_seven_gates() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; evolve CLI capability test skipped");
        return;
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lab = root.join("projects/autonomous_evolution_lab");
    let constitution = lab.join("constitution.txt");
    let constitution_hash = sha256_hex(&std::fs::read(&constitution).unwrap());
    let audit = std::env::temp_dir().join(format!(
        "iris_evolution_cli_{}_audit.jsonl",
        std::process::id()
    ));
    let audit_head = audit.with_extension("head.json");
    let _ = std::fs::remove_file(&audit);
    let _ = std::fs::remove_file(&audit_head);

    let output = Command::new(env!("CARGO_BIN_EXE_iris"))
        .arg("evolve")
        .arg("--baseline")
        .arg(lab.join("baseline.iris"))
        .arg("--candidate")
        .arg(lab.join("candidate.iris"))
        .arg("--cases")
        .arg(lab.join("cases.json"))
        .arg("--constitution")
        .arg(&constitution)
        .arg("--constitution-sha256")
        .arg(constitution_hash)
        .arg("--audit")
        .arg(&audit)
        .arg("--audit-head")
        .arg(&audit_head)
        .arg("--min-output=-1000")
        .arg("--max-output")
        .arg("1000")
        .output()
        .expect("run iris evolve");
    let mut diagnostic = String::from_utf8_lossy(&output.stdout).into_owned();
    diagnostic.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{diagnostic}");
    assert!(diagnostic.contains("after 7/7 gates"), "{diagnostic}");
    assert!(diagnostic.contains("candidate 5/5"), "{diagnostic}");

    let records = read_verified_records(&audit).expect("verified CLI audit chain");
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event.kind == EvolutionEventKind::GatePassed)
            .count(),
        7
    );
    assert_eq!(
        records.last().unwrap().event.kind,
        EvolutionEventKind::Promoted
    );
    let _ = std::fs::remove_file(audit);
    let _ = std::fs::remove_file(audit_head);
}

#[test]
fn evolve_search_command_discovers_and_saves_candidate() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lab = root.join("projects/autonomous_evolution_lab");
    let candidate_out = std::env::temp_dir().join(format!(
        "iris_evolved_candidate_{}.iris",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&candidate_out);

    let output = Command::new(env!("CARGO_BIN_EXE_iris"))
        .arg("evolve-search")
        .arg("--baseline")
        .arg(lab.join("baseline.iris"))
        .arg("--cases")
        .arg(lab.join("cases.json"))
        .arg("--generations")
        .arg("20")
        .arg("--pop-size")
        .arg("25")
        .arg("--seed")
        .arg("42")
        .arg("--out-candidate")
        .arg(&candidate_out)
        .output()
        .expect("run iris evolve-search");

    let mut diagnostic = String::from_utf8_lossy(&output.stdout).into_owned();
    diagnostic.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{diagnostic}");
    assert!(
        diagnostic.contains("== IRIS Evolutionary Search =="),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("=== Evolution Outcome ==="),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("Saved candidate source to"),
        "{diagnostic}"
    );

    assert!(
        candidate_out.exists(),
        "candidate output file was not created"
    );
    let content = std::fs::read_to_string(&candidate_out).unwrap();
    assert!(
        content.contains("def policy(input: i64) -> i64"),
        "missing policy in: {content}"
    );
    assert!(
        content.contains("def test_policy()"),
        "missing test_policy in: {content}"
    );

    let _ = std::fs::remove_file(candidate_out);
}

#[test]
fn evolve_search_command_with_promote_promotes_through_seven_gates() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; evolve-search promote test skipped");
        return;
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lab = root.join("projects/autonomous_evolution_lab");
    let constitution = lab.join("constitution.txt");
    let constitution_hash = sha256_hex(&std::fs::read(&constitution).unwrap());
    let audit = std::env::temp_dir().join(format!(
        "iris_evolve_search_promote_{}_audit.jsonl",
        std::process::id()
    ));
    let audit_head = audit.with_extension("head.json");
    let candidate_out = std::env::temp_dir().join(format!(
        "iris_evolve_search_promote_{}_candidate.iris",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&audit);
    let _ = std::fs::remove_file(&audit_head);
    let _ = std::fs::remove_file(&candidate_out);

    let output = Command::new(env!("CARGO_BIN_EXE_iris"))
        .arg("evolve-search")
        .arg("--baseline")
        .arg(lab.join("baseline.iris"))
        .arg("--cases")
        .arg(lab.join("cases.json"))
        .arg("--generations")
        .arg("35")
        .arg("--pop-size")
        .arg("40")
        .arg("--seed")
        .arg("12345")
        .arg("--out-candidate")
        .arg(&candidate_out)
        .arg("--promote")
        .arg("--constitution")
        .arg(&constitution)
        .arg("--constitution-sha256")
        .arg(constitution_hash)
        .arg("--audit")
        .arg(&audit)
        .arg("--audit-head")
        .arg(&audit_head)
        .arg("--min-output=-1000")
        .arg("--max-output")
        .arg("1000")
        .output()
        .expect("run iris evolve-search --promote");

    let mut diagnostic = String::from_utf8_lossy(&output.stdout).into_owned();
    diagnostic.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{diagnostic}");
    assert!(
        diagnostic.contains("promoted policy generation"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("after 7/7 gates"), "{diagnostic}");

    let records = read_verified_records(&audit).expect("verified CLI audit chain");
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event.kind == EvolutionEventKind::GatePassed)
            .count(),
        7
    );
    assert_eq!(
        records.last().unwrap().event.kind,
        EvolutionEventKind::Promoted
    );

    let _ = std::fs::remove_file(audit);
    let _ = std::fs::remove_file(audit_head);
    let _ = std::fs::remove_file(candidate_out);
}

/// `--abi f64:2 --require-allocation-free --validate sandbox`: a control law
/// promoted after its tests and canary ran natively inside the sandbox.
#[test]
fn evolve_command_promotes_a_control_law_validated_in_the_sandbox() {
    if !is_orc_jit_available()
        || !iris::sandbox::NativeSandbox::detect()
            .availability()
            .is_enforced()
    {
        eprintln!("ORC or an enforced native sandbox is unavailable; test skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!("iris_evolve_cli_control_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let probe = "
def test_transaction_rollback() -> i64 effect alloc, transaction {
    val state: list<i64> = list()
    push(state, 10);
    transaction_begin();
    push(state, 99);
    transaction_rollback();
    if list_len(state) == 1 && list_get(state, 0) == 10 { 0 } else { 1 }
}
";
    std::fs::write(
        dir.join("baseline.iris"),
        "def policy(error: f64, rate: f64) -> f64 effect pure { 0.5 * error }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("candidate.iris"),
        format!(
            "def policy(error: f64, rate: f64) -> f64 effect pure {{ 2.0 * error - 0.5 * rate }}
def test_policy() -> i64 effect throw {{ assert(policy(1.0, 0.0) == 2.0); 0 }}
{probe}"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("cases.json"),
        r#"[{"inputs":[1.0,0.0],"expected":2.0},{"inputs":[2.0,2.0],"expected":3.0}]"#,
    )
    .unwrap();
    let constitution = dir.join("constitution.txt");
    std::fs::write(&constitution, "force within [-10, 10]").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_iris"))
        .arg("evolve")
        .args([
            "--abi",
            "f64:2",
            "--require-allocation-free",
            "--validate",
            "sandbox",
        ])
        .arg("--baseline")
        .arg(dir.join("baseline.iris"))
        .arg("--candidate")
        .arg(dir.join("candidate.iris"))
        .arg("--cases")
        .arg(dir.join("cases.json"))
        .arg("--constitution")
        .arg(&constitution)
        .arg("--constitution-sha256")
        .arg(sha256_hex(b"force within [-10, 10]"))
        .arg("--audit")
        .arg(dir.join("audit.jsonl"))
        .arg("--audit-head")
        .arg(dir.join("audit.head.json"))
        .args(["--min-output", "-10", "--max-output", "10"])
        .output()
        .expect("run iris evolve");
    let mut diagnostic = String::from_utf8_lossy(&output.stdout).into_owned();
    diagnostic.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{diagnostic}");
    assert!(diagnostic.contains("after 7/7 gates"), "{diagnostic}");
    assert!(diagnostic.contains("ABI (f64, f64) -> f64"), "{diagnostic}");
    assert!(diagnostic.contains("allocation-free proof"), "{diagnostic}");
    assert!(diagnostic.contains("sandboxed native run"), "{diagnostic}");
    assert!(diagnostic.contains("candidate 2/2"), "{diagnostic}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A scratch directory with a pinned constitution, removed by the caller.
fn evolve_fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("iris_evolve_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let constitution = dir.join("constitution.txt");
    std::fs::write(&constitution, "commands stay within bounds\n").unwrap();
    let hash = sha256_hex(&std::fs::read(&constitution).unwrap());
    (dir, constitution, hash)
}

const ROLLBACK_PROBE: &str = r#"
def test_transaction_rollback() -> i64 effect alloc, transaction, throw {
    val state: list<i64> = list(10);
    transaction_begin();
    list_push(state, 99);
    transaction_rollback();
    assert(transaction_depth() == 0);
    assert(list_len(state) == 1);
    0
}
"#;

#[test]
fn evolve_search_promotes_a_float_control_law_through_the_gates() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; float search promotion test skipped");
        return;
    }
    let (dir, constitution, hash) = evolve_fixture("float_search");
    let cases: Vec<serde_json::Value> = (-4..=4)
        .flat_map(|i| (-2..=2).map(move |j| (i as f64 * 0.5, j as f64 * 0.7)))
        .map(|(e, r)| serde_json::json!({ "inputs": [e, r], "expected": -1.5 * e - 0.4 * r }))
        .collect();
    std::fs::write(
        dir.join("cases.json"),
        serde_json::to_string(&cases).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("baseline.iris"),
        format!(
            "def policy(e: f64, r: f64) -> f64 {{ 0.0 }}\n\n\
             def test_policy() -> i64 effect throw {{ assert(policy(0.0, 0.0) == 0.0); 0 }}\n{ROLLBACK_PROBE}"
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args([
            "evolve-search",
            "--abi",
            "f64:2",
            "--generations",
            "20",
            "--pop-size",
            "120",
        ])
        .args([
            "--seed",
            "5",
            "--promote",
            "--min-output=-10",
            "--max-output",
            "10",
        ])
        .arg("--cases")
        .arg(dir.join("cases.json"))
        .arg("--baseline")
        .arg(dir.join("baseline.iris"))
        .arg("--constitution")
        .arg(&constitution)
        .arg("--constitution-sha256")
        .arg(hash)
        .arg("--audit")
        .arg(dir.join("audit.jsonl"))
        .arg("--audit-head")
        .arg(dir.join("audit.head.json"))
        .arg("--out-candidate")
        .arg(dir.join("winner.iris"))
        .output()
        .expect("run iris evolve-search");
    let mut diagnostic = String::from_utf8_lossy(&output.stdout).into_owned();
    diagnostic.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{diagnostic}");
    assert!(diagnostic.contains("after 7/7 gates"), "{diagnostic}");
    assert!(diagnostic.contains("candidate 45/45"), "{diagnostic}");
    let winner = std::fs::read_to_string(dir.join("winner.iris")).unwrap();
    assert!(
        winner.contains("def policy(x0: f64, x1: f64) -> f64"),
        "{winner}"
    );
    let records = read_verified_records(&dir.join("audit.jsonl")).unwrap();
    assert_eq!(
        records.last().unwrap().event.kind,
        EvolutionEventKind::Promoted
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn evolve_promotes_a_record_policy_with_field_bounds() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; record ABI CLI test skipped");
        return;
    }
    let (dir, constitution, hash) = evolve_fixture("record");
    let support = format!(
        "record CartState {{ position: f64, velocity: f64, step: i64 }}\n\
         record Command {{ force: f64, brake: bool }}\n\n\
         def test_policy() -> i64 effect throw {{\n\
         \x20   val c = policy(CartState {{ position: 0.5, velocity: 0.0, step: 3 }});\n\
         \x20   assert(c.force == policy(CartState {{ position: 0.5, velocity: 0.0, step: 3 }}).force);\n\
         \x20   0\n}}\n{ROLLBACK_PROBE}"
    );
    std::fs::write(
        dir.join("baseline.iris"),
        format!("{support}\ndef policy(s: CartState) -> Command {{ Command {{ force: 0.0, brake: false }} }}\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("candidate.iris"),
        format!(
            "{support}\ndef policy(s: CartState) -> Command {{\n\
             \x20   Command {{ force: -1.5 * s.position - 0.4 * s.velocity, brake: s.step > 100 }}\n}}\n"
        ),
    )
    .unwrap();
    let mut cases = Vec::new();
    for position in [-2.0, 0.0, 1.0] {
        for (velocity, step) in [(0.0, 1), (1.0, 200)] {
            cases.push(serde_json::json!({
                "input": { "position": position, "velocity": velocity, "step": step },
                "expected": { "force": -1.5 * position - 0.4 * velocity, "brake": step > 100 },
            }));
        }
    }
    std::fs::write(
        dir.join("cases.json"),
        serde_json::to_string(&cases).unwrap(),
    )
    .unwrap();

    let run = |bound: &str| {
        let _ = std::fs::remove_file(dir.join("audit.jsonl"));
        let _ = std::fs::remove_file(dir.join("audit.head.json"));
        Command::new(env!("CARGO_BIN_EXE_iris"))
            .args(["evolve", "--abi", "record", "--bound", bound])
            .arg("--baseline")
            .arg(dir.join("baseline.iris"))
            .arg("--candidate")
            .arg(dir.join("candidate.iris"))
            .arg("--cases")
            .arg(dir.join("cases.json"))
            .arg("--constitution")
            .arg(&constitution)
            .arg("--constitution-sha256")
            .arg(&hash)
            .arg("--audit")
            .arg(dir.join("audit.jsonl"))
            .arg("--audit-head")
            .arg(dir.join("audit.head.json"))
            .output()
            .expect("run iris evolve")
    };
    let output = run("force=-5:5");
    let mut diagnostic = String::from_utf8_lossy(&output.stdout).into_owned();
    diagnostic.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{diagnostic}");
    assert!(diagnostic.contains("after 7/7 gates"), "{diagnostic}");
    assert!(
        diagnostic.contains("ABI (record) -> record"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("candidate 6/6"), "{diagnostic}");

    // A tighter bound on the force field is a constitution failure.
    let output = run("force=-1:1");
    let diagnostic = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success());
    assert!(diagnostic.contains("safety_constitution"), "{diagnostic}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn evolve_promotes_a_whole_program_with_its_state() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; program evolution CLI test skipped");
        return;
    }
    let (dir, constitution, hash) = evolve_fixture("program");
    let support = format!(
        "def test_program() -> i64 effect throw {{ assert(add(1) == count); 0 }}\n{ROLLBACK_PROBE}"
    );
    std::fs::write(
        dir.join("baseline.iris"),
        format!(
            "var count: i64 = 0\nvar total: f64 = 0.0\n\
             def add(x: i64) -> i64 {{ count = count + 1; total = total + (x to f64); count }}\n\
             def mean() -> i64 {{ total to i64 }}\n{support}"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("candidate.iris"),
        format!(
            "var count: i64 = 0\nvar total: f64 = 0.0\n\
             def add(x: i64) -> i64 {{ count = count + 1; total = total + (x to f64); count }}\n\
             def mean() -> i64 {{ if count == 0 {{ 0 }} else {{ (total / (count to f64)) to i64 }} }}\n{support}"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("program.json"),
        serde_json::json!({
            "schema": "iris-evolution-program/2",
            "program_id": "running_mean",
            "entrypoints": [
                { "gateway": "add", "symbol": "add", "abi": "scalar_i64_v1" },
                { "gateway": "mean", "symbol": "mean", "abi": "command_v1" }
            ]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("scenarios.json"),
        serde_json::json!([
            { "name": "fresh", "steps": [
                { "gateway": "add", "input": 4, "expected": 1 },
                { "gateway": "add", "input": 6, "expected": 2 },
                { "gateway": "mean", "expected": 5 }
            ]},
            { "name": "resumed", "state": { "count": 3, "total": 30.0 }, "steps": [
                { "gateway": "mean", "expected": 10 }
            ]}
        ])
        .to_string(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_iris"))
        .arg("evolve")
        .arg("--manifest")
        .arg(dir.join("program.json"))
        .arg("--scenarios")
        .arg(dir.join("scenarios.json"))
        .arg("--baseline")
        .arg(dir.join("baseline.iris"))
        .arg("--candidate")
        .arg(dir.join("candidate.iris"))
        .arg("--constitution")
        .arg(&constitution)
        .arg("--constitution-sha256")
        .arg(&hash)
        .arg("--audit")
        .arg(dir.join("audit.jsonl"))
        .arg("--audit-head")
        .arg(dir.join("audit.head.json"))
        .args(["--min-output=-1000", "--max-output", "1000"])
        .output()
        .expect("run iris evolve --manifest");
    let mut diagnostic = String::from_utf8_lossy(&output.stdout).into_owned();
    diagnostic.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{diagnostic}");
    assert!(
        diagnostic.contains("promoted program running_mean"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("after 7/7 gates"), "{diagnostic}");
    assert!(
        diagnostic.contains("state carries count, total"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("candidate 4/4 exact"), "{diagnostic}");
    let records = read_verified_records(&dir.join("audit.jsonl")).unwrap();
    assert_eq!(
        records.last().unwrap().event.kind,
        EvolutionEventKind::Promoted
    );
    let _ = std::fs::remove_dir_all(dir);
}
