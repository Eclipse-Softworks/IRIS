//! Phase 14 integration tests: binary ONNX protobuf output.

use iris::proto::{encode_string_field, encode_varint};
use iris::{compile, EmitKind};

// Helper: decode hex string back to bytes.
fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

// Minimal model DSL source with one Dense (MatMul + Add) layer.
const MATMUL_MODEL: &str = r#"
model MatMulNet {
    input x: tensor<f32, [1, 4]>
    layer y Dense(units=8)
    output y
}
"#;

// Two-layer model for size check.
const TWO_LAYER_MODEL: &str = r#"
model Net {
    input x: tensor<f32, [1, 8]>
    layer h1 Dense(units=16)
    layer h2 ReLU(h1)
    output h2
}
"#;

// ---------------------------------------------------------------------------
// 1. encode_varint(1) == [0x01]
// ---------------------------------------------------------------------------
#[test]
fn test_proto_varint_1() {
    assert_eq!(encode_varint(1), vec![0x01]);
}

// ---------------------------------------------------------------------------
// 2. encode_varint(300) == [0xAC, 0x02]
// ---------------------------------------------------------------------------
#[test]
fn test_proto_varint_300() {
    // 300 = 0b1_0010_1100
    // LEB-128: low 7 bits = 0101100 (0x2C), set high bit → 0xAC
    //          remaining = 0b10 = 2 → 0x02
    assert_eq!(encode_varint(300), vec![0xAC, 0x02]);
}

// ---------------------------------------------------------------------------
// 3. encode_string_field(1, "hello") has correct tag and content
// ---------------------------------------------------------------------------
#[test]
fn test_proto_string_field() {
    // Field 1, wire type 2 → tag byte = (1 << 3) | 2 = 0x0A
    // Length = 5 → 0x05
    // UTF-8: b'h' b'e' b'l' b'l' b'o'
    let expected = vec![0x0A, 0x05, b'h', b'e', b'l', b'l', b'o'];
    assert_eq!(encode_string_field(1, "hello"), expected);
}

// ---------------------------------------------------------------------------
// 4. Output starts with ModelProto ir_version field (field 1, varint, value 7)
// ---------------------------------------------------------------------------
#[test]
fn test_onnx_binary_starts_with_magic() {
    let hex =
        compile(MATMUL_MODEL, "test", EmitKind::OnnxBinary).expect("should compile to ONNX binary");
    let bytes = hex_to_bytes(&hex);
    assert!(!bytes.is_empty(), "output should not be empty");
    // ModelProto field 1 (ir_version), wire type 0: tag = 0x08
    // ir_version = 8, the IR version of opset 17: encoded as 0x08
    assert_eq!(
        bytes[0], 0x08,
        "first byte should be field-1 varint tag (0x08)"
    );
    assert_eq!(bytes[1], 0x08, "second byte should be ir_version = 8");
}

// ---------------------------------------------------------------------------
// 5. A Dense layer is MatMul + Add, with its weight and bias as initializers
// ---------------------------------------------------------------------------
#[test]
fn test_onnx_binary_matmul_node() {
    let hex = compile(MATMUL_MODEL, "test", EmitKind::OnnxBinary).expect("should compile");
    let bytes = hex_to_bytes(&hex);
    for needle in [&b"MatMul"[..], b"Add", b"y.weight", b"y.bias"] {
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "expected {:?} in the ONNX output",
            String::from_utf8_lossy(needle)
        );
    }
    // The 4x8 weight's bytes: raw_data of 32 floats.
    assert!(bytes.len() > 32 * 4, "the weights are in the model");
}

// ---------------------------------------------------------------------------
// 6. Output contains the graph name "Linear" or "Net" as bytes
// ---------------------------------------------------------------------------
#[test]
fn test_onnx_binary_has_graph() {
    let hex = compile(MATMUL_MODEL, "test", EmitKind::OnnxBinary).expect("should compile");
    let bytes = hex_to_bytes(&hex);
    // Graph name "MatMulNet" is encoded as a string field in GraphProto (field 2).
    let name = b"MatMulNet";
    let found = bytes.windows(name.len()).any(|w| w == name);
    assert!(found, "expected graph name 'MatMulNet' in ONNX output");
}

// ---------------------------------------------------------------------------
// 7. opset_import has domain="" and version=17
// ---------------------------------------------------------------------------
#[test]
fn test_onnx_binary_opset() {
    let hex = compile(MATMUL_MODEL, "test", EmitKind::OnnxBinary).expect("should compile");
    let bytes = hex_to_bytes(&hex);
    // OperatorSetIdProto.version = 2, varint 17 = 0x11
    // field 2 tag (varint) = (2 << 3) | 0 = 0x10
    // So we expect the sequence [0x10, 0x11] somewhere in the output.
    let opset_version_bytes: &[u8] = &[0x10, 0x11];
    let found = bytes.windows(2).any(|w| w == opset_version_bytes);
    assert!(found, "expected opset version=17 encoded as [0x10, 0x11]");
}

// ---------------------------------------------------------------------------
// 8. Two-layer model produces > 20 bytes
// ---------------------------------------------------------------------------
#[test]
fn test_onnx_binary_roundtrip_size() {
    let hex = compile(TWO_LAYER_MODEL, "test", EmitKind::OnnxBinary).expect("should compile");
    let bytes = hex_to_bytes(&hex);
    assert!(
        bytes.len() > 20,
        "expected > 20 bytes for a 2-node graph, got {}",
        bytes.len()
    );
}

// ---------------------------------------------------------------------------
// 9. ONNX's checker accepts the models, and ONNX Runtime computes what numpy
//    computes from the same weights (needs Python with onnx, onnxruntime)
// ---------------------------------------------------------------------------
#[test]
fn test_onnx_binary_runs_in_onnxruntime() {
    let py = if cfg!(windows) { "python" } else { "python3" };
    let probe = std::process::Command::new(py)
        .args(["-c", "import onnx, onnxruntime, numpy"])
        .output();
    if !probe.map(|o| o.status.success()).unwrap_or(false) {
        eprintln!("skipped: no Python with onnx, onnxruntime and numpy");
        return;
    }
    let dir = std::env::temp_dir().join(format!("iris_onnx_rt_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mlp = r#"
model Net {
    input x: tensor<f32, [N, 4]>
    layer h Dense(units=3)
    layer a ReLU(h)
    layer g GELU(a)
    layer o Dense(units=2)
    layer p Softmax(o)
    output p
}
"#;
    let cnn = r#"
model Cnn {
    input img: tensor<f32, [1, 3, 8, 8]>
    layer c Conv2D(filters=4, kernel_size=3, padding=1)
    layer b BatchNorm(c)
    layer r ReLU(b)
    layer m MaxPool(r, stride=2)
    layer f Flatten(m)
    layer d Dense(f, units=5)
    output d
}
"#;
    let weights = r#"{"h.weight": [[0.1, -0.2, 0.3], [0.4, 0.5, -0.6], [-0.7, 0.8, 0.9], [1.0, -1.1, 1.2]],
 "h.bias": [0.01, -0.02, 0.03],
 "o.weight": [[0.5, -0.5], [0.25, 0.75], [-1.0, 1.0]],
 "o.bias": [0.1, -0.1]}"#;
    std::fs::write(dir.join("mlp.iris"), mlp).unwrap();
    std::fs::write(dir.join("cnn.iris"), cnn).unwrap();
    std::fs::write(dir.join("w.json"), weights).unwrap();
    let iris = env!("CARGO_BIN_EXE_iris");
    for (name, extra) in [
        (
            "mlp",
            vec![
                "--weights".to_string(),
                dir.join("w.json").display().to_string(),
            ],
        ),
        ("cnn", vec![]),
    ] {
        let out = std::process::Command::new(iris)
            .args(&extra)
            .args(["--emit", "onnx-binary", "-o"])
            .arg(dir.join(format!("{name}.onnx")))
            .arg(dir.join(format!("{name}.iris")))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The same model in text form (`--emit onnx`).
        let text = std::process::Command::new(iris)
            .args(&extra)
            .args(["--emit", "onnx", "-o"])
            .arg(dir.join(format!("{name}.onnxtxt")))
            .arg(dir.join(format!("{name}.iris")))
            .output()
            .unwrap();
        assert!(
            text.status.success(),
            "{}",
            String::from_utf8_lossy(&text.stderr)
        );
    }
    let check = r#"
import sys, json, numpy as np, onnx, onnxruntime as ort
from google.protobuf import text_format
from math import erf
d = sys.argv[1]
for name in ["mlp", "cnn"]:
    binary = onnx.load(f"{d}/{name}.onnx")
    onnx.checker.check_model(binary, full_check=True)
    # The text form parses back to the identical model, weights included.
    text = text_format.Parse(open(f"{d}/{name}.onnxtxt", encoding="utf-8").read(), onnx.ModelProto())
    assert text == binary, name
    onnx.checker.check_model(text, full_check=True)
w = json.load(open(f"{d}/w.json"))
x = np.array([[1.0, -2.0, 0.5, 3.0], [0.0, 0.1, -0.3, 0.2]], dtype=np.float32)
got = ort.InferenceSession(f"{d}/mlp.onnx").run(None, {"x": x})[0]
h = x @ np.array(w["h.weight"], np.float32) + np.array(w["h.bias"], np.float32)
a = np.maximum(h, 0)
g = 0.5 * a * (1 + np.vectorize(erf)(a / np.sqrt(2)))
o = g @ np.array(w["o.weight"], np.float32) + np.array(w["o.bias"], np.float32)
e = np.exp(o - o.max(axis=1, keepdims=True))
assert np.allclose(got, e / e.sum(axis=1, keepdims=True), atol=1e-5), got
img = np.random.default_rng(0).standard_normal((1, 3, 8, 8)).astype(np.float32)
assert ort.InferenceSession(f"{d}/cnn.onnx").run(None, {"img": img})[0].shape == (1, 5)
print("ok")
"#;
    std::fs::write(dir.join("check.py"), check).unwrap();
    let run = std::process::Command::new(py)
        .arg(dir.join("check.py"))
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

// ---------------------------------------------------------------------------
// 10. A weights file must give every parameter, and only those
// ---------------------------------------------------------------------------
#[test]
fn test_onnx_binary_weights_are_checked() {
    use iris::codegen::onnx_binary::{emit_onnx_binary_with, Weights};
    let tokens = iris::parser::lexer::Lexer::new(MATMUL_MODEL)
        .tokenize()
        .expect("lex");
    let ast = iris::parser::parse::Parser::new(&tokens)
        .parse_module()
        .expect("parse");
    let graph = iris::lower::lower_model(&ast.models[0]).expect("lower");
    let shapes = iris::pass::infer_shapes(&graph).expect("shapes");
    let missing = Weights::from_json(r#"{"y.bias": [0,0,0,0,0,0,0,0]}"#).unwrap();
    let err = emit_onnx_binary_with(&graph, &shapes, Some(&missing))
        .unwrap_err()
        .to_string();
    assert!(err.contains("no `y.weight`"), "{err}");
    let wrong =
        Weights::from_json(r#"{"y.weight": [[1,2],[3,4]], "y.bias": [0,0,0,0,0,0,0,0]}"#).unwrap();
    let err = emit_onnx_binary_with(&graph, &shapes, Some(&wrong))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("has shape [2, 2], but the layer needs [4, 8]"),
        "{err}"
    );
}
