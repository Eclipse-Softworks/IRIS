use iris::cli::{parse_args, ParseArgsResult};
use iris::evolution::unrestricted::UNRESTRICTED_ACKNOWLEDGEMENT;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn parses_meta_inspection_command() {
    let parsed = parse_args(&args(&["iris", "meta", "program.iris", "--emit-ir"])).unwrap();
    match parsed {
        ParseArgsResult::Meta { file, emit_ir } => {
            assert_eq!(file.to_string_lossy(), "program.iris");
            assert!(emit_ir);
        }
        other => panic!("unexpected parse result: {other:?}"),
    }
}

#[test]
fn parses_explicit_unrestricted_activation_command() {
    let parsed = parse_args(&args(&[
        "iris",
        "evolve-unrestricted",
        "--candidate",
        "candidate.iris",
        "--manifest",
        "manifest.json",
        "--acknowledge-unsafe",
        UNRESTRICTED_ACKNOWLEDGEMENT,
    ]))
    .unwrap();
    match parsed {
        ParseArgsResult::EvolveUnrestricted {
            candidate,
            manifest,
            acknowledge_unsafe,
        } => {
            assert_eq!(candidate.to_string_lossy(), "candidate.iris");
            assert_eq!(manifest.to_string_lossy(), "manifest.json");
            assert_eq!(acknowledge_unsafe, UNRESTRICTED_ACKNOWLEDGEMENT);
        }
        other => panic!("unexpected parse result: {other:?}"),
    }
}
