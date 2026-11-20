use iris::compile_to_module;

fn opt_in() {
    std::env::set_var("IRIS_STRICT_RECORD_OWNERSHIP", "1");
}

#[test]
fn implicit_call_move_is_checked_when_opted_in() {
    opt_in();
    let src = r#"
record Packet {
    value: i64,
}
def consume(p: Packet) -> i64 { return p.value }
def transfer() -> i64 {
    val packet = Packet { value: 17 }
    val seen = consume(packet)
    val again = packet.value
    return seen + again
}
"#;
    let err = compile_to_module(src, "strict_call_move").expect_err("reuse after move must fail");
    assert_eq!(err.diagnostic_code(), "E0382", "got: {err}");
}

#[test]
fn implicit_binding_move_is_checked_when_opted_in() {
    opt_in();
    let src = r#"
record Packet {
    value: i64,
}
def transfer() -> i64 {
    val first = Packet { value: 3 }
    val second = first
    val again = first.value
    return second.value + again
}
"#;
    let err =
        compile_to_module(src, "strict_binding_move").expect_err("reuse after move must fail");
    assert_eq!(err.diagnostic_code(), "E0382", "got: {err}");
}
