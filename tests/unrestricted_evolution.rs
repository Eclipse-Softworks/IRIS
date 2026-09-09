use iris::evolution::manifest::{
    HostAbi, ProgramEntrypoint, ProgramManifest, PROGRAM_MANIFEST_SCHEMA,
};
use iris::evolution::unrestricted::{
    acknowledge_unrestricted, UnrestrictedEvolutionAuthority, UNRESTRICTED_ACKNOWLEDGEMENT,
};

fn manifest() -> ProgramManifest {
    ProgramManifest {
        schema: PROGRAM_MANIFEST_SCHEMA.to_owned(),
        program_id: "unsafe_counter".to_owned(),
        entrypoints: vec![ProgramEntrypoint {
            gateway: "step".to_owned(),
            symbol: "step".to_owned(),
            abi: HostAbi::ScalarI64V1,
        }],
        state: None,
    }
}

#[test]
fn unrestricted_authority_requires_exact_acknowledgement() {
    let error = acknowledge_unrestricted("yes").expect_err("weak acknowledgement was accepted");
    assert!(error.to_string().contains(UNRESTRICTED_ACKNOWLEDGEMENT));
}

#[test]
fn unrestricted_authority_activates_effectful_source_and_rolls_back() {
    let token = acknowledge_unrestricted(UNRESTRICTED_ACKNOWLEDGEMENT).unwrap();
    let authority = UnrestrictedEvolutionAuthority::with_history_limit(token, 4);
    let first = r#"
def step(x: i64) -> i64 effect io {
    println("unrestricted generation one");
    return x + 1
}
"#;
    let second = r#"
def step(x: i64) -> i64 effect io {
    println("unrestricted generation two");
    return x + 10
}
"#;

    let first_receipt = authority
        .compile_and_activate(first, "unsafe_generation_one", &manifest())
        .unwrap();
    assert_eq!(first_receipt.swap.replaced_generation, None);
    assert_eq!(
        authority
            .lease_program("unsafe_counter")
            .unwrap()
            .call_i64_1("step", 5)
            .unwrap(),
        6
    );

    let second_receipt = authority
        .compile_and_activate(second, "unsafe_generation_two", &manifest())
        .unwrap();
    assert_eq!(
        authority
            .lease_program("unsafe_counter")
            .unwrap()
            .call_i64_1("step", 5)
            .unwrap(),
        15
    );

    let rollback = authority.rollback(&second_receipt).unwrap();
    assert_eq!(rollback.generation, first_receipt.swap.generation);
    assert_eq!(
        authority
            .lease_program("unsafe_counter")
            .unwrap()
            .call_i64_1("step", 5)
            .unwrap(),
        6
    );
}
