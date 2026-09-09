use iris::sandbox::protocol::{
    read_json_frame, write_json_frame, ProtocolError, WorkerOperation, WorkerRequest,
    MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
use std::io::Cursor;

#[test]
fn request_round_trips_through_length_framing() {
    let request = WorkerRequest::new(
        "candidate_42",
        WorkerOperation::ValidateArtifact {
            artifact_sha256: "ab".repeat(32),
            entrypoint: "policy".into(),
        },
    );
    request.validate().unwrap();

    let mut wire = Vec::new();
    write_json_frame(&mut wire, &request).unwrap();
    assert_eq!(
        u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize,
        wire.len() - 4
    );

    let decoded: WorkerRequest = read_json_frame(&mut Cursor::new(wire)).unwrap();
    assert_eq!(decoded, request);
}

#[test]
fn oversized_announced_frame_is_rejected_before_payload_allocation() {
    let announced = u32::try_from(MAX_FRAME_BYTES + 1).unwrap();
    let error =
        read_json_frame::<_, WorkerRequest>(&mut Cursor::new(announced.to_be_bytes())).unwrap_err();
    assert!(matches!(error, ProtocolError::FrameTooLarge { .. }));
}

#[test]
fn truncated_frame_and_unknown_fields_fail_closed() {
    let mut truncated = Vec::from(100_u32.to_be_bytes());
    truncated.extend_from_slice(b"{}");
    assert!(matches!(
        read_json_frame::<_, WorkerRequest>(&mut Cursor::new(truncated)).unwrap_err(),
        ProtocolError::Io(_)
    ));

    let json = format!(
        r#"{{"version":{PROTOCOL_VERSION},"request_id":"r1","operation":{{"kind":"shutdown"}},"unexpected":true}}"#
    );
    let mut wire = Vec::from((json.len() as u32).to_be_bytes());
    wire.extend_from_slice(json.as_bytes());
    assert!(matches!(
        read_json_frame::<_, WorkerRequest>(&mut Cursor::new(wire)).unwrap_err(),
        ProtocolError::Json(_)
    ));
}

#[test]
fn protocol_version_and_request_id_are_validated() {
    let mut wrong = WorkerRequest::new("ok", WorkerOperation::Shutdown);
    wrong.version += 1;
    assert!(matches!(
        wrong.validate().unwrap_err(),
        ProtocolError::UnsupportedVersion(_)
    ));

    let invalid = WorkerRequest::new("../../escape", WorkerOperation::Shutdown);
    assert!(matches!(
        invalid.validate().unwrap_err(),
        ProtocolError::InvalidRequestId
    ));
}
