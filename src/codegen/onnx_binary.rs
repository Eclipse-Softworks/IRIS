//! ONNX: a `model` as an ONNX `ModelProto` that ONNX tools load and run --
//! ONNX Runtime, TensorRT (`--emit tensorrt`), the ONNX checker -- in the
//! binary wire format (`--emit onnx-binary`) or protobuf's text format
//! (`--emit onnx`), the same model either way.
//!
//! Each DSL layer becomes the ONNX operators it means, with its parameters
//! as initializers:
//!
//! | Layer                          | ONNX                                     | Parameters (`--weights` keys)         |
//! |--------------------------------|------------------------------------------|---------------------------------------|
//! | `Dense` / `Linear(units)`      | `MatMul` + `Add`                         | `.weight` [in, units], `.bias` [units] |
//! | `Conv2D(filters, kernel_size, stride, padding)` | `Conv`                  | `.weight` [filters, c, k, k], `.bias` [filters] |
//! | `BatchNorm`                    | `BatchNormalization` (channel axis 1)    | `.scale` `.bias` `.mean` `.var` [c]   |
//! | `LayerNorm`                    | `LayerNormalization` (last axis)         | `.scale` `.bias` [d]                  |
//! | `Embedding(vocab, embed_dim)`  | `Gather`                                 | `.weight` [vocab, embed_dim]          |
//! | `ReLU` `Sigmoid` `Tanh`        | `Relu` `Sigmoid` `Tanh`                  |                                       |
//! | `GELU`                         | `0.5 x (1 + erf(x / sqrt 2))` (opset 17) |                                       |
//! | `Softmax`                      | `Softmax` (last axis)                    |                                       |
//! | `MaxPool` / `AvgPool(stride)`  | `MaxPool` / `AveragePool`, window = stride |                                     |
//! | `GlobalAveragePool` `GlobalMaxPool` `Flatten` `Add` `Concat(axis)` | the same |                         |
//! | `Dropout`                      | `Identity` (inference)                   |                                       |
//!
//! Parameters come from the weights file (`--weights FILE`: a JSON object of
//! `"layer.param": nested lists`), which must give every one; without one,
//! the network is as freshly constructed: Glorot-uniform weights from a seed
//! per layer name, zero biases, unit scales and variances.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::CodegenError;
use crate::ir::graph::{GraphIr, GraphNode, LayerParam, NodeId, ParamValue};
use crate::ir::types::{DType, Dim, IrType};
use crate::proto::{encode_message_field, encode_string_field, encode_varint, encode_varint_field};

const OPSET: u64 = 17;
/// The IR version that goes with opset 17.
const IR_VERSION: u64 = 8;

// ---------------------------------------------------------------------------
// Weights
// ---------------------------------------------------------------------------

static WEIGHTS: Mutex<Option<String>> = Mutex::new(None);

/// `--weights FILE`: the JSON text every ONNX/TensorRT build of this process
/// takes its parameters from.
pub fn set_weights(json: Option<String>) {
    *WEIGHTS.lock().unwrap() = json;
}

/// Trained parameters by name (`layer.weight`), flattened row-major.
#[derive(Debug, Default, Clone)]
pub struct Weights {
    tensors: HashMap<String, (Vec<usize>, Vec<f32>)>,
}

impl Weights {
    pub fn from_json(text: &str) -> Result<Self, CodegenError> {
        let bad = |detail: String| CodegenError::Unsupported {
            backend: "onnx".into(),
            detail,
        };
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| bad(format!("the weights file is not JSON: {e}")))?;
        let obj = value.as_object().ok_or_else(|| {
            bad("the weights file must be a JSON object of name: nested lists".into())
        })?;
        let mut tensors = HashMap::new();
        for (name, v) in obj {
            let mut shape = Vec::new();
            let mut data = Vec::new();
            flatten(v, 0, &mut shape, &mut data)
                .map_err(|e| bad(format!("weights `{name}`: {e}")))?;
            tensors.insert(name.clone(), (shape, data));
        }
        Ok(Self { tensors })
    }
}

fn flatten(
    v: &serde_json::Value,
    depth: usize,
    shape: &mut Vec<usize>,
    out: &mut Vec<f32>,
) -> Result<(), String> {
    match v {
        serde_json::Value::Array(items) => {
            if depth == shape.len() {
                shape.push(items.len());
            } else if shape[depth] != items.len() {
                return Err("rows of different lengths".into());
            }
            for item in items {
                flatten(item, depth + 1, shape, out)?;
            }
            Ok(())
        }
        serde_json::Value::Number(n) => {
            if depth != shape.len() {
                return Err("rows of different depths".into());
            }
            out.push(n.as_f64().ok_or("not a number")? as f32);
            Ok(())
        }
        _ => Err("expected numbers in nested lists".into()),
    }
}

/// Where parameters come from: a weights file, or fresh initialisation.
struct Params<'w> {
    file: Option<&'w Weights>,
    used: Vec<String>,
}

impl Params<'_> {
    /// The parameter `name` of shape `shape`; `init` makes a fresh one.
    fn get(&mut self, name: &str, shape: &[usize], init: Init) -> Result<Vec<f32>, CodegenError> {
        let n: usize = shape.iter().product();
        let Some(file) = self.file else {
            return Ok(init.make(name, shape, n));
        };
        self.used.push(name.to_owned());
        let (got_shape, data) =
            file.tensors
                .get(name)
                .ok_or_else(|| CodegenError::Unsupported {
                    backend: "onnx".into(),
                    detail: format!("the weights file has no `{name}` (shape {shape:?})"),
                })?;
        if got_shape != shape {
            return Err(CodegenError::Unsupported {
                backend: "onnx".into(),
                detail: format!(
                    "weights `{name}` has shape {got_shape:?}, but the layer needs {shape:?}"
                ),
            });
        }
        Ok(data.clone())
    }
}

/// How a fresh parameter starts.
#[derive(Clone, Copy)]
enum Init {
    /// Uniform in +-sqrt(6 / (fan_in + fan_out)).
    Glorot {
        fan_in: usize,
        fan_out: usize,
    },
    Const(f32),
}

impl Init {
    fn make(self, name: &str, _shape: &[usize], n: usize) -> Vec<f32> {
        match self {
            Init::Const(c) => vec![c; n],
            Init::Glorot { fan_in, fan_out } => {
                let limit = (6.0 / (fan_in + fan_out).max(1) as f64).sqrt();
                // SplitMix64 from the parameter's name: the same network every build.
                let mut state = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                    (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
                });
                (0..n)
                    .map(|_| {
                        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                        let mut z = state;
                        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                        z ^= z >> 31;
                        let unit = (z >> 11) as f64 / (1u64 << 53) as f64;
                        ((unit * 2.0 - 1.0) * limit) as f32
                    })
                    .collect()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The model as protobuf messages (onnx.proto field numbers), written in the
// binary wire format or in the text format
// ---------------------------------------------------------------------------

/// A field's value.
enum Field {
    /// A varint field (int64, int32, uint64).
    Int(i64),
    /// An enum field: its number, and its name for the text format.
    Enum(i64, &'static str),
    /// A fixed32 float.
    Float(f32),
    Str(String),
    Bytes(Vec<u8>),
    Msg(Msg),
}

/// A protobuf message: its fields, by name and number, in order.
#[derive(Default)]
struct Msg(Vec<(&'static str, u32, Field)>);

impl Msg {
    fn int(&mut self, name: &'static str, num: u32, v: i64) -> &mut Self {
        self.0.push((name, num, Field::Int(v)));
        self
    }

    fn enumeration(
        &mut self,
        name: &'static str,
        num: u32,
        v: i64,
        label: &'static str,
    ) -> &mut Self {
        self.0.push((name, num, Field::Enum(v, label)));
        self
    }

    fn float(&mut self, name: &'static str, num: u32, v: f32) -> &mut Self {
        self.0.push((name, num, Field::Float(v)));
        self
    }

    fn string(&mut self, name: &'static str, num: u32, v: &str) -> &mut Self {
        self.0.push((name, num, Field::Str(v.to_owned())));
        self
    }

    fn bytes(&mut self, name: &'static str, num: u32, v: Vec<u8>) -> &mut Self {
        self.0.push((name, num, Field::Bytes(v)));
        self
    }

    fn message(&mut self, name: &'static str, num: u32, m: Msg) -> &mut Self {
        self.0.push((name, num, Field::Msg(m)));
        self
    }

    /// The binary wire format.
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (_, num, field) in &self.0 {
            match field {
                Field::Int(v) | Field::Enum(v, _) => {
                    out.extend(encode_varint_field(*num, *v as u64))
                }
                Field::Float(v) => {
                    out.extend(encode_varint(((*num as u64) << 3) | 5));
                    out.extend_from_slice(&v.to_bits().to_le_bytes());
                }
                Field::Str(s) => out.extend(encode_string_field(*num, s)),
                Field::Bytes(b) => out.extend(encode_message_field(*num, b)),
                Field::Msg(m) => out.extend(encode_message_field(*num, &m.encode())),
            }
        }
        out
    }

    /// The text format: `name: value` and `name { ... }`, one field a line.
    fn text(&self, depth: usize, out: &mut String) {
        let pad = "  ".repeat(depth);
        for (name, _, field) in &self.0 {
            let line = match field {
                Field::Int(v) => format!("{pad}{name}: {v}\n"),
                Field::Enum(_, label) => format!("{pad}{name}: {label}\n"),
                Field::Float(v) if v.is_nan() => format!("{pad}{name}: nan\n"),
                Field::Float(v) => format!("{pad}{name}: {v:?}\n"),
                Field::Str(s) => format!("{pad}{name}: \"{}\"\n", escape(s.as_bytes())),
                Field::Bytes(b) => format!("{pad}{name}: \"{}\"\n", escape(b)),
                Field::Msg(m) => {
                    out.push_str(&format!("{pad}{name} {{\n"));
                    m.text(depth + 1, out);
                    format!("{pad}}}\n")
                }
            };
            out.push_str(&line);
        }
    }
}

/// Bytes as a text-format string literal's contents (C escapes, octal for
/// anything not printable ASCII).
fn escape(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            b'\n' => s.push_str("\\n"),
            0x20..=0x7e => s.push(b as char),
            _ => s.push_str(&format!("\\{b:03o}")),
        }
    }
    s
}

fn dtype_to_onnx_elem(dtype: DType) -> i64 {
    match dtype {
        DType::F32 => 1,
        DType::U8 => 2,
        DType::I8 => 3,
        DType::I32 => 6,
        DType::I64 | DType::USize => 7,
        DType::Bool => 9,
        DType::F64 => 11,
        DType::U32 => 12,
        DType::U64 => 13,
    }
}

enum Attr {
    Int(i64),
    Float(f32),
    Ints(Vec<i64>),
}

fn attribute(name: &str, value: &Attr) -> Msg {
    // AttributeProto: name 1, f 2, i 3, ints 8, type 20 (FLOAT 1, INT 2, INTS 7).
    let mut m = Msg::default();
    m.string("name", 1, name);
    match value {
        Attr::Float(v) => {
            m.float("f", 2, *v).enumeration("type", 20, 1, "FLOAT");
        }
        Attr::Int(n) => {
            m.int("i", 3, *n).enumeration("type", 20, 2, "INT");
        }
        Attr::Ints(ns) => {
            for n in ns {
                m.int("ints", 8, *n);
            }
            m.enumeration("type", 20, 7, "INTS");
        }
    }
    m
}

fn node(
    op: &str,
    name: &str,
    inputs: &[String],
    outputs: &[String],
    attrs: &[(&str, Attr)],
) -> Msg {
    // NodeProto: input 1, output 2, name 3, op_type 4, attribute 5.
    let mut m = Msg::default();
    for i in inputs {
        m.string("input", 1, i);
    }
    for o in outputs {
        m.string("output", 2, o);
    }
    m.string("name", 3, name).string("op_type", 4, op);
    for (k, v) in attrs {
        m.message("attribute", 5, attribute(k, v));
    }
    m
}

fn initializer(name: &str, shape: &[usize], data: &[f32]) -> Msg {
    // TensorProto: dims 1, data_type 2, name 8, raw_data 9 (little-endian).
    let mut m = Msg::default();
    for d in shape {
        m.int("dims", 1, *d as i64);
    }
    m.int("data_type", 2, 1).string("name", 8, name);
    m.bytes(
        "raw_data",
        9,
        data.iter().flat_map(|v| v.to_le_bytes()).collect(),
    );
    m
}

fn value_info(name: &str, ty: &IrType) -> Msg {
    // ValueInfoProto: name 1, type 2; TypeProto: tensor_type 1;
    // Tensor: elem_type 1, shape 2; TensorShapeProto: dim 1;
    // Dimension: dim_value 1, dim_param 2.
    let mut tensor = Msg::default();
    if let IrType::Tensor { dtype, shape } = ty {
        tensor.int("elem_type", 1, dtype_to_onnx_elem(*dtype));
        let mut dims = Msg::default();
        for d in &shape.0 {
            let mut dim = Msg::default();
            match d {
                Dim::Literal(n) => dim.int("dim_value", 1, *n as i64),
                Dim::Symbolic(s) => dim.string("dim_param", 2, s),
            };
            dims.message("dim", 1, dim);
        }
        tensor.message("shape", 2, dims);
    } else if let IrType::Scalar(d) = ty {
        tensor.int("elem_type", 1, dtype_to_onnx_elem(*d));
    }
    let mut type_proto = Msg::default();
    type_proto.message("tensor_type", 1, tensor);
    let mut m = Msg::default();
    m.string("name", 1, name).message("type", 2, type_proto);
    m
}

// ---------------------------------------------------------------------------
// Layers to operators
// ---------------------------------------------------------------------------

fn int_param(params: &[LayerParam], keys: &[&str]) -> Option<i64> {
    params
        .iter()
        .find(|p| keys.contains(&p.key.as_str()))
        .and_then(|p| match p.value {
            ParamValue::Int(n) => Some(n),
            _ => None,
        })
}

fn literal_dims(ty: &IrType) -> Vec<Option<usize>> {
    match ty {
        IrType::Tensor { shape, .. } => shape
            .0
            .iter()
            .map(|d| match d {
                Dim::Literal(n) => Some(*n as usize),
                Dim::Symbolic(_) => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

struct Builder<'w> {
    nodes: Vec<Msg>,
    inits: Vec<Msg>,
    params: Params<'w>,
}

impl Builder<'_> {
    fn node(
        &mut self,
        op: &str,
        name: &str,
        inputs: &[String],
        outputs: &[String],
        attrs: &[(&str, Attr)],
    ) {
        self.nodes.push(node(op, name, inputs, outputs, attrs));
    }

    fn param(&mut self, name: &str, shape: &[usize], init: Init) -> Result<String, CodegenError> {
        let data = self.params.get(name, shape, init)?;
        self.inits.push(initializer(name, shape, &data));
        Ok(name.to_owned())
    }

    fn constant(&mut self, name: &str, value: f32) -> String {
        self.inits.push(initializer(name, &[], &[value]));
        name.to_owned()
    }
}

fn layer_error(name: &str, op: &str, detail: &str) -> CodegenError {
    CodegenError::Unsupported {
        backend: "onnx".into(),
        detail: format!("layer `{name}` ({op}): {detail}"),
    }
}

/// The operators for one layer, producing the tensor named `name`.
fn lower_layer(
    b: &mut Builder,
    name: &str,
    op: &str,
    params: &[LayerParam],
    inputs: &[String],
    input_tys: &[IrType],
) -> Result<(), CodegenError> {
    let out = vec![name.to_owned()];
    let x = inputs
        .first()
        .cloned()
        .ok_or_else(|| layer_error(name, op, "no input"))?;
    let in_dims = input_tys.first().map(literal_dims).unwrap_or_default();
    match op {
        "Dense" | "Linear" => {
            let units = int_param(params, &["units", "out_features"])
                .ok_or_else(|| layer_error(name, op, "needs `units`"))?
                as usize;
            let fan_in = in_dims.last().copied().flatten().ok_or_else(|| {
                layer_error(name, op, "its input's last dimension must be a number")
            })?;
            let w = b.param(
                &format!("{name}.weight"),
                &[fan_in, units],
                Init::Glorot {
                    fan_in,
                    fan_out: units,
                },
            )?;
            let bias = b.param(&format!("{name}.bias"), &[units], Init::Const(0.0))?;
            let mm = format!("{name}.matmul");
            b.node("MatMul", &mm, &[x, w], std::slice::from_ref(&mm), &[]);
            b.node("Add", &format!("{name}.add"), &[mm, bias], &out, &[]);
        }
        "Conv2D" => {
            let c = in_dims.get(1).copied().flatten().ok_or_else(|| {
                layer_error(name, op, "needs an NCHW input with a numeric channel count")
            })?;
            let k = int_param(params, &["kernel_size"]).unwrap_or(3) as usize;
            let s = int_param(params, &["stride"]).unwrap_or(1).max(1);
            let p = int_param(params, &["padding"]).unwrap_or(0);
            let filters = int_param(params, &["filters"]).map_or(c, |f| f as usize);
            let w = b.param(
                &format!("{name}.weight"),
                &[filters, c, k, k],
                Init::Glorot {
                    fan_in: c * k * k,
                    fan_out: filters * k * k,
                },
            )?;
            let bias = b.param(&format!("{name}.bias"), &[filters], Init::Const(0.0))?;
            b.node(
                "Conv",
                name,
                &[x, w, bias],
                &out,
                &[
                    ("kernel_shape", Attr::Ints(vec![k as i64, k as i64])),
                    ("strides", Attr::Ints(vec![s, s])),
                    ("pads", Attr::Ints(vec![p, p, p, p])),
                ],
            );
        }
        "BatchNorm" => {
            let c = in_dims.get(1).copied().flatten().ok_or_else(|| {
                layer_error(name, op, "needs a numeric channel dimension (axis 1)")
            })?;
            let scale = b.param(&format!("{name}.scale"), &[c], Init::Const(1.0))?;
            let bias = b.param(&format!("{name}.bias"), &[c], Init::Const(0.0))?;
            let mean = b.param(&format!("{name}.mean"), &[c], Init::Const(0.0))?;
            let var = b.param(&format!("{name}.var"), &[c], Init::Const(1.0))?;
            b.node(
                "BatchNormalization",
                name,
                &[x, scale, bias, mean, var],
                &out,
                &[("epsilon", Attr::Float(1e-5))],
            );
        }
        "LayerNorm" => {
            let d = in_dims.last().copied().flatten().ok_or_else(|| {
                layer_error(name, op, "its input's last dimension must be a number")
            })?;
            let scale = b.param(&format!("{name}.scale"), &[d], Init::Const(1.0))?;
            let bias = b.param(&format!("{name}.bias"), &[d], Init::Const(0.0))?;
            b.node(
                "LayerNormalization",
                name,
                &[x, scale, bias],
                &out,
                &[("axis", Attr::Int(-1)), ("epsilon", Attr::Float(1e-5))],
            );
        }
        "Embedding" => {
            let vocab = int_param(params, &["vocab", "vocab_size", "num_embeddings"])
                .ok_or_else(|| layer_error(name, op, "needs `vocab` (the number of embeddings)"))?
                as usize;
            let dim = int_param(params, &["embed_dim"]).unwrap_or(64) as usize;
            let table = b.param(
                &format!("{name}.weight"),
                &[vocab, dim],
                Init::Glorot {
                    fan_in: vocab,
                    fan_out: dim,
                },
            )?;
            b.node("Gather", name, &[table, x], &out, &[("axis", Attr::Int(0))]);
        }
        "ReLU" => b.node("Relu", name, &[x], &out, &[]),
        "Sigmoid" => b.node("Sigmoid", name, &[x], &out, &[]),
        "Tanh" => b.node("Tanh", name, &[x], &out, &[]),
        "Softmax" => b.node("Softmax", name, &[x], &out, &[("axis", Attr::Int(-1))]),
        "GELU" => {
            let sqrt2 = b.constant(&format!("{name}.sqrt2"), std::f32::consts::SQRT_2);
            let one = b.constant(&format!("{name}.one"), 1.0);
            let half = b.constant(&format!("{name}.half"), 0.5);
            let t: Vec<String> = (0..4).map(|i| format!("{name}.t{i}")).collect();
            b.node("Div", &t[0], &[x.clone(), sqrt2], &[t[0].clone()], &[]);
            b.node("Erf", &t[1], &[t[0].clone()], &[t[1].clone()], &[]);
            b.node("Add", &t[2], &[t[1].clone(), one], &[t[2].clone()], &[]);
            b.node("Mul", &t[3], &[x, t[2].clone()], &[t[3].clone()], &[]);
            b.node("Mul", name, &[t[3].clone(), half], &out, &[]);
        }
        "MaxPool" | "AvgPool" => {
            let s = int_param(params, &["stride"]).unwrap_or(2).max(1);
            let onnx = if op == "MaxPool" {
                "MaxPool"
            } else {
                "AveragePool"
            };
            b.node(
                onnx,
                name,
                &[x],
                &out,
                &[
                    ("kernel_shape", Attr::Ints(vec![s, s])),
                    ("strides", Attr::Ints(vec![s, s])),
                ],
            );
        }
        "GlobalAveragePool" | "GlobalMaxPool" => b.node(op, name, &[x], &out, &[]),
        "Flatten" => b.node("Flatten", name, &[x], &out, &[("axis", Attr::Int(1))]),
        "Dropout" => b.node("Identity", name, &[x], &out, &[]),
        "Add" => {
            if inputs.len() != 2 {
                return Err(layer_error(name, op, "takes two inputs"));
            }
            b.node("Add", name, inputs, &out, &[]);
        }
        "Concat" => {
            let axis = int_param(params, &["axis"]).unwrap_or(1);
            b.node("Concat", name, inputs, &out, &[("axis", Attr::Int(axis))]);
        }
        other => return Err(layer_error(name, other, "has no ONNX operator")),
    }
    Ok(())
}

/// The parameters `--weights` gave this process, if any.
fn configured_weights() -> Result<Option<Weights>, CodegenError> {
    let text = WEIGHTS.lock().unwrap().clone();
    text.as_deref().map(Weights::from_json).transpose()
}

/// Encode `graph` (with `shapes` from `infer_shapes`) as an ONNX model,
/// parameters from `--weights` or freshly initialised.
pub fn emit_onnx_binary(
    graph: &GraphIr,
    shapes: &HashMap<NodeId, IrType>,
) -> Result<Vec<u8>, CodegenError> {
    emit_onnx_binary_with(graph, shapes, configured_weights()?.as_ref())
}

/// `emit_onnx_binary` with the parameters from `weights` (all of them), or
/// freshly initialised when `None`.
pub fn emit_onnx_binary_with(
    graph: &GraphIr,
    shapes: &HashMap<NodeId, IrType>,
    weights: Option<&Weights>,
) -> Result<Vec<u8>, CodegenError> {
    Ok(model(graph, shapes, weights)?.encode())
}

/// The same model as `emit_onnx_binary`, in protobuf's text format: what
/// `google.protobuf.text_format.Parse(text, onnx.ModelProto())` reads back
/// as the binary model, parameters included.
pub fn emit_onnx_text(
    graph: &GraphIr,
    shapes: &HashMap<NodeId, IrType>,
) -> Result<String, CodegenError> {
    emit_onnx_text_with(graph, shapes, configured_weights()?.as_ref())
}

/// `emit_onnx_text` with the parameters from `weights`, or freshly
/// initialised when `None`.
pub fn emit_onnx_text_with(
    graph: &GraphIr,
    shapes: &HashMap<NodeId, IrType>,
    weights: Option<&Weights>,
) -> Result<String, CodegenError> {
    let mut out = format!(
        "# ONNX ModelProto `{}` (opset {OPSET}), protobuf text format.\n\
         # Load: google.protobuf.text_format.Parse(text, onnx.ModelProto())\n",
        graph.name
    );
    model(graph, shapes, weights)?.text(0, &mut out);
    Ok(out)
}

/// The `ModelProto` for `graph`.
fn model(
    graph: &GraphIr,
    shapes: &HashMap<NodeId, IrType>,
    weights: Option<&Weights>,
) -> Result<Msg, CodegenError> {
    let names: HashMap<NodeId, &str> = graph.nodes().iter().map(|n| (n.id(), n.name())).collect();
    let mut b = Builder {
        nodes: Vec::new(),
        inits: Vec::new(),
        params: Params {
            file: weights,
            used: Vec::new(),
        },
    };
    for node in graph.layers() {
        if let GraphNode::Layer {
            name,
            op,
            params,
            inputs,
            ..
        } = node
        {
            let input_names: Vec<String> = inputs
                .iter()
                .filter_map(|i| names.get(i))
                .map(|s| s.to_string())
                .collect();
            let input_tys: Vec<IrType> = inputs
                .iter()
                .filter_map(|i| shapes.get(i).cloned())
                .collect();
            lower_layer(&mut b, name, op, params, &input_names, &input_tys)?;
        }
    }
    if let Some(w) = weights {
        let mut unused: Vec<&String> = w
            .tensors
            .keys()
            .filter(|k| !b.params.used.contains(k))
            .collect();
        unused.sort();
        if !unused.is_empty() {
            return Err(CodegenError::Unsupported {
                backend: "onnx".into(),
                detail: format!(
                    "the weights file has parameters no layer of `{}` takes: {}",
                    graph.name,
                    unused
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
    }

    // GraphProto: node 1, name 2, initializer 5, input 11, output 12.
    let mut g = Msg::default();
    for n in b.nodes {
        g.message("node", 1, n);
    }
    g.string("name", 2, &graph.name);
    for init in b.inits {
        g.message("initializer", 5, init);
    }
    for node in graph.inputs() {
        if let GraphNode::Input { name, ty, .. } = node {
            g.message("input", 11, value_info(name, ty));
        }
    }
    for node in graph.outputs() {
        if let GraphNode::Output { name, from, .. } = node {
            // The output is the tensor its source layer produces.
            let source = names.get(from).copied().unwrap_or(name.as_str());
            let mut ty = shapes
                .get(from)
                .cloned()
                .ok_or_else(|| CodegenError::Unsupported {
                    backend: "onnx".into(),
                    detail: format!(
                        "internal compiler error: output `{name}` of `{}` has no inferred shape",
                        graph.name
                    ),
                })?;
            // An embedding yields its table's floats, whatever indexes it.
            let from_embedding = graph.layers().any(
                |l| matches!(l, GraphNode::Layer { id, op, .. } if id == from && op == "Embedding"),
            );
            if let (true, IrType::Tensor { dtype, .. }) = (from_embedding, &mut ty) {
                *dtype = DType::F32;
            }
            g.message("output", 12, value_info(source, &ty));
        }
    }

    // ModelProto: ir_version 1, producer_name 2, graph 7, opset_import 8.
    let mut opset = Msg::default();
    opset
        .string("domain", 1, "")
        .int("version", 2, OPSET as i64);
    let mut m = Msg::default();
    m.int("ir_version", 1, IR_VERSION as i64)
        .string("producer_name", 2, "iris")
        .message("graph", 7, g)
        .message("opset_import", 8, opset);
    Ok(m)
}
