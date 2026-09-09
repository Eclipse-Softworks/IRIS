//! Explicit host authority: compile, atomically activate, lease, and roll back.
//! Run: cargo run --example unrestricted_evolution
//! Candidates have the host process's capabilities; behavioral gates are skipped.
use iris::evolution::manifest::{
    HostAbi, ProgramEntrypoint, ProgramManifest, PROGRAM_MANIFEST_SCHEMA,
};
use iris::evolution::unrestricted::{
    acknowledge_unrestricted, UnrestrictedEvolutionAuthority, UNRESTRICTED_ACKNOWLEDGEMENT,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = acknowledge_unrestricted(UNRESTRICTED_ACKNOWLEDGEMENT)?;
    let authority = UnrestrictedEvolutionAuthority::with_history_limit(token, 4);
    let manifest = ProgramManifest {
        schema: PROGRAM_MANIFEST_SCHEMA.to_owned(),
        program_id: "learning_counter".to_owned(),
        entrypoints: vec![ProgramEntrypoint {
            gateway: "step".to_owned(),
            symbol: "step".to_owned(),
            abi: HostAbi::ScalarI64V1,
        }],
        state: None,
    };
    let first = authority.compile_and_activate(
        "def step(value: i64) -> i64 { return value + 1 }",
        "generation_one",
        &manifest,
    )?;
    let original = authority.lease_program("learning_counter")?;
    assert_eq!(original.call_i64_1("step", 5)?, 6);
    let second = authority.compile_and_activate(
        "def step(value: i64) -> i64 { return value + 10 }",
        "generation_two",
        &manifest,
    )?;
    assert_eq!(
        authority
            .lease_program("learning_counter")?
            .call_i64_1("step", 5)?,
        15
    );
    assert_eq!(original.call_i64_1("step", 5)?, 6);
    assert_eq!(
        authority.rollback(&second)?.generation,
        first.swap.generation
    );
    assert_eq!(
        authority
            .lease_program("learning_counter")?
            .call_i64_1("step", 5)?,
        6
    );
    println!("unrestricted host: activation, lease, rollback checked");
    Ok(())
}
