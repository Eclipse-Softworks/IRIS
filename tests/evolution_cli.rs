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
