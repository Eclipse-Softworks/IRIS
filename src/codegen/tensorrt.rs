//! TensorRT engines from IRIS models.
//!
//! A `model` compiles to ONNX (`onnx_binary`), and TensorRT builds a
//! serialized engine from it with `trtexec`, its builder tool -- the engine
//! `--emit tensorrt -o model.engine` writes, which TensorRT's runtime
//! deserializes and runs.
//!
//! `trtexec` is found through `TENSORRT_ROOT` (its `bin`), else on `PATH`.
//! A dimension that is a name (`[N, 4]`) gets an optimisation profile from
//! 1 to `IRIS_TRT_MAX_DIM` (64), optimised for 1; `IRIS_TRT_PRECISION`
//! (`fp32`, `fp16`, `int8`, `best`) chooses the precision, `fp32` by default.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::CodegenError;
use crate::ir::graph::{GraphIr, GraphNode};
use crate::ir::types::{Dim, IrType};

/// `trtexec`, from `TENSORRT_ROOT` or `PATH`.
fn find_trtexec() -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "trtexec.exe"
    } else {
        "trtexec"
    };
    if let Ok(root) = std::env::var("TENSORRT_ROOT") {
        let p = Path::new(&root).join("bin").join(exe);
        if p.is_file() {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(exe))
        .find(|p| p.is_file())
}

/// `--minShapes` / `--optShapes` / `--maxShapes` for the inputs with named
/// dimensions, or none when every dimension is a number.
fn shape_profile(graph: &GraphIr) -> Vec<String> {
    let max: u64 = std::env::var("IRIS_TRT_MAX_DIM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);
    let mut min = Vec::new();
    let mut opt = Vec::new();
    let mut top = Vec::new();
    let mut dynamic = false;
    for node in graph.inputs() {
        if let GraphNode::Input {
            name,
            ty: IrType::Tensor { shape, .. },
            ..
        } = node
        {
            let dims = |sym: u64| {
                shape
                    .0
                    .iter()
                    .map(|d| match d {
                        Dim::Literal(n) => n.to_string(),
                        Dim::Symbolic(_) => sym.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("x")
            };
            dynamic |= shape.0.iter().any(|d| matches!(d, Dim::Symbolic(_)));
            min.push(format!("{name}:{}", dims(1)));
            opt.push(format!("{name}:{}", dims(1)));
            top.push(format!("{name}:{}", dims(max)));
        }
    }
    if !dynamic {
        return Vec::new();
    }
    vec![
        format!("--minShapes={}", min.join(",")),
        format!("--optShapes={}", opt.join(",")),
        format!("--maxShapes={}", top.join(",")),
    ]
}

/// The `trtexec` arguments that build `engine` from the ONNX file `model`
/// for `graph`: the files, a shape profile for named dimensions, and the
/// precision `IRIS_TRT_PRECISION` asks for.
pub fn trtexec_args(
    graph: &GraphIr,
    model: &Path,
    engine: &Path,
) -> Result<Vec<String>, CodegenError> {
    let mut args = vec![
        format!("--onnx={}", model.display()),
        format!("--saveEngine={}", engine.display()),
    ];
    args.extend(shape_profile(graph));
    match std::env::var("IRIS_TRT_PRECISION").as_deref() {
        Ok("fp16") => args.push("--fp16".into()),
        Ok("int8") => args.push("--int8".into()),
        Ok("best") => args.push("--best".into()),
        Ok("fp32") | Err(_) => {}
        Ok(other) => {
            return Err(CodegenError::Unsupported {
                backend: "tensorrt".into(),
                detail: format!("IRIS_TRT_PRECISION is `{other}`: use fp32, fp16, int8 or best"),
            })
        }
    }
    Ok(args)
}

/// Build a serialized TensorRT engine for `graph` from its ONNX encoding.
pub fn build_engine(graph: &GraphIr, onnx: &[u8]) -> Result<Vec<u8>, CodegenError> {
    let trtexec = find_trtexec().ok_or_else(|| CodegenError::ToolchainMissing {
        detail: "TensorRT's trtexec, to build an engine: install TensorRT and put trtexec on PATH, or set TENSORRT_ROOT \
                 (`--emit onnx-binary` writes the ONNX model the engine is built from)"
            .into(),
    })?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("iris_trt_{}_{}", std::process::id(), stamp));
    std::fs::create_dir_all(&dir).map_err(CodegenError::Io)?;
    let model = dir.join("model.onnx");
    let engine = dir.join("model.engine");
    let result = (|| {
        std::fs::write(&model, onnx).map_err(CodegenError::Io)?;
        let out = Command::new(&trtexec)
            .args(trtexec_args(graph, &model, &engine)?)
            .output()
            .map_err(|e| CodegenError::ToolchainMissing {
                detail: format!("trtexec could not start ({e})"),
            })?;
        if !out.status.success() {
            let log = String::from_utf8_lossy(&out.stdout);
            let errors: Vec<&str> = log
                .lines()
                .filter(|l| l.contains("ERROR") || l.contains("FAILED"))
                .take(20)
                .collect();
            return Err(CodegenError::Unsupported {
                backend: "tensorrt".into(),
                detail: format!(
                    "trtexec could not build the engine:\n{}",
                    if errors.is_empty() {
                        log.trim_end().to_string()
                    } else {
                        errors.join("\n")
                    }
                ),
            });
        }
        std::fs::read(&engine).map_err(CodegenError::Io)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}
