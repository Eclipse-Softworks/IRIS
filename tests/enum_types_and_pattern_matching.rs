//! Phase 15 integration tests: enum types and pattern matching.

use iris::interp::{eval_function, IrValue};
use iris::ir::instr::IrInstr;
use iris::ir::module::IrFunctionBuilder;
use iris::ir::types::{DType, IrType};
use iris::{compile, EmitKind};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn i64_ty() -> IrType {
    IrType::Scalar(DType::I64)
}

// ---------------------------------------------------------------------------
// 1. Lexer/parser: `choice` keyword is recognized
// ---------------------------------------------------------------------------
#[test]
fn test_choice_keyword_lexed() {
    let src = r#"
choice Color { Red, Green, Blue }
def zero() -> i64 { 0 }
"#;
    let result = compile(src, "test", EmitKind::Ir);
    assert!(
        result.is_ok(),
        "choice keyword should be recognized: {:?}",
        result.err()
    );
}

// ---------------------------------------------------------------------------
// 2. Parser: enum definition with multiple variants
// ---------------------------------------------------------------------------
#[test]
fn test_choice_variants_parsed() {
    let src = r#"
choice Direction { North, South, East, West }
def go() -> i64 { 1 }
"#;
    let out = compile(src, "test", EmitKind::Ir).expect("should compile");
    assert!(out.contains("def go"), "IR should contain the function");
}

// ---------------------------------------------------------------------------
// 3. Enum variant construction: `Color.Red` lowers to make_variant
// ---------------------------------------------------------------------------
#[test]
fn test_enum_variant_construction_ir() {
    let src = r#"
choice Color { Red, Green, Blue }
def get_red() -> Color {
    Color.Red
}
"#;
    let out = compile(src, "test", EmitKind::Ir).expect("should compile");
    assert!(
        out.contains("make_variant"),
        "IR should contain make_variant: {}",
        out
    );
}

// ---------------------------------------------------------------------------
// 4. `when` expression compiles
// ---------------------------------------------------------------------------
#[test]
fn test_when_expression_compiles() {
    let src = r#"
choice Color { Red, Green, Blue }
def color_code(c: Color) -> i64 {
    when c {
        Color.Red => 0
        Color.Green => 1
        Color.Blue => 2
    }
}
"#;
    let result = compile(src, "test", EmitKind::Ir);
    assert!(
        result.is_ok(),
        "when expression should compile: {:?}",
        result.err()
    );
}

// ---------------------------------------------------------------------------
// 5. IR contains switch_variant and make_variant
// ---------------------------------------------------------------------------
#[test]
fn test_when_ir_has_switch_variant() {
    let src = r#"
choice Dir { Up, Down }
def pick(d: Dir) -> i64 {
    when d {
        Dir.Up => 10
        Dir.Down => 20
    }
}
"#;
    let out = compile(src, "test", EmitKind::Ir).expect("should compile");
    assert!(
        out.contains("switch_variant"),
        "IR should contain switch_variant: {}",
        out
    );
}

// ---------------------------------------------------------------------------
// 6. Interpreter: MakeVariant produces IrValue::Enum(idx)
// ---------------------------------------------------------------------------
#[test]
fn test_interp_make_variant() {
    let enum_ty = IrType::Enum {
        name: "Color".into(),
        variants: vec!["Red".into(), "Green".into(), "Blue".into()],
    };
    let mut b = IrFunctionBuilder::new("get_green", vec![], enum_ty.clone());
    let entry = b.create_block(Some("entry"));
    b.set_current_block(entry);

    let v = b.fresh_value();
    b.push_instr(
        IrInstr::MakeVariant {
            result: v,
            variant_idx: 1,
            fields: vec![],
            result_ty: enum_ty,
        },
        Some(i64_ty()),
    );
    b.push_instr(IrInstr::Return { values: vec![v] }, None);
    let func = b.build();

    let result = eval_function(&func, &[]).expect("should eval");
    assert_eq!(result, vec![IrValue::Enum(1, vec![])]);
}

// ---------------------------------------------------------------------------
// 7. Interpreter: SwitchVariant dispatches to the correct arm
// ---------------------------------------------------------------------------
#[test]
fn test_interp_switch_variant() {
    let enum_ty = IrType::Enum {
        name: "Dir".into(),
        variants: vec!["Up".into(), "Down".into()],
    };
    let mut b = IrFunctionBuilder::new("pick", vec![], i64_ty());
    let entry = b.create_block(Some("entry"));
    let up_bb = b.create_block(Some("arm_up"));
    let down_bb = b.create_block(Some("arm_down"));
    let merge_bb = b.create_block(Some("merge"));
    let merge_param = b.add_block_param(merge_bb, Some("result"), i64_ty());

    b.set_current_block(entry);
    let tag = b.fresh_value();
    b.push_instr(
        IrInstr::MakeVariant {
            result: tag,
            variant_idx: 0,
            fields: vec![],
            result_ty: enum_ty,
        },
        Some(i64_ty()),
    );
    b.push_instr(
        IrInstr::SwitchVariant {
            scrutinee: tag,
            arms: vec![(0, up_bb), (1, down_bb)],
            default_block: None,
        },
        None,
    );

    b.set_current_block(up_bb);
    let ten = b.fresh_value();
    b.push_instr(
        IrInstr::ConstInt {
            result: ten,
            value: 10,
            ty: i64_ty(),
        },
        Some(i64_ty()),
    );
    b.push_instr(
        IrInstr::Br {
            target: merge_bb,
            args: vec![ten],
        },
        None,
    );

    b.set_current_block(down_bb);
    let twenty = b.fresh_value();
    b.push_instr(
        IrInstr::ConstInt {
            result: twenty,
            value: 20,
            ty: i64_ty(),
        },
        Some(i64_ty()),
    );
    b.push_instr(
        IrInstr::Br {
            target: merge_bb,
            args: vec![twenty],
        },
        None,
    );

    b.set_current_block(merge_bb);
    b.push_instr(
        IrInstr::Return {
            values: vec![merge_param],
        },
        None,
    );

    let func = b.build();
    let result = eval_function(&func, &[]).expect("should eval");
    assert_eq!(result, vec![IrValue::I64(10)]);
}

// ---------------------------------------------------------------------------
// 8. End-to-end: enum + when through compile() + EmitKind::Eval
// ---------------------------------------------------------------------------
#[test]
fn test_enum_e2e_eval() {
    let src = r#"
choice Season { Spring, Summer, Autumn, Winter }
def temp() -> i64 {
    val s = Season.Summer
    when s {
        Season.Spring => 15
        Season.Summer => 30
        Season.Autumn => 10
        Season.Winter => 0
    }
}
"#;
    let out = compile(src, "test", EmitKind::Eval).expect("should eval");
    assert_eq!(out.trim(), "30", "Summer should map to 30");
}

static PARITY_TEST_ID: AtomicUsize = AtomicUsize::new(0);

fn assert_native_and_interp_stdout(name: &str, src: &str, expected_stdout: &str) {
    let id = PARITY_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("iris_enum_eq_{}_{}", std::process::id(), id));
    std::fs::create_dir_all(&dir).expect("create temp test dir");
    let file = dir.join(format!("{name}.iris"));
    std::fs::write(&file, src).expect("write temp iris source");

    for force_interp in [false, true] {
        let backend = if force_interp { "interp" } else { "llvm" };
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_iris"));
        cmd.args(["run"]).arg(&file);
        if force_interp {
            cmd.env("IRIS_FORCE_INTERP", "1");
        } else {
            cmd.env_remove("IRIS_FORCE_INTERP");
        }
        let out = cmd.output().expect("launch iris binary");
        let stdout = String::from_utf8_lossy(&out.stdout)
            .replace("\r\n", "\n")
            .trim()
            .to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            out.status.success(),
            "[{name} @ {backend}] exited {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            out.status.code()
        );
        assert_eq!(
            stdout, expected_stdout,
            "[{name} @ {backend}] stdout mismatch\nstderr:\n{stderr}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_enum_equality_tag_only_payload_struct_and_chan_parity() {
    let src = r#"
choice Corridor {
    GulfOfFinland,
    DoverStrait,
    Skagerrak,
}

choice Shape {
    Circle(f64),
    Label(str, Shape),
    Dot,
}

record Config {
    corridor: Corridor,
    shape: Shape,
    offline: bool,
}

def make_corridor_indirect(flag: bool) -> Corridor {
    if flag {
        Corridor.GulfOfFinland
    } else {
        Corridor.DoverStrait
    }
}

def main() -> i64 effect io, alloc, thread {
    val cfg0 = Config { corridor: Corridor.GulfOfFinland, shape: Shape.Dot, offline: true }
    val cfg0b = Config { corridor: make_corridor_indirect(true), shape: Shape.Dot, offline: true }
    val cfg1 = Config { corridor: Corridor.DoverStrait, shape: Shape.Circle(2.5), offline: true }
    val cfg1b = Config { corridor: Corridor.DoverStrait, shape: Shape.Circle(2.5), offline: true }
    val cfg1c = Config { corridor: Corridor.DoverStrait, shape: Shape.Circle(3.0), offline: true }

    println(cfg0.corridor == cfg0b.corridor)
    println(cfg0.corridor != cfg1.corridor)
    println(cfg0.corridor == make_corridor_indirect(true))
    println(cfg0.corridor == make_corridor_indirect(false))

    println(cfg0 == cfg0b)
    println(cfg1 == cfg1b)
    println(cfg1 != cfg1c)

    val s1 = Shape.Label("a", Shape.Circle(1.5))
    val s2 = Shape.Label("a", Shape.Circle(1.5))
    val s3 = Shape.Label("b", Shape.Circle(1.5))
    val d = Shape.Dot
    println(s1 == s2)
    println(s1 != s3)
    println(d == Shape.Dot)
    println(d != s1)

    val o0: option<Corridor> = some(cfg0.corridor)
    val o0b: option<Corridor> = some(Corridor.GulfOfFinland)
    val o1: option<Corridor> = some(cfg1.corridor)
    val o1b: option<Corridor> = some(Corridor.DoverStrait)
    println(o0 == o0b)
    println(o1 == o1b)
    println(o0 != o1)

    val ls0: list<Shape> = list()
    list_push(ls0, Shape.Circle(1.0))
    list_push(ls0, Shape.Dot)
    val ls1: list<Shape> = list()
    list_push(ls1, Shape.Circle(1.0))
    list_push(ls1, Shape.Dot)
    println(ls0 == ls1)
    val lc0: list<Corridor> = list()
    list_push(lc0, cfg0.corridor)
    list_push(lc0, cfg1.corridor)
    val lc1: list<Corridor> = list()
    list_push(lc1, Corridor.GulfOfFinland)
    list_push(lc1, Corridor.DoverStrait)
    println(lc0 == lc1)

    val ch: chan<Config> = channel(2)
    spawn {
        send(ch, Config { corridor: Corridor.GulfOfFinland, shape: Shape.Circle(4.0), offline: true })
    }
    val received = recv(ch)
    println(received.corridor == cfg0.corridor)
    println(received.shape == Shape.Circle(4.0))
    println(received.corridor != cfg1.corridor)
    return 0
}
"#;
    let expected = [
        "true", "true", "true", "false", "true", "true", "true", "true", "true", "true", "true",
        "true", "true", "true", "true", "true", "true", "true", "true",
    ]
    .join("\n");
    assert_native_and_interp_stdout("enum_eq_parity", src, &expected);
}
