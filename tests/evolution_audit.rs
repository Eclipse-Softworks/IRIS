#[path = "../src/evolution/audit.rs"]
mod audit;

use audit::{
    read_verified_records, verify_file, AuditError, EvolutionAuditLog, EvolutionEvent,
    EvolutionEventKind,
};
use std::path::PathBuf;

fn temp_log(name: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "iris_evolution_audit_{name}_{}_{}.jsonl",
        std::process::id(),
        nonce
    ))
}

fn event(kind: EvolutionEventKind, candidate: &str) -> EvolutionEvent {
    let mut event = EvolutionEvent::new(kind, candidate);
    event.constitution_hash = Some("constitution-v1".to_owned());
    event.policy_hash = Some("policy-v1".to_owned());
    event
        .evidence
        .insert("assertions".to_owned(), "3".to_owned());
    event
}

#[test]
fn appends_durable_records_and_reopens_at_the_verified_head() {
    let path = temp_log("append");
    let mut log = EvolutionAuditLog::open(&path).expect("open new audit log");
    assert_eq!(log.head().sequence, 0);

    let first = log
        .append(event(EvolutionEventKind::CandidateReceived, "candidate-a"))
        .expect("append candidate event");
    let mut promoted = event(EvolutionEventKind::Promoted, "candidate-a");
    promoted.generation = Some(7);
    promoted.decision = Some("canary thresholds satisfied".to_owned());
    let second = log.append_at(promoted, 200).expect("append promotion");

    assert_eq!(first.sequence, 1);
    assert!(first.timestamp_ms > 0);
    assert_eq!(second.sequence, 2);
    assert_eq!(second.previous_hash, first.hash);
    assert_eq!(log.path(), path.as_path());
    assert_eq!(log.verify().unwrap(), *log.head());
    assert_eq!(verify_file(&path).unwrap(), *log.head());

    let reopened = EvolutionAuditLog::open_with_expected_head(&path, log.head())
        .expect("reopen with retained checkpoint");
    assert_eq!(reopened.head(), log.head());
    let records = read_verified_records(&path).expect("read verified records");
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].event.generation, Some(7));

    std::fs::remove_file(path).unwrap();
}

#[test]
fn detects_record_content_tampering() {
    let path = temp_log("tamper");
    let mut log = EvolutionAuditLog::open(&path).unwrap();
    log.append_at(
        event(EvolutionEventKind::GatePassed, "candidate-original"),
        100,
    )
    .unwrap();

    let original = std::fs::read_to_string(&path).unwrap();
    assert!(original.contains("candidate-original"));
    let tampered = original.replace("candidate-original", "candidate-attacker");
    assert_ne!(tampered, original);
    std::fs::write(&path, tampered).unwrap();

    let error = verify_file(&path).expect_err("changed event must break its record hash");
    assert!(
        matches!(error, AuditError::RecordHashMismatch { line: 1, .. }),
        "unexpected verification error: {error}"
    );

    std::fs::remove_file(path).unwrap();
}

#[test]
fn expected_head_detects_valid_tail_truncation() {
    let path = temp_log("truncate");
    let mut log = EvolutionAuditLog::open(&path).unwrap();
    log.append_at(
        event(EvolutionEventKind::CandidateReceived, "candidate-a"),
        100,
    )
    .unwrap();
    log.append_at(event(EvolutionEventKind::Promoted, "candidate-a"), 200)
        .unwrap();
    let retained_head = log.head().clone();

    let contents = std::fs::read_to_string(&path).unwrap();
    let first_line = contents.lines().next().unwrap();
    std::fs::write(&path, format!("{first_line}\n")).unwrap();

    // The remaining prefix is internally valid, so ordinary chain validation
    // alone cannot prove that its former tail existed.
    assert_eq!(verify_file(&path).unwrap().sequence, 1);
    let error = match EvolutionAuditLog::open_with_expected_head(&path, &retained_head) {
        Ok(_) => panic!("external head checkpoint must detect deleted tail"),
        Err(error) => error,
    };
    assert!(
        matches!(error, AuditError::ExpectedHeadMismatch { .. }),
        "unexpected verification error: {error}"
    );

    std::fs::remove_file(path).unwrap();
}

#[test]
fn persisted_checkpoint_is_advanced_and_detects_tail_truncation() {
    let path = temp_log("checkpointed");
    let checkpoint = path.with_extension("head.json");
    let mut log = EvolutionAuditLog::open_with_checkpoint(&path, &checkpoint).unwrap();
    log.append_at(
        event(EvolutionEventKind::CandidateReceived, "candidate-a"),
        100,
    )
    .unwrap();
    log.append_at(event(EvolutionEventKind::Promoted, "candidate-a"), 200)
        .unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let first_line = contents.lines().next().unwrap();
    std::fs::write(&path, format!("{first_line}\n")).unwrap();
    let error = match EvolutionAuditLog::open_with_checkpoint(&path, &checkpoint) {
        Ok(_) => panic!("persisted checkpoint must detect deleted audit tail"),
        Err(error) => error,
    };
    assert!(matches!(error, AuditError::ExpectedHeadMismatch { .. }));

    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(checkpoint).unwrap();
}

#[test]
fn nonempty_log_without_checkpoint_fails_closed() {
    let path = temp_log("missing_checkpoint");
    let checkpoint = path.with_extension("head.json");
    let mut log = EvolutionAuditLog::open(&path).unwrap();
    log.append_at(
        event(EvolutionEventKind::CandidateReceived, "candidate-a"),
        100,
    )
    .unwrap();

    let error = match EvolutionAuditLog::open_with_checkpoint(&path, &checkpoint) {
        Ok(_) => panic!("a nonempty log must not silently create a new trust anchor"),
        Err(error) => error,
    };
    assert!(matches!(error, AuditError::MissingCheckpoint { .. }));

    std::fs::remove_file(path).unwrap();
}

#[test]
fn append_fails_closed_if_log_changes_after_open() {
    let path = temp_log("head_changed");
    let mut first_writer = EvolutionAuditLog::open(&path).unwrap();
    let mut second_writer = EvolutionAuditLog::open(&path).unwrap();
    first_writer
        .append_at(
            event(EvolutionEventKind::CandidateReceived, "candidate-a"),
            100,
        )
        .unwrap();

    let error = second_writer
        .append_at(event(EvolutionEventKind::GatePassed, "candidate-b"), 200)
        .expect_err("stale writer must not fork the chain");
    assert!(
        matches!(error, AuditError::HeadChanged { .. }),
        "unexpected append error: {error}"
    );
    assert_eq!(read_verified_records(&path).unwrap().len(), 1);

    std::fs::remove_file(path).unwrap();
}
