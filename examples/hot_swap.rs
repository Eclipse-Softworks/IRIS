//! Embedding IRIS from Rust: native ORC, verified replacement, leases, rollback.
//! Run from the repository: cargo run --example hot_swap
use iris::codegen::hot_swap::HotSwapEngine;
use iris::compile_to_module;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let first = compile_to_module("def value() -> i64 { return 21 }", "first")?;
    let second = compile_to_module("def value() -> i64 { return 42 }", "second")?;
    let swaps = HotSwapEngine::new();
    let installed = swaps.install_verified(&first, "value", |candidate| {
        assert_eq!(candidate.call_i64_0("value")?, 21);
        Ok(())
    })?;
    let in_flight = swaps.lease("value")?;
    let replacement = swaps.install_verified(&second, "value", |candidate| {
        assert_eq!(candidate.call_i64_0("value")?, 42);
        Ok(())
    })?;
    assert_eq!(swaps.lease("value")?.call_i64_0()?, 42);
    assert_eq!(in_flight.call_i64_0()?, 21);
    let restored = swaps.rollback_generation("value", replacement.generation)?;
    assert_eq!(restored.generation, installed.generation);
    assert_eq!(swaps.lease("value")?.call_i64_0()?, 21);
    println!("hot swap: validated replacement, retained lease, rollback checked");
    Ok(())
}
