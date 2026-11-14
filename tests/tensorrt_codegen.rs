//! The TensorRT backend: a `model` compiles to ONNX, from which TensorRT's
//! `trtexec` builds a serialized engine. Without TensorRT installed, asking
//! for an engine says what is missing.

use std::path::Path;

use iris::compile_multi;
use iris::EmitKind;

const MODEL_SRC: &str = r#"
model TensorRtNet {
    input x: tensor<f32, [1, 4]>
    layer y Dense(units=8)
    output y
}
"#;

const DYNAMIC_SRC: &str = r#"
model Dyn {
    input x: tensor<f32, [N, 4]>
    layer y Dense(units=8)
    output y
}
"#;

fn trtexec_on_path() -> bool {
    let exe = if cfg!(windows) {
        "trtexec.exe"
    } else {
        "trtexec"
    };
    std::env::var("TENSORRT_ROOT").is_ok_and(|r| Path::new(&r).join("bin").join(exe).is_file())
        || std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()))
}

#[test]
fn test_tensorrt_engine_compilation() {
    let res = compile_multi(&[("main", MODEL_SRC)], "main", EmitKind::TensorRt);
    if trtexec_on_path() {
        let hex = res.expect("trtexec builds the engine");
        assert!(hex.len() > 1000, "a serialized engine");
    } else {
        let err = res.expect_err("no TensorRT here").to_string();
        assert!(
            err.contains("trtexec"),
            "the error names the missing tool: {err}"
        );
        assert!(err.contains("TENSORRT_ROOT"), "{err}");
    }
}

fn graph(src: &str) -> iris::ir::graph::GraphIr {
    let tokens = iris::parser::lexer::Lexer::new(src)
        .tokenize()
        .expect("lex");
    let ast = iris::parser::parse::Parser::new(&tokens)
        .parse_module()
        .expect("parse");
    iris::lower::lower_model(&ast.models[0]).expect("lower")
}

#[test]
fn test_trtexec_arguments_for_fixed_shapes() {
    let args = iris::codegen::tensorrt::trtexec_args(
        &graph(MODEL_SRC),
        Path::new("m.onnx"),
        Path::new("m.engine"),
    )
    .unwrap();
    assert_eq!(
        args,
        vec![
            "--onnx=m.onnx".to_string(),
            "--saveEngine=m.engine".to_string()
        ]
    );
}

#[test]
fn test_trtexec_arguments_profile_named_dimensions() {
    let args = iris::codegen::tensorrt::trtexec_args(
        &graph(DYNAMIC_SRC),
        Path::new("m.onnx"),
        Path::new("m.engine"),
    )
    .unwrap();
    assert!(args.contains(&"--minShapes=x:1x4".to_string()), "{args:?}");
    assert!(args.contains(&"--optShapes=x:1x4".to_string()), "{args:?}");
    assert!(args.contains(&"--maxShapes=x:64x4".to_string()), "{args:?}");
}

#[test]
fn test_tensorrt_needs_exactly_one_model() {
    let two = format!("{MODEL_SRC}\n{}", DYNAMIC_SRC);
    let err = compile_multi(&[("main", two.as_str())], "main", EmitKind::TensorRt)
        .unwrap_err()
        .to_string();
    assert!(err.contains("one `model`"), "{err}");
}
