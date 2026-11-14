//! CUDA for IRIS: GPU kernels from `par for` bodies and `@kernel` functions.
//!
//! `device_module` turns the program's GPU code into an NVPTX LLVM module:
//!
//! - **Device functions**: every function that works on numbers and tensors
//!   -- arithmetic with IRIS's checks, tensor indexing with bounds checks,
//!   whole-tensor operations, calls to other device functions, branches,
//!   `panic`/`assert`. A function that uses anything else (strings, lists,
//!   I/O, ...) is host code and is left out (`host_only` says why).
//! - **`par for` kernels**: `{body}_kernel(i64 start, i64 end, ptr env)`
//!   runs iteration `start + global thread id`; `env` holds the captures in
//!   8-byte slots (a tensor's slot is its device header).
//! - **`@kernel` functions**: `{f}_kernel(i64 n, ptr a0, ..., ptr out)` maps
//!   `f` over `n` elements: a number parameter is an array (element `i` for
//!   thread `i`), a tensor parameter is passed whole, and `out[i]` receives
//!   the result.
//!
//! A tensor on the device is a header `{ float* data, int64* shape, int32
//! ndim, int64 numel }` -- the runtime's `IrisTensor` layout. Whole-tensor
//! operations allocate their result with device `malloc`.
//!
//! A failing operation (index out of range, overflow, division by zero,
//! `panic`, out of device memory) records the first error in
//! `__iris_dev_error` -- code, operands, source position -- and the thread
//! returns. `iris_cuda_launch` (the runtime) reads it after the kernel and
//! stops the program with the same report the CPU gives.
//!
//! `--gpu cuda` (`offload`) runs every `par for` whose body is device code on
//! the GPU: the PTX, compiled at build time with clang's NVPTX backend (and
//! libdevice for transcendental functions, from the CUDA toolkit), is
//! embedded in the program, and the CUDA driver runs it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write;
use std::path::PathBuf;
use std::process::Command;

use super::build::find_clang;
use crate::error::CodegenError;
use crate::ir::block::BlockId;
use crate::ir::function::IrFunction;
use crate::ir::instr::{BinOp, IrInstr, ScalarUnaryOp, TensorOp};
use crate::ir::module::IrModule;
use crate::ir::types::{DType, Dim, IrType};
use crate::ir::value::ValueId;

const NVPTX_DATALAYOUT: &str =
    "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v32:32:32-v64:64:64-v128:128:128-n16:32:64";
const NVPTX_TRIPLE: &str = "nvptx64-nvidia-cuda";

/// Error codes in `__iris_dev_error.code`, which the runtime turns into
/// reports.
pub const ERR_INDEX: i32 = 1; // a = index, b = length, dim = dimension (-1: flat)
pub const ERR_RANK: i32 = 2; // a = indices given, b = rank
pub const ERR_OVERFLOW: i32 = 3; // dim = operation (0 add, 1 sub, 2 mul, 3 div), a, b
pub const ERR_DIV_ZERO: i32 = 4;
pub const ERR_PANIC: i32 = 5; // msg = index into the panic message table
pub const ERR_NO_MEMORY: i32 = 6; // a = bytes asked for
pub const ERR_SHAPE: i32 = 7; // a, b = the sizes that differ

/// What the device module is compiled for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceTarget {
    /// The GPU.
    Nvptx,
    /// The host CPU, kernels looping over their range: the same device code,
    /// runnable where there is no GPU, for testing it.
    HostTest,
}

/// A `par for` kernel: its body function, kernel name, and capture kinds
/// (`t` tensor, `l` i64, `w` i32, `c` i8, `d` f64, `f` f32, `b` bool).
#[derive(Debug, Clone)]
pub struct KernelInfo {
    pub body: String,
    pub kernel: String,
    pub captures: String,
}

#[derive(Debug, Clone)]
pub struct DeviceModule {
    pub ir: String,
    pub kernels: Vec<KernelInfo>,
    /// `panic`/`assert` messages, indexed by `__iris_dev_error.msg`.
    pub panics: Vec<String>,
    /// Calls libdevice (`__nv_*`), which compiling to PTX links in.
    pub libdevice: bool,
    /// Functions left out as host code, and why.
    pub host_only: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// `--emit cuda`: the program's device code as NVPTX LLVM IR.
pub fn emit_cuda(module: &IrModule) -> Result<String, CodegenError> {
    Ok(device_module(module, DeviceTarget::Nvptx)?.ir)
}

/// `--emit cuda-ptx`: the device code compiled to PTX.
pub fn emit_cuda_ptx(module: &IrModule) -> Result<String, CodegenError> {
    compile_ptx(&device_module(module, DeviceTarget::Nvptx)?)
}

fn err(detail: impl Into<String>) -> CodegenError {
    CodegenError::Unsupported {
        backend: "cuda".into(),
        detail: detail.into(),
    }
}

/// libdevice, from the CUDA toolkit (`CUDA_PATH`, `CUDA_HOME`, or the usual
/// install locations).
fn find_libdevice() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("IRIS_LIBDEVICE") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    for var in ["CUDA_PATH", "CUDA_HOME", "CUDA_ROOT"] {
        if let Ok(p) = std::env::var(var) {
            roots.push(PathBuf::from(p));
        }
    }
    roots.push(PathBuf::from("/usr/local/cuda"));
    roots.push(PathBuf::from("/opt/cuda"));
    for root in roots {
        let p = root.join("nvvm").join("libdevice").join("libdevice.10.bc");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Compile a device module to PTX with clang's NVPTX backend, for
/// `IRIS_CUDA_ARCH` (default `sm_52`), linking libdevice when it is called.
pub fn compile_ptx(device: &DeviceModule) -> Result<String, CodegenError> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("iris_cuda_{}_{}", std::process::id(), stamp));
    std::fs::create_dir_all(&dir).map_err(CodegenError::Io)?;
    let ll = dir.join("device.ll");
    let ptx = dir.join("device.ptx");
    std::fs::write(&ll, &device.ir).map_err(CodegenError::Io)?;
    let arch = std::env::var("IRIS_CUDA_ARCH").unwrap_or_else(|_| "sm_52".into());
    let clang = find_clang();
    let mut cmd = Command::new(&clang);
    cmd.args([
        "-target",
        NVPTX_TRIPLE,
        &format!("-march={arch}"),
        "-O3",
        "-S",
        "-x",
        "ir",
    ])
    .arg(&ll)
    .arg("-o")
    .arg(&ptx);
    if device.libdevice {
        let lib = find_libdevice().ok_or_else(|| CodegenError::ToolchainMissing {
            detail: "the CUDA toolkit's libdevice (nvvm/libdevice/libdevice.10.bc), for the GPU's sin, cos, exp, log and pow: \
                     install the CUDA toolkit, or set CUDA_PATH or IRIS_LIBDEVICE"
                .into(),
        })?;
        cmd.args(["-Xclang", "-mlink-builtin-bitcode", "-Xclang"])
            .arg(lib);
    }
    let out = cmd.output().map_err(|e| CodegenError::ToolchainMissing {
        detail: format!("clang ('{clang}') with the NVPTX target, to compile PTX ({e})"),
    });
    let result = match out {
        Err(e) => Err(e),
        Ok(o) if !o.status.success() => Err(err(format!(
            "clang could not compile the device code to PTX:\n{}",
            String::from_utf8_lossy(&o.stderr)
        ))),
        Ok(_) => std::fs::read_to_string(&ptx).map_err(CodegenError::Io),
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

// ---------------------------------------------------------------------------
// Device types
// ---------------------------------------------------------------------------

/// A value's device representation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dv {
    Bool,
    I8,
    I32,
    I64,
    F32,
    F64,
    Tensor,
    Unit,
}

impl Dv {
    fn ll(self) -> &'static str {
        match self {
            Dv::Bool => "i1",
            Dv::I8 => "i8",
            Dv::I32 => "i32",
            Dv::I64 => "i64",
            Dv::F32 => "float",
            Dv::F64 => "double",
            Dv::Tensor => "ptr",
            Dv::Unit => "void",
        }
    }
    fn zero(self) -> &'static str {
        match self {
            Dv::Bool => "false",
            Dv::F32 | Dv::F64 => "0.0",
            Dv::Tensor => "null",
            _ => "0",
        }
    }
    fn is_float(self) -> bool {
        matches!(self, Dv::F32 | Dv::F64)
    }
    fn is_int(self) -> bool {
        matches!(self, Dv::I8 | Dv::I32 | Dv::I64)
    }
    fn bits(self) -> u32 {
        match self {
            Dv::Bool => 1,
            Dv::I8 => 8,
            Dv::I32 | Dv::F32 => 32,
            _ => 64,
        }
    }
    /// The `env` slot kind of a capture.
    fn slot(self) -> char {
        match self {
            Dv::Tensor => 't',
            Dv::I64 => 'l',
            Dv::I32 => 'w',
            Dv::I8 => 'c',
            Dv::F64 => 'd',
            Dv::F32 => 'f',
            Dv::Bool | Dv::Unit => 'b',
        }
    }
}

/// The device representation of `ty`, and whether it is unsigned.
fn dv(ty: &IrType) -> Option<(Dv, bool)> {
    Some(match ty {
        IrType::Scalar(DType::Bool) => (Dv::Bool, false),
        IrType::Scalar(DType::I8) => (Dv::I8, false),
        IrType::Scalar(DType::U8) => (Dv::I8, true),
        IrType::Scalar(DType::I32) => (Dv::I32, false),
        IrType::Scalar(DType::U32) => (Dv::I32, true),
        IrType::Scalar(DType::I64) => (Dv::I64, false),
        IrType::Scalar(DType::U64 | DType::USize) => (Dv::I64, true),
        IrType::Scalar(DType::F32) => (Dv::F32, false),
        IrType::Scalar(DType::F64) => (Dv::F64, false),
        IrType::Tensor { .. } => (Dv::Tensor, false),
        IrType::Tuple(t) if t.is_empty() => (Dv::Unit, false),
        IrType::Struct { name, fields } if name == "unit" && fields.is_empty() => (Dv::Unit, false),
        _ => return None,
    })
}

fn float_lit(v: f64, d: Dv) -> String {
    let v = if d == Dv::F32 { v as f32 as f64 } else { v };
    format!("0x{:016X}", v.to_bits())
}

const TENSOR_UNARY: &[&str] = &[
    "neg", "relu", "sigmoid", "tanh", "exp", "log", "sqrt", "abs",
];
const REDUCE_OPS: &[&str] = &["sum", "max", "mean", "min", "prod"];

/// An einsum notation's subscripts: one or two inputs, and the output.
fn parse_einsum(notation: &str) -> Option<(Vec<Vec<char>>, Vec<char>)> {
    let (ins, out) = notation.split_once("->")?;
    let inputs: Vec<Vec<char>> = ins.split(',').map(|s| s.trim().chars().collect()).collect();
    let out: Vec<char> = out.trim().chars().collect();
    if inputs.is_empty()
        || inputs.len() > 2
        || inputs
            .iter()
            .any(|i| i.iter().any(|c| !c.is_ascii_alphabetic()))
    {
        return None;
    }
    if out.iter().any(|c| !inputs.iter().any(|i| i.contains(c))) {
        return None;
    }
    Some((inputs, out))
}

// ---------------------------------------------------------------------------
// Which functions are device code
// ---------------------------------------------------------------------------

fn instr_name(instr: &IrInstr) -> String {
    let what = format!("{instr:?}");
    what.split([' ', '{', '('])
        .next()
        .unwrap_or("instruction")
        .to_owned()
}

/// Why `func` is not device code on its own (ignoring callees), if it is not.
fn local_reason(func: &IrFunction) -> Option<String> {
    let ty_ok = |v: &ValueId| func.value_type(*v).and_then(dv).is_some();
    let entry = func.blocks().first()?;
    let used = used_values(func);
    for p in &entry.params {
        match func.value_type(p.id).and_then(dv) {
            // A capture the body never reads may be anything.
            None if !used.contains(&p.id) && func.name.starts_with("__par_body_") => {}
            Some((Dv::Unit, _)) | None => {
                return Some(format!(
                    "a parameter of type `{}`",
                    func.value_type(p.id)
                        .map(|t| t.to_string())
                        .unwrap_or_default()
                ))
            }
            _ => {}
        }
    }
    if dv(&func.return_ty).is_none() {
        return Some(format!("returns `{}`", func.return_ty));
    }
    // Strings only as panic messages.
    let panic_msgs: HashSet<ValueId> = func
        .blocks()
        .iter()
        .flat_map(|b| &b.instrs)
        .filter_map(|i| {
            if let IrInstr::Panic { msg, .. } = i {
                Some(*msg)
            } else {
                None
            }
        })
        .collect();
    for block in func.blocks() {
        // The entry block's params are the arguments, checked above.
        for p in block.params.iter().filter(|_| block.id != entry.id) {
            if func.value_type(p.id).and_then(dv).is_none() {
                return Some("a value of a host-only type".into());
            }
        }
        for instr in &block.instrs {
            let ok = match instr {
                IrInstr::ConstInt { .. }
                | IrInstr::ConstFloat { .. }
                | IrInstr::ConstBool { .. } => true,
                IrInstr::ConstStr { result, .. } => panic_msgs.contains(result),
                IrInstr::Panic { msg, .. } => func
                    .blocks()
                    .iter()
                    .flat_map(|b| &b.instrs)
                    .any(|i| matches!(i, IrInstr::ConstStr { result, .. } if result == msg)),
                IrInstr::BinOp { lhs, rhs, op, .. } => {
                    match (
                        func.value_type(*lhs).and_then(dv),
                        func.value_type(*rhs).and_then(dv),
                    ) {
                        (Some((Dv::Tensor, _)), Some((Dv::Tensor, _))) => {
                            matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
                        }
                        (Some((a, _)), Some((b, _))) => a == b && a != Dv::Tensor && a != Dv::Unit,
                        _ => false,
                    }
                }
                IrInstr::UnaryOp { operand, .. } => {
                    matches!(func.value_type(*operand).and_then(dv), Some((d, _)) if d != Dv::Tensor && d != Dv::Unit)
                }
                IrInstr::Cast { operand, to_ty, .. } => {
                    ty_ok(operand)
                        && matches!(dv(to_ty), Some((d, _)) if d != Dv::Tensor && d != Dv::Unit)
                }
                IrInstr::Load {
                    tensor,
                    indices,
                    result_ty,
                    ..
                } => matches!(func.value_type(*tensor).and_then(dv), Some((Dv::Tensor, _)))
                    && indices.iter().all(
                        |i| matches!(func.value_type(*i).and_then(dv), Some((d, _)) if d.is_int()),
                    )
                    && matches!(dv(result_ty), Some((d, _)) if d != Dv::Tensor && d != Dv::Unit),
                IrInstr::Store {
                    tensor,
                    indices,
                    value,
                } => matches!(func.value_type(*tensor).and_then(dv), Some((Dv::Tensor, _)))
                    && indices.iter().all(
                        |i| matches!(func.value_type(*i).and_then(dv), Some((d, _)) if d.is_int()),
                    )
                    && matches!(func.value_type(*value).and_then(dv), Some((d, _)) if d != Dv::Tensor && d != Dv::Unit),
                IrInstr::TensorOp {
                    op,
                    inputs,
                    result_ty,
                    ..
                } => {
                    let tensors = inputs
                        .iter()
                        .filter(|i| {
                            matches!(func.value_type(**i).and_then(dv), Some((Dv::Tensor, _)))
                        })
                        .count();
                    match op {
                        TensorOp::Unary { op } => TENSOR_UNARY.contains(&op.as_str()) && inputs.len() == 1 && tensors == 1,
                        TensorOp::Einsum { notation } => {
                            parse_einsum(notation).is_some_and(|(ins, _)| ins.len() == inputs.len()) && tensors == inputs.len()
                        }
                        TensorOp::Reduce { op, .. } => REDUCE_OPS.contains(&op.as_str()) && inputs.len() == 1 && tensors == 1,
                        TensorOp::Transpose { .. } => inputs.len() == 1 && tensors == 1,
                        TensorOp::Reshape => {
                            tensors == 1
                                && (inputs.len() > 1
                                    && inputs[1..].iter().all(|i| matches!(func.value_type(*i).and_then(dv), Some((d, _)) if d.is_int()))
                                    || matches!(result_ty, IrType::Tensor { shape, .. } if shape.0.iter().all(|d| matches!(d, Dim::Literal(_)))))
                        }
                    }
                }
                IrInstr::Call {
                    args, result_ty, ..
                } => args.iter().all(ty_ok) && result_ty.as_ref().map_or(true, |t| dv(t).is_some()),
                IrInstr::Br { .. }
                | IrInstr::CondBr { .. }
                | IrInstr::Return { .. }
                | IrInstr::Barrier => true,
                _ => false,
            };
            if !ok {
                return Some(format!("uses `{}`", instr_name(instr)));
            }
        }
    }
    None
}

/// Every value an instruction of `func` reads.
fn used_values(func: &IrFunction) -> HashSet<ValueId> {
    let mut used = HashSet::new();
    for instr in func.blocks().iter().flat_map(|b| &b.instrs) {
        used.extend(instr.operands());
        match instr {
            IrInstr::Br { args, .. } => used.extend(args.iter().copied()),
            IrInstr::CondBr {
                then_args,
                else_args,
                ..
            } => {
                used.extend(then_args.iter().copied());
                used.extend(else_args.iter().copied());
            }
            _ => {}
        }
    }
    used
}

/// The device functions, and why each other function is host code.
fn classify(module: &IrModule) -> (HashSet<String>, Vec<(String, String)>) {
    let mut reasons: BTreeMap<String, String> = BTreeMap::new();
    let mut eligible: HashSet<String> = HashSet::new();
    for f in module.functions() {
        match local_reason(f) {
            Some(r) => {
                reasons.insert(f.name.clone(), r);
            }
            None => {
                eligible.insert(f.name.clone());
            }
        }
    }
    // A function calling host code is host code.
    loop {
        let mut changed = false;
        for f in module.functions() {
            if !eligible.contains(&f.name) {
                continue;
            }
            let bad = f
                .blocks()
                .iter()
                .flat_map(|b| &b.instrs)
                .find_map(|i| match i {
                    IrInstr::Call { callee, .. } if !eligible.contains(callee) => {
                        Some(callee.clone())
                    }
                    _ => None,
                });
            if let Some(callee) = bad {
                eligible.remove(&f.name);
                reasons.insert(
                    f.name.clone(),
                    format!("calls `{callee}`, which is host code"),
                );
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    (eligible, reasons.into_iter().collect())
}

// ---------------------------------------------------------------------------
// Module emission
// ---------------------------------------------------------------------------

struct ModCtx<'m> {
    module: &'m IrModule,
    target: DeviceTarget,
    decls: BTreeSet<String>,
    helpers: BTreeMap<String, String>,
    panics: Vec<String>,
    libdevice: bool,
    einsums: HashMap<String, String>,
}

impl ModCtx<'_> {
    /// The error record, as a generic pointer.
    fn errp(&self) -> &'static str {
        match self.target {
            DeviceTarget::Nvptx => "addrspacecast (ptr addrspace(1) @__iris_dev_error to ptr)",
            DeviceTarget::HostTest => "@__iris_dev_error",
        }
    }

    fn declare(&mut self, line: impl Into<String>) {
        self.decls.insert(line.into());
    }

    /// An LLVM intrinsic over `ty`: `llvm.{name}.{sfx}`.
    fn intrinsic(&mut self, name: &str, ty: &str, arity: usize) -> String {
        let sfx = match ty {
            "float" => "f32",
            "double" => "f64",
            other => other,
        };
        let full = format!("llvm.{name}.{sfx}");
        self.declare(format!(
            "declare {ty} @{full}({})",
            vec![ty; arity].join(", ")
        ));
        full
    }

    /// A libdevice function, `__nv_{name}` (`f` suffix for `float`).
    fn libdevice(&mut self, name: &str, ty: &str, arity: usize) -> String {
        let full = if ty == "float" {
            format!("__nv_{name}f")
        } else {
            format!("__nv_{name}")
        };
        self.declare(format!(
            "declare {ty} @{full}({})",
            vec![ty; arity].join(", ")
        ));
        self.libdevice = true;
        full
    }

    fn panic_index(&mut self, msg: &str) -> usize {
        if let Some(i) = self.panics.iter().position(|m| m == msg) {
            return i;
        }
        self.panics.push(msg.to_owned());
        self.panics.len() - 1
    }

    fn helper(&mut self, name: &str, make: impl FnOnce(&mut Self) -> String) {
        if !self.helpers.contains_key(name) {
            self.helpers.insert(name.to_owned(), String::new());
            let text = make(self);
            self.helpers.insert(name.to_owned(), text);
        }
    }
}

/// The device module of `module`.
pub fn device_module(
    module: &IrModule,
    target: DeviceTarget,
) -> Result<DeviceModule, CodegenError> {
    let (eligible, host_only) = classify(module);
    let mut cx = ModCtx {
        module,
        target,
        decls: BTreeSet::new(),
        helpers: BTreeMap::new(),
        panics: Vec::new(),
        libdevice: false,
        einsums: HashMap::new(),
    };
    cx.declare("declare ptr @malloc(i64)");
    cx.declare("declare void @free(ptr)");
    if target == DeviceTarget::Nvptx {
        for sreg in [
            "tid.x", "tid.y", "tid.z", "ctaid.x", "ctaid.y", "ctaid.z", "ntid.x", "ntid.y",
            "ntid.z",
        ] {
            cx.declare(format!("declare i32 @llvm.nvvm.read.ptx.sreg.{sreg}()"));
        }
        cx.declare("declare void @llvm.nvvm.barrier0()");
    }
    helpers_base(&mut cx);

    // Functions, in module order.
    let mut functions = String::new();
    for f in module.functions() {
        if eligible.contains(&f.name) {
            functions.push_str(&emit_function(&mut cx, f)?);
        }
    }

    // `par for` kernels.
    let mut kernels = Vec::new();
    let mut kernel_text = String::new();
    let mut annotated: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for f in module.functions() {
        for instr in f.blocks().iter().flat_map(|b| &b.instrs) {
            let IrInstr::ParFor { body_fn, .. } = instr else {
                continue;
            };
            if !seen.insert(body_fn.clone()) || !eligible.contains(body_fn) {
                continue;
            }
            let body = module
                .function_by_name(body_fn)
                .ok_or_else(|| err(format!("no function `{body_fn}`")))?;
            let (text, info) = par_kernel(&mut cx, body)?;
            kernel_text.push_str(&text);
            annotated.push(info.kernel.clone());
            kernels.push(info);
        }
    }
    // `@kernel` map kernels.
    for f in module.functions() {
        if !f.attrs.iter().any(|a| a == "kernel") {
            continue;
        }
        if !eligible.contains(&f.name) {
            let why = host_only
                .iter()
                .find(|(n, _)| n == &f.name)
                .map(|(_, r)| r.clone())
                .unwrap_or_default();
            return Err(err(format!(
                "`@kernel` function `{}` is not device code: it {why}",
                f.name
            )));
        }
        let text = map_kernel(&mut cx, f)?;
        kernel_text.push_str(&text);
        annotated.push(format!("{}_kernel", f.name));
    }

    let mut out = String::new();
    writeln!(out, "; IRIS CUDA device code (NVPTX); `par for` kernels are launched by the runtime's iris_cuda_launch.")?;
    for (name, why) in &host_only {
        writeln!(out, "; host only: {name} ({why})")?;
    }
    match target {
        DeviceTarget::Nvptx => {
            writeln!(out, "target datalayout = \"{NVPTX_DATALAYOUT}\"")?;
            writeln!(out, "target triple = \"{NVPTX_TRIPLE}\"")?;
        }
        DeviceTarget::HostTest => {
            writeln!(
                out,
                "target triple = \"{}\"",
                super::llvm_ir::native_target_triple()
            )?;
        }
    }
    writeln!(out)?;
    writeln!(out, "%dt = type {{ ptr, ptr, i32, i64 }}")?;
    writeln!(out, "%err_t = type {{ i32, i32, i64, i64, i64, i64 }}")?;
    match target {
        DeviceTarget::Nvptx => writeln!(out, "@__iris_dev_error = addrspace(1) externally_initialized global %err_t zeroinitializer, align 8")?,
        DeviceTarget::HostTest => writeln!(out, "@__iris_dev_error = global %err_t zeroinitializer, align 8")?,
    }
    writeln!(out)?;
    for d in &cx.decls {
        writeln!(out, "{d}")?;
    }
    writeln!(out)?;
    out.push_str(&functions);
    for text in cx.helpers.values() {
        out.push_str(text);
    }
    out.push_str(&kernel_text);
    if target == DeviceTarget::Nvptx && !annotated.is_empty() {
        writeln!(
            out,
            "!nvvm.annotations = !{{{}}}",
            (0..annotated.len())
                .map(|i| format!("!{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        )?;
        for (i, k) in annotated.iter().enumerate() {
            writeln!(out, "!{i} = !{{ptr @{k}, !\"kernel\", i32 1}}")?;
        }
    }
    Ok(DeviceModule {
        ir: out,
        kernels,
        panics: cx.panics,
        libdevice: cx.libdevice,
        host_only,
    })
}

// ---------------------------------------------------------------------------
// Device functions
// ---------------------------------------------------------------------------

struct FnEmit<'f> {
    func: &'f IrFunction,
    ret: Dv,
    entry_allocas: String,
    cur: String,
    cur_label: String,
    n: u32,
    consts: HashMap<ValueId, String>,
    strs: HashMap<ValueId, String>,
}

impl FnEmit<'_> {
    fn t(&mut self) -> String {
        self.n += 1;
        format!("%t{}", self.n)
    }
    fn label(&mut self, prefix: &str) -> String {
        self.n += 1;
        format!("{prefix}{}", self.n)
    }
    fn line(&mut self, s: impl AsRef<str>) {
        self.cur.push_str("  ");
        self.cur.push_str(s.as_ref());
        self.cur.push('\n');
    }
    fn start(&mut self, label: &str) {
        self.cur.push_str(label);
        self.cur.push_str(":\n");
        self.cur_label = label.to_owned();
    }
    fn val(&self, v: ValueId) -> String {
        self.consts
            .get(&v)
            .cloned()
            .unwrap_or_else(|| format!("%v{}", v.0))
    }
    fn dv_of(&self, v: ValueId) -> (Dv, bool) {
        self.func
            .value_type(v)
            .and_then(dv)
            .unwrap_or((Dv::I64, false))
    }
    fn ret_default(&mut self) {
        match self.ret {
            Dv::Unit => self.line("ret void"),
            d => self.line(format!("ret {} {}", d.ll(), d.zero())),
        }
    }
    /// When `bad`, record the error and return.
    fn fail_if(
        &mut self,
        bad: &str,
        code: i32,
        dim: &str,
        bounds: (&str, &str),
        loc: i64,
        msg: usize,
    ) {
        let (a, b) = bounds;
        let fail = self.label("fail");
        let ok = self.label("ok");
        self.line(format!("br i1 {bad}, label %{fail}, label %{ok}"));
        self.start(&fail);
        self.line(format!("call void @__iris_dev_fail(i32 {code}, i32 {dim}, i64 {a}, i64 {b}, i64 {loc}, i64 {msg})"));
        self.ret_default();
        self.start(&ok);
    }
    /// After a call that can fail: return when an error is recorded.
    fn propagate(&mut self) {
        let f = self.t();
        self.line(format!("{f} = call i1 @__iris_dev_failed()"));
        let prop = self.label("prop");
        let ok = self.label("ok");
        self.line(format!("br i1 {f}, label %{prop}, label %{ok}"));
        self.start(&prop);
        self.ret_default();
        self.start(&ok);
    }
    /// `x` widened to i64 for an error record.
    fn wide(&mut self, x: &str, d: Dv, unsigned: bool) -> String {
        match d {
            Dv::I64 => x.to_owned(),
            Dv::Bool | Dv::I8 | Dv::I32 => {
                let w = self.t();
                let ext = if unsigned || d == Dv::Bool {
                    "zext"
                } else {
                    "sext"
                };
                self.line(format!("{w} = {ext} {} {x} to i64", d.ll()));
                w
            }
            _ => "0".to_owned(),
        }
    }
}

fn location(module: &IrModule, func: &IrFunction, block: u32, idx: usize) -> i64 {
    let Some(src) = module.source.as_deref() else {
        return 0;
    };
    func.error_span(block, idx)
        .and_then(|b| src.locate(b))
        .map(|(file, line, col)| super::llvm_ir::pack_location(file, line, col))
        .unwrap_or(0)
}

fn emit_function(cx: &mut ModCtx, func: &IrFunction) -> Result<String, CodegenError> {
    let ret = dv(&func.return_ty).map(|(d, _)| d).unwrap_or(Dv::Unit);
    let mut fe = FnEmit {
        func,
        ret,
        entry_allocas: String::new(),
        cur: String::new(),
        cur_label: String::new(),
        n: 0,
        consts: HashMap::new(),
        strs: HashMap::new(),
    };
    for instr in func.blocks().iter().flat_map(|b| &b.instrs) {
        match instr {
            IrInstr::ConstInt { result, value, ty } => {
                let d = dv(ty).map(|x| x.0).unwrap_or(Dv::I64);
                let lit = if d == Dv::Bool {
                    (*value != 0).to_string()
                } else if d.is_float() {
                    float_lit(*value as f64, d)
                } else {
                    value.to_string()
                };
                fe.consts.insert(*result, lit);
            }
            IrInstr::ConstFloat { result, value, ty } => {
                let d = dv(ty).map(|x| x.0).unwrap_or(Dv::F64);
                fe.consts.insert(*result, float_lit(*value, d));
            }
            IrInstr::ConstBool { result, value } => {
                fe.consts.insert(*result, value.to_string());
            }
            IrInstr::ConstStr { result, value } => {
                fe.strs.insert(*result, value.clone());
            }
            _ => {}
        }
    }

    // Each IRIS block's text and the label its terminator ends up in.
    let mut bodies: Vec<(BlockId, String, String)> = Vec::new();
    for block in func.blocks() {
        fe.cur = String::new();
        fe.cur_label = format!("bb{}", block.id.0);
        for (idx, instr) in block.instrs.iter().enumerate() {
            let loc = location(cx.module, func, block.id.0, idx);
            emit_instr(cx, &mut fe, instr, loc)?;
        }
        // A block that ends in a `panic` falls into nothing.
        if !matches!(
            block.instrs.last(),
            Some(IrInstr::Br { .. } | IrInstr::CondBr { .. } | IrInstr::Return { .. })
        ) {
            fe.line("unreachable");
        }
        bodies.push((block.id, std::mem::take(&mut fe.cur), fe.cur_label.clone()));
    }
    let final_label: HashMap<BlockId, String> =
        bodies.iter().map(|(id, _, l)| (*id, l.clone())).collect();

    // Phis for block parameters, from each predecessor's terminator.
    let mut incoming: HashMap<(BlockId, usize), Vec<(String, String)>> = HashMap::new();
    for block in func.blocks() {
        let from = final_label[&block.id].clone();
        let mut edge = |target: &BlockId, args: &Vec<ValueId>| {
            for (k, a) in args.iter().enumerate() {
                incoming
                    .entry((*target, k))
                    .or_default()
                    .push((fe.val(*a), from.clone()));
            }
        };
        match block.instrs.last() {
            Some(IrInstr::Br { target, args }) => edge(target, args),
            Some(IrInstr::CondBr {
                then_block,
                then_args,
                else_block,
                else_args,
                ..
            }) => {
                edge(then_block, then_args);
                edge(else_block, else_args);
            }
            _ => {}
        }
    }

    let params: Vec<String> = func.blocks()[0]
        .params
        .iter()
        .map(|p| format!("{} %v{}", fe.dv_of(p.id).0.ll(), p.id.0))
        .collect();
    let linkage = if cx
        .module
        .function_by_name(&func.name)
        .is_some_and(|f| f.attrs.iter().any(|a| a == "kernel"))
    {
        ""
    } else {
        "internal "
    };
    let mut out = format!(
        "define {linkage}{} @{}({}) {{\n",
        ret.ll(),
        func.name,
        params.join(", ")
    );
    for (i, (block, body, _)) in bodies.iter().enumerate() {
        writeln!(out, "bb{}:", block.0)?;
        if i == 0 {
            out.push_str(&fe.entry_allocas);
        } else {
            let b = func.blocks().iter().find(|x| x.id == *block).unwrap();
            for (k, p) in b.params.iter().enumerate() {
                let arms: Vec<String> = incoming
                    .get(&(*block, k))
                    .map(|v| {
                        v.iter()
                            .map(|(val, from)| format!("[ {val}, %{from} ]"))
                            .collect()
                    })
                    .unwrap_or_default();
                if arms.is_empty() {
                    continue;
                }
                writeln!(
                    out,
                    "  %v{} = phi {} {}",
                    p.id.0,
                    fe.dv_of(p.id).0.ll(),
                    arms.join(", ")
                )?;
            }
        }
        out.push_str(body);
    }
    out.push_str("}\n\n");
    Ok(out)
}

fn emit_instr(
    cx: &mut ModCtx,
    fe: &mut FnEmit,
    instr: &IrInstr,
    loc: i64,
) -> Result<(), CodegenError> {
    match instr {
        IrInstr::ConstInt { .. }
        | IrInstr::ConstFloat { .. }
        | IrInstr::ConstBool { .. }
        | IrInstr::ConstStr { .. } => {}
        IrInstr::BinOp {
            result,
            op,
            lhs,
            rhs,
            ..
        } => {
            let (d, unsigned) = fe.dv_of(*lhs);
            let (a, b) = (fe.val(*lhs), fe.val(*rhs));
            let r = format!("%v{}", result.0);
            if d == Dv::Tensor {
                let code = match op {
                    BinOp::Add => 0,
                    BinOp::Sub => 1,
                    BinOp::Mul => 2,
                    _ => 3,
                };
                helper_tbin(cx);
                fe.line(format!(
                    "{r} = call ptr @__iris_dev_tbin(ptr {a}, ptr {b}, i32 {code}, i64 {loc})"
                ));
                fe.propagate();
                return Ok(());
            }
            emit_binop(cx, fe, *op, d, unsigned, &a, &b, &r, loc);
        }
        IrInstr::UnaryOp {
            result,
            op,
            operand,
            ..
        } => {
            let (d, unsigned) = fe.dv_of(*operand);
            let x = fe.val(*operand);
            emit_unary(cx, fe, *op, d, unsigned, &x, &format!("%v{}", result.0));
        }
        IrInstr::Cast {
            result,
            operand,
            to_ty,
            ..
        } => {
            let (from, fu) = fe.dv_of(*operand);
            let (to, tu) = dv(to_ty).unwrap_or((from, fu));
            let x = fe.val(*operand);
            let r = format!("%v{}", result.0);
            emit_cast(cx, fe, from, fu, to, tu, &x, &r);
        }
        IrInstr::Load {
            result,
            tensor,
            indices,
            result_ty,
        } => {
            let off = tensor_offset(cx, fe, *tensor, indices, loc);
            let t = fe.val(*tensor);
            let data = fe.t();
            fe.line(format!("{data} = load ptr, ptr {t}"));
            let p = fe.t();
            fe.line(format!(
                "{p} = getelementptr inbounds float, ptr {data}, i64 {off}"
            ));
            let x = fe.t();
            fe.line(format!("{x} = load float, ptr {p}"));
            let (d, u) = dv(result_ty).unwrap_or((Dv::F32, false));
            emit_cast(cx, fe, Dv::F32, false, d, u, &x, &format!("%v{}", result.0));
        }
        IrInstr::Store {
            tensor,
            indices,
            value,
        } => {
            let off = tensor_offset(cx, fe, *tensor, indices, loc);
            let (d, u) = fe.dv_of(*value);
            let v = fe.val(*value);
            let f = fe.t();
            emit_cast(cx, fe, d, u, Dv::F32, false, &v, &f);
            let t = fe.val(*tensor);
            let data = fe.t();
            fe.line(format!("{data} = load ptr, ptr {t}"));
            let p = fe.t();
            fe.line(format!(
                "{p} = getelementptr inbounds float, ptr {data}, i64 {off}"
            ));
            fe.line(format!("store float {f}, ptr {p}"));
        }
        IrInstr::TensorOp {
            result,
            op,
            inputs,
            result_ty,
        } => {
            let r = format!("%v{}", result.0);
            let x = fe.val(inputs[0]);
            match op {
                TensorOp::Unary { op } => {
                    let name = helper_tunary(cx, op);
                    fe.line(format!("{r} = call ptr @{name}(ptr {x}, i64 {loc})"));
                }
                TensorOp::Einsum { notation } => {
                    let name = helper_einsum(cx, notation)?;
                    let args: Vec<String> = inputs
                        .iter()
                        .map(|i| format!("ptr {}", fe.val(*i)))
                        .collect();
                    fe.line(format!(
                        "{r} = call ptr @{name}({}, i64 {loc})",
                        args.join(", ")
                    ));
                }
                TensorOp::Reshape => {
                    let dims: Vec<String> = if inputs.len() > 1 {
                        inputs[1..]
                            .iter()
                            .map(|i| {
                                let (d, u) = fe.dv_of(*i);
                                let v = fe.val(*i);
                                fe.wide(&v, d, u)
                            })
                            .collect()
                    } else if let IrType::Tensor { shape, .. } = result_ty {
                        shape
                            .0
                            .iter()
                            .map(|d| {
                                if let Dim::Literal(n) = d {
                                    n.to_string()
                                } else {
                                    "1".into()
                                }
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let arr = entry_array(fe, "i64", &dims);
                    helper_reshape(cx);
                    fe.line(format!(
                        "{r} = call ptr @__iris_dev_reshape(ptr {x}, i32 {}, ptr {arr}, i64 {loc})",
                        dims.len()
                    ));
                }
                TensorOp::Transpose { axes } => {
                    let vals: Vec<String> = axes.iter().map(|a| a.to_string()).collect();
                    let arr = entry_array(fe, "i64", &vals);
                    helper_transpose(cx);
                    fe.line(format!("{r} = call ptr @__iris_dev_transpose(ptr {x}, i32 {}, ptr {arr}, i64 {loc})", vals.len()));
                }
                TensorOp::Reduce { op, axes, keepdims } => {
                    let code = REDUCE_OPS.iter().position(|o| o == op).unwrap_or(0);
                    let vals: Vec<String> = axes.iter().map(|a| a.to_string()).collect();
                    let arr = entry_array(fe, "i64", &vals);
                    helper_reduce(cx);
                    fe.line(format!(
                        "{r} = call ptr @__iris_dev_reduce(ptr {x}, i32 {code}, i32 {}, ptr {arr}, i1 {keepdims}, i64 {loc})",
                        vals.len()
                    ));
                }
            }
            fe.propagate();
        }
        IrInstr::Call {
            result,
            callee,
            args,
            ..
        } => {
            let callee_fn = cx
                .module
                .function_by_name(callee)
                .ok_or_else(|| err(format!("no function `{callee}`")))?;
            let rt = dv(&callee_fn.return_ty).map(|x| x.0).unwrap_or(Dv::Unit);
            let a: Vec<String> = args
                .iter()
                .map(|v| format!("{} {}", fe.dv_of(*v).0.ll(), fe.val(*v)))
                .collect();
            match (result, rt) {
                (Some(r), d) if d != Dv::Unit => fe.line(format!(
                    "%v{} = call {} @{callee}({})",
                    r.0,
                    d.ll(),
                    a.join(", ")
                )),
                _ => fe.line(format!("call {} @{callee}({})", rt.ll(), a.join(", "))),
            }
            fe.propagate();
        }
        IrInstr::Panic { msg, .. } => {
            let text = fe.strs.get(msg).cloned().unwrap_or_else(|| "panic".into());
            let idx = cx.panic_index(&text);
            fe.line(format!("call void @__iris_dev_fail(i32 {ERR_PANIC}, i32 0, i64 0, i64 0, i64 {loc}, i64 {idx})"));
            fe.ret_default();
            let after = fe.label("dead");
            fe.start(&after);
        }
        IrInstr::Barrier => {
            if cx.target == DeviceTarget::Nvptx {
                fe.line("call void @llvm.nvvm.barrier0()");
            }
        }
        IrInstr::Br { target, .. } => fe.line(format!("br label %bb{}", target.0)),
        IrInstr::CondBr {
            cond,
            then_block,
            else_block,
            ..
        } => {
            let c = fe.val(*cond);
            fe.line(format!(
                "br i1 {c}, label %bb{}, label %bb{}",
                then_block.0, else_block.0
            ));
        }
        IrInstr::Return { values } => match (values.first(), fe.ret) {
            (Some(v), d) if d != Dv::Unit => {
                let x = fe.val(*v);
                fe.line(format!("ret {} {x}", d.ll()));
            }
            _ => fe.ret_default(),
        },
        other => return Err(err(format!("`{}` has no device form", instr_name(other)))),
    }
    Ok(())
}

/// An entry-block array of `ty` holding `vals`.
fn entry_array(fe: &mut FnEmit, ty: &str, vals: &[String]) -> String {
    let arr = fe.t();
    fe.entry_allocas.push_str(&format!(
        "  {arr} = alloca [{} x {ty}]\n",
        vals.len().max(1)
    ));
    for (k, v) in vals.iter().enumerate() {
        let p = fe.t();
        fe.line(format!(
            "{p} = getelementptr inbounds [{} x {ty}], ptr {arr}, i64 0, i64 {k}",
            vals.len().max(1)
        ));
        fe.line(format!("store {ty} {v}, ptr {p}"));
    }
    arr
}

/// The flat offset `tensor[indices]` names, checked; returns when out of range.
fn tensor_offset(
    cx: &mut ModCtx,
    fe: &mut FnEmit,
    tensor: ValueId,
    indices: &[ValueId],
    loc: i64,
) -> String {
    let t = fe.val(tensor);
    let idx: Vec<String> = indices
        .iter()
        .map(|i| {
            let (d, u) = fe.dv_of(*i);
            let v = fe.val(*i);
            fe.wide(&v, d, u)
        })
        .collect();
    let off = fe.t();
    if idx.len() == 1 {
        helper_off1(cx);
        fe.line(format!(
            "{off} = call i64 @__iris_dev_off1(ptr {t}, i64 {}, i64 {loc})",
            idx[0]
        ));
    } else {
        let arr = entry_array(fe, "i64", &idx);
        helper_offn(cx);
        fe.line(format!(
            "{off} = call i64 @__iris_dev_offn(ptr {t}, i32 {}, ptr {arr}, i64 {loc})",
            idx.len()
        ));
    }
    let bad = fe.t();
    fe.line(format!("{bad} = icmp slt i64 {off}, 0"));
    let ret = fe.label("oob");
    let ok = fe.label("ok");
    fe.line(format!("br i1 {bad}, label %{ret}, label %{ok}"));
    fe.start(&ret);
    fe.ret_default();
    fe.start(&ok);
    off
}

#[allow(clippy::too_many_arguments)]
fn emit_binop(
    cx: &mut ModCtx,
    fe: &mut FnEmit,
    op: BinOp,
    d: Dv,
    unsigned: bool,
    a: &str,
    b: &str,
    r: &str,
    loc: i64,
) {
    let ty = d.ll();
    if matches!(
        op,
        BinOp::CmpEq | BinOp::CmpNe | BinOp::CmpLt | BinOp::CmpLe | BinOp::CmpGt | BinOp::CmpGe
    ) {
        let pred = if d.is_float() {
            match op {
                BinOp::CmpEq => "fcmp oeq",
                BinOp::CmpNe => "fcmp une",
                BinOp::CmpLt => "fcmp olt",
                BinOp::CmpLe => "fcmp ole",
                BinOp::CmpGt => "fcmp ogt",
                _ => "fcmp oge",
            }
        } else {
            match (op, unsigned) {
                (BinOp::CmpEq, _) => "icmp eq",
                (BinOp::CmpNe, _) => "icmp ne",
                (BinOp::CmpLt, false) => "icmp slt",
                (BinOp::CmpLt, true) => "icmp ult",
                (BinOp::CmpLe, false) => "icmp sle",
                (BinOp::CmpLe, true) => "icmp ule",
                (BinOp::CmpGt, false) => "icmp sgt",
                (BinOp::CmpGt, true) => "icmp ugt",
                (_, false) => "icmp sge",
                (_, true) => "icmp uge",
            }
        };
        fe.line(format!("{r} = {pred} {ty} {a}, {b}"));
        return;
    }
    if d.is_float() {
        match op {
            BinOp::Add => fe.line(format!("{r} = fadd {ty} {a}, {b}")),
            BinOp::Sub => fe.line(format!("{r} = fsub {ty} {a}, {b}")),
            BinOp::Mul => fe.line(format!("{r} = fmul {ty} {a}, {b}")),
            BinOp::Div => fe.line(format!("{r} = fdiv {ty} {a}, {b}")),
            BinOp::Mod => fe.line(format!("{r} = frem {ty} {a}, {b}")),
            BinOp::FloorDiv => {
                let q = fe.t();
                fe.line(format!("{q} = fdiv {ty} {a}, {b}"));
                let f = cx.intrinsic("floor", ty, 1);
                fe.line(format!("{r} = call {ty} @{f}({ty} {q})"));
            }
            BinOp::Pow => {
                let f = cx.libdevice("pow", ty, 2);
                fe.line(format!("{r} = call {ty} @{f}({ty} {a}, {ty} {b})"));
            }
            BinOp::Min | BinOp::Max => {
                let f = cx.intrinsic(if op == BinOp::Min { "minnum" } else { "maxnum" }, ty, 2);
                fe.line(format!("{r} = call {ty} @{f}({ty} {a}, {ty} {b})"));
            }
            _ => fe.line(format!("{r} = fadd {ty} {a}, {b}")),
        }
        return;
    }
    // Integers (and bool for the bitwise operators).
    let checked = matches!(d, Dv::I64 | Dv::I32);
    let min = if d == Dv::I64 {
        "-9223372036854775808"
    } else {
        "-2147483648"
    };
    let (wa, wb) = (fe.wide(a, d, unsigned), fe.wide(b, d, unsigned));
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul if checked => {
            let (name, code) = match op {
                BinOp::Add => ("sadd", 0),
                BinOp::Sub => ("ssub", 1),
                _ => ("smul", 2),
            };
            let full = format!("llvm.{name}.with.overflow.{ty}");
            cx.declare(format!("declare {{ {ty}, i1 }} @{full}({ty}, {ty})"));
            let pair = fe.t();
            fe.line(format!(
                "{pair} = call {{ {ty}, i1 }} @{full}({ty} {a}, {ty} {b})"
            ));
            let ov = fe.t();
            fe.line(format!("{ov} = extractvalue {{ {ty}, i1 }} {pair}, 1"));
            fe.fail_if(&ov, ERR_OVERFLOW, &code.to_string(), (&wa, &wb), loc, 0);
            fe.line(format!("{r} = extractvalue {{ {ty}, i1 }} {pair}, 0"));
        }
        BinOp::Add | BinOp::WrapAdd => fe.line(format!("{r} = add {ty} {a}, {b}")),
        BinOp::Sub | BinOp::WrapSub => fe.line(format!("{r} = sub {ty} {a}, {b}")),
        BinOp::Mul | BinOp::WrapMul => fe.line(format!("{r} = mul {ty} {a}, {b}")),
        BinOp::Div | BinOp::FloorDiv | BinOp::Mod => {
            let z = fe.t();
            fe.line(format!("{z} = icmp eq {ty} {b}, 0"));
            fe.fail_if(&z, ERR_DIV_ZERO, "0", (&wa, &wb), loc, 0);
            if checked {
                let m = fe.t();
                fe.line(format!("{m} = icmp eq {ty} {a}, {min}"));
                let n = fe.t();
                fe.line(format!("{n} = icmp eq {ty} {b}, -1"));
                let edge = fe.t();
                fe.line(format!("{edge} = and i1 {m}, {n}"));
                if op == BinOp::Mod {
                    // MIN % -1 is 0.
                    let safe = fe.t();
                    fe.line(format!("{safe} = select i1 {edge}, {ty} 1, {ty} {b}"));
                    fe.line(format!("{r} = srem {ty} {a}, {safe}"));
                    return;
                }
                fe.fail_if(&edge, ERR_OVERFLOW, "3", (&wa, &wb), loc, 0);
                if op == BinOp::Div {
                    fe.line(format!("{r} = sdiv {ty} {a}, {b}"));
                } else {
                    let q = fe.t();
                    fe.line(format!("{q} = sdiv {ty} {a}, {b}"));
                    let rem = fe.t();
                    fe.line(format!("{rem} = srem {ty} {a}, {b}"));
                    let rnz = fe.t();
                    fe.line(format!("{rnz} = icmp ne {ty} {rem}, 0"));
                    let x = fe.t();
                    fe.line(format!("{x} = xor {ty} {rem}, {b}"));
                    let neg = fe.t();
                    fe.line(format!("{neg} = icmp slt {ty} {x}, 0"));
                    let adj = fe.t();
                    fe.line(format!("{adj} = and i1 {rnz}, {neg}"));
                    let dz = fe.t();
                    fe.line(format!("{dz} = zext i1 {adj} to {ty}"));
                    fe.line(format!("{r} = sub {ty} {q}, {dz}"));
                }
            } else {
                let inst = match (op, unsigned) {
                    (BinOp::Mod, true) => "urem",
                    (BinOp::Mod, false) => "srem",
                    (_, true) => "udiv",
                    (_, false) => "sdiv",
                };
                fe.line(format!("{r} = {inst} {ty} {a}, {b}"));
            }
        }
        BinOp::Pow => {
            helper_pow(cx);
            if d == Dv::I64 {
                fe.line(format!(
                    "{r} = call i64 @__iris_dev_pow_i64(i64 {a}, i64 {b})"
                ));
            } else {
                let p = fe.t();
                fe.line(format!(
                    "{p} = call i64 @__iris_dev_pow_i64(i64 {wa}, i64 {wb})"
                ));
                fe.line(format!("{r} = trunc i64 {p} to {ty}"));
            }
        }
        BinOp::Min | BinOp::Max => {
            let name = match (op, unsigned) {
                (BinOp::Min, false) => "smin",
                (BinOp::Min, true) => "umin",
                (_, false) => "smax",
                (_, true) => "umax",
            };
            let f = cx.intrinsic(name, ty, 2);
            fe.line(format!("{r} = call {ty} @{f}({ty} {a}, {ty} {b})"));
        }
        BinOp::BitAnd => fe.line(format!("{r} = and {ty} {a}, {b}")),
        BinOp::BitOr => fe.line(format!("{r} = or {ty} {a}, {b}")),
        BinOp::BitXor => fe.line(format!("{r} = xor {ty} {a}, {b}")),
        BinOp::Shl | BinOp::Shr => {
            let amt = fe.t();
            fe.line(format!(
                "{amt} = and {ty} {b}, {}",
                d.bits().saturating_sub(1)
            ));
            let inst = match (op, unsigned) {
                (BinOp::Shl, _) => "shl",
                (_, true) => "lshr",
                _ => "ashr",
            };
            fe.line(format!("{r} = {inst} {ty} {a}, {amt}"));
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_unary(
    cx: &mut ModCtx,
    fe: &mut FnEmit,
    op: ScalarUnaryOp,
    d: Dv,
    _unsigned: bool,
    x: &str,
    r: &str,
) {
    let ty = d.ll();
    let float = d.is_float();
    let lib = |cx: &mut ModCtx, name: &str| cx.libdevice(name, ty, 1);
    match op {
        ScalarUnaryOp::Neg if float => fe.line(format!("{r} = fneg {ty} {x}")),
        ScalarUnaryOp::Neg => fe.line(format!("{r} = sub {ty} 0, {x}")),
        ScalarUnaryOp::Not => fe.line(format!("{r} = xor i1 {x}, true")),
        ScalarUnaryOp::BitNot => fe.line(format!("{r} = xor {ty} {x}, -1")),
        ScalarUnaryOp::Abs if float => {
            let f = cx.intrinsic("fabs", ty, 1);
            fe.line(format!("{r} = call {ty} @{f}({ty} {x})"));
        }
        ScalarUnaryOp::Abs => {
            let full = format!("llvm.abs.{ty}");
            cx.declare(format!("declare {ty} @{full}({ty}, i1)"));
            fe.line(format!("{r} = call {ty} @{full}({ty} {x}, i1 false)"));
        }
        ScalarUnaryOp::Sqrt | ScalarUnaryOp::Floor | ScalarUnaryOp::Ceil | ScalarUnaryOp::Round => {
            let name = match op {
                ScalarUnaryOp::Sqrt => "sqrt",
                ScalarUnaryOp::Floor => "floor",
                ScalarUnaryOp::Ceil => "ceil",
                _ => "round",
            };
            let f = cx.intrinsic(name, ty, 1);
            fe.line(format!("{r} = call {ty} @{f}({ty} {x})"));
        }
        ScalarUnaryOp::Sin
        | ScalarUnaryOp::Cos
        | ScalarUnaryOp::Tan
        | ScalarUnaryOp::Exp
        | ScalarUnaryOp::Log
        | ScalarUnaryOp::Log2 => {
            let name = match op {
                ScalarUnaryOp::Sin => "sin",
                ScalarUnaryOp::Cos => "cos",
                ScalarUnaryOp::Tan => "tan",
                ScalarUnaryOp::Exp => "exp",
                ScalarUnaryOp::Log => "log",
                _ => "log2",
            };
            let f = lib(cx, name);
            fe.line(format!("{r} = call {ty} @{f}({ty} {x})"));
        }
        ScalarUnaryOp::Sign => {
            let (one, minus, zero) = if float {
                ("1.0", "-1.0", "0.0")
            } else {
                ("1", "-1", "0")
            };
            let (gt, lt) = if float {
                ("fcmp ogt", "fcmp olt")
            } else {
                ("icmp sgt", "icmp slt")
            };
            let pos = fe.t();
            fe.line(format!("{pos} = {gt} {ty} {x}, {zero}"));
            let neg = fe.t();
            fe.line(format!("{neg} = {lt} {ty} {x}, {zero}"));
            let mid = fe.t();
            fe.line(format!(
                "{mid} = select i1 {neg}, {ty} {minus}, {ty} {zero}"
            ));
            fe.line(format!("{r} = select i1 {pos}, {ty} {one}, {ty} {mid}"));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_cast(
    cx: &mut ModCtx,
    fe: &mut FnEmit,
    from: Dv,
    fu: bool,
    to: Dv,
    tu: bool,
    x: &str,
    r: &str,
) {
    let (f, t) = (from.ll(), to.ll());
    if from == to {
        // A copy, so `r` names a value.
        if from.is_float() {
            fe.line(format!("{r} = fadd {f} {x}, -0.0"));
        } else {
            fe.line(format!(
                "{r} = or {f} {x}, {}",
                if from == Dv::Bool { "false" } else { "0" }
            ));
        }
        return;
    }
    match (from.is_float(), to.is_float()) {
        (true, true) => {
            let inst = if from == Dv::F32 { "fpext" } else { "fptrunc" };
            fe.line(format!("{r} = {inst} {f} {x} to {t}"));
        }
        (true, false) if to == Dv::Bool => fe.line(format!("{r} = fcmp une {f} {x}, 0.0")),
        (true, false) => {
            let name = if tu { "fptoui" } else { "fptosi" };
            let sfx = if from == Dv::F32 { "f32" } else { "f64" };
            let full = format!("llvm.{name}.sat.{t}.{sfx}");
            cx.declare(format!("declare {t} @{full}({f})"));
            fe.line(format!("{r} = call {t} @{full}({f} {x})"));
        }
        (false, true) => {
            let inst = if fu || from == Dv::Bool {
                "uitofp"
            } else {
                "sitofp"
            };
            fe.line(format!("{r} = {inst} {f} {x} to {t}"));
        }
        (false, false) => {
            let (fb, tb) = (from.bits(), to.bits());
            if fb > tb {
                fe.line(format!("{r} = trunc {f} {x} to {t}"));
            } else {
                let inst = if fu || from == Dv::Bool {
                    "zext"
                } else {
                    "sext"
                };
                fe.line(format!("{r} = {inst} {f} {x} to {t}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Kernels
// ---------------------------------------------------------------------------

/// The flat global thread index, `ctaid.x * ntid.x + tid.x`, as i64.
fn thread_index(out: &mut String) -> Result<(), CodegenError> {
    writeln!(out, "  %tid = call i32 @llvm.nvvm.read.ptx.sreg.tid.x()")?;
    writeln!(
        out,
        "  %ctaid = call i32 @llvm.nvvm.read.ptx.sreg.ctaid.x()"
    )?;
    writeln!(out, "  %ntid = call i32 @llvm.nvvm.read.ptx.sreg.ntid.x()")?;
    writeln!(out, "  %tid64 = zext i32 %tid to i64")?;
    writeln!(out, "  %ctaid64 = zext i32 %ctaid to i64")?;
    writeln!(out, "  %ntid64 = zext i32 %ntid to i64")?;
    writeln!(out, "  %base = mul i64 %ctaid64, %ntid64")?;
    writeln!(out, "  %gid = add i64 %base, %tid64")?;
    Ok(())
}

/// `{body}_kernel(i64 start, i64 end, ptr env)`.
fn par_kernel(cx: &mut ModCtx, body: &IrFunction) -> Result<(String, KernelInfo), CodegenError> {
    let args = &body.blocks()[0].params;
    // A capture of a host type (unread, or the body would be host code)
    // travels as an empty slot.
    let opaque: Vec<bool> = args
        .iter()
        .skip(1)
        .map(|p| body.value_type(p.id).and_then(dv).is_none())
        .collect();
    let caps: Vec<Dv> = args
        .iter()
        .skip(1)
        .map(|p| {
            body.value_type(p.id)
                .and_then(dv)
                .map(|x| x.0)
                .unwrap_or(Dv::I64)
        })
        .collect();
    let kernel = format!("{}_kernel", body.name);
    let mut out = format!("define void @{kernel}(i64 %start, i64 %end, ptr %env) {{\nentry:\n");
    let unpack = |out: &mut String| -> Result<Vec<String>, CodegenError> {
        let mut vals = Vec::new();
        for (k, d) in caps.iter().enumerate() {
            writeln!(
                out,
                "  %s{k}p = getelementptr inbounds i64, ptr %env, i64 {k}"
            )?;
            let ty = match d {
                Dv::Bool => "i8",
                other => other.ll(),
            };
            writeln!(out, "  %s{k} = load {ty}, ptr %s{k}p")?;
            if *d == Dv::Bool {
                writeln!(out, "  %s{k}b = trunc i8 %s{k} to i1")?;
                vals.push(format!("i1 %s{k}b"));
            } else {
                vals.push(format!("{} %s{k}", d.ll()));
            }
        }
        Ok(vals)
    };
    let ret = dv(&body.return_ty).map(|x| x.0).unwrap_or(Dv::Unit);
    match cx.target {
        DeviceTarget::Nvptx => {
            thread_index(&mut out)?;
            writeln!(out, "  %i = add i64 %start, %gid")?;
            writeln!(out, "  %in = icmp slt i64 %i, %end")?;
            writeln!(out, "  br i1 %in, label %run, label %done")?;
            writeln!(out, "run:")?;
            let vals = unpack(&mut out)?;
            let mut call_args = vec!["i64 %i".to_string()];
            call_args.extend(vals);
            writeln!(
                out,
                "  call {} @{}({})",
                ret.ll(),
                body.name,
                call_args.join(", ")
            )?;
            writeln!(out, "  br label %done")?;
            writeln!(out, "done:")?;
            writeln!(out, "  ret void")?;
        }
        DeviceTarget::HostTest => {
            // Every iteration in turn, stopping at the first error.
            let vals = unpack(&mut out)?;
            writeln!(out, "  br label %head")?;
            writeln!(out, "head:")?;
            writeln!(out, "  %i = phi i64 [ %start, %entry ], [ %next, %body ]")?;
            writeln!(out, "  %more = icmp slt i64 %i, %end")?;
            writeln!(out, "  br i1 %more, label %body, label %done")?;
            writeln!(out, "body:")?;
            let mut call_args = vec!["i64 %i".to_string()];
            call_args.extend(vals);
            writeln!(
                out,
                "  call {} @{}({})",
                ret.ll(),
                body.name,
                call_args.join(", ")
            )?;
            writeln!(out, "  %next = add i64 %i, 1")?;
            writeln!(out, "  br label %head")?;
            writeln!(out, "done:")?;
            writeln!(out, "  ret void")?;
        }
    }
    writeln!(out, "}}\n")?;
    let captures: String = caps
        .iter()
        .zip(&opaque)
        .map(|(d, o)| if *o { 'x' } else { d.slot() })
        .collect();
    Ok((
        out,
        KernelInfo {
            body: body.name.clone(),
            kernel,
            captures,
        },
    ))
}

/// `{f}_kernel(i64 n, ptr a0, ..., ptr out)`: `out[i] = f(a0[i], ...)`.
fn map_kernel(cx: &mut ModCtx, f: &IrFunction) -> Result<String, CodegenError> {
    let args = &f.blocks()[0].params;
    let tys: Vec<Dv> = args
        .iter()
        .map(|p| {
            f.value_type(p.id)
                .and_then(dv)
                .map(|x| x.0)
                .unwrap_or(Dv::I64)
        })
        .collect();
    let ret = dv(&f.return_ty).map(|x| x.0).unwrap_or(Dv::Unit);
    let mut params: Vec<String> = vec!["i64 %n".into()];
    params.extend((0..tys.len()).map(|k| format!("ptr %a{k}")));
    if ret != Dv::Unit {
        params.push("ptr %out".into());
    }
    let mut out = format!(
        "define void @{}_kernel({}) {{\nentry:\n",
        f.name,
        params.join(", ")
    );
    match cx.target {
        DeviceTarget::Nvptx => {
            thread_index(&mut out)?;
            writeln!(out, "  %i = add i64 0, %gid")?;
        }
        DeviceTarget::HostTest => writeln!(out, "  %i = add i64 0, 0")?,
    }
    writeln!(out, "  %in = icmp slt i64 %i, %n")?;
    writeln!(out, "  br i1 %in, label %run, label %done")?;
    writeln!(out, "run:")?;
    let mut call = Vec::new();
    for (k, d) in tys.iter().enumerate() {
        if *d == Dv::Tensor {
            call.push(format!("ptr %a{k}"));
        } else {
            let ty = if *d == Dv::Bool { "i8" } else { d.ll() };
            writeln!(
                out,
                "  %p{k} = getelementptr inbounds {ty}, ptr %a{k}, i64 %i"
            )?;
            writeln!(out, "  %x{k} = load {ty}, ptr %p{k}")?;
            if *d == Dv::Bool {
                writeln!(out, "  %x{k}b = trunc i8 %x{k} to i1")?;
                call.push(format!("i1 %x{k}b"));
            } else {
                call.push(format!("{ty} %x{k}"));
            }
        }
    }
    if ret == Dv::Unit {
        writeln!(out, "  call void @{}({})", f.name, call.join(", "))?;
    } else {
        writeln!(
            out,
            "  %r = call {} @{}({})",
            ret.ll(),
            f.name,
            call.join(", ")
        )?;
        let ty = if ret == Dv::Bool { "i8" } else { ret.ll() };
        writeln!(out, "  %q = getelementptr inbounds {ty}, ptr %out, i64 %i")?;
        if ret == Dv::Bool {
            writeln!(out, "  %rb = zext i1 %r to i8")?;
            writeln!(out, "  store i8 %rb, ptr %q")?;
        } else {
            writeln!(out, "  store {ty} %r, ptr %q")?;
        }
    }
    writeln!(out, "  br label %done")?;
    writeln!(out, "done:")?;
    writeln!(out, "  ret void")?;
    writeln!(out, "}}\n")?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Device helpers, emitted once each when used
// ---------------------------------------------------------------------------

fn helpers_base(cx: &mut ModCtx) {
    let e = cx.errp();
    cx.helper("__iris_dev_fail", |_| {
        format!(
            "define internal void @__iris_dev_fail(i32 %code, i32 %dim, i64 %a, i64 %b, i64 %loc, i64 %msg) {{\n\
entry:\n\
  %r = cmpxchg ptr {e}, i32 0, i32 %code seq_cst seq_cst\n\
  %won = extractvalue {{ i32, i1 }} %r, 1\n\
  br i1 %won, label %set, label %done\n\
set:\n\
  %p1 = getelementptr inbounds %err_t, ptr {e}, i32 0, i32 1\n\
  store volatile i32 %dim, ptr %p1\n\
  %p2 = getelementptr inbounds %err_t, ptr {e}, i32 0, i32 2\n\
  store volatile i64 %a, ptr %p2\n\
  %p3 = getelementptr inbounds %err_t, ptr {e}, i32 0, i32 3\n\
  store volatile i64 %b, ptr %p3\n\
  %p4 = getelementptr inbounds %err_t, ptr {e}, i32 0, i32 4\n\
  store volatile i64 %loc, ptr %p4\n\
  %p5 = getelementptr inbounds %err_t, ptr {e}, i32 0, i32 5\n\
  store volatile i64 %msg, ptr %p5\n\
  br label %done\n\
done:\n\
  ret void\n\
}}\n\n"
        )
    });
    cx.helper("__iris_dev_failed", |_| {
        format!(
            "define internal i1 @__iris_dev_failed() {{\n\
entry:\n\
  %c = load volatile i32, ptr {e}, align 8\n\
  %f = icmp ne i32 %c, 0\n\
  ret i1 %f\n\
}}\n\n"
        )
    });
    // A tensor of `ndim` dimensions `shape`, data uninitialised: header,
    // shape and data in one allocation.
    cx.helper("__iris_dev_alloc", |_| {
        "define internal ptr @__iris_dev_alloc(i32 %ndim, ptr %shape) {\n\
entry:\n\
  %n = sext i32 %ndim to i64\n\
  br label %h\n\
h:\n\
  %i = phi i64 [ 0, %entry ], [ %i1, %b ]\n\
  %acc = phi i64 [ 1, %entry ], [ %acc1, %b ]\n\
  %c = icmp slt i64 %i, %n\n\
  br i1 %c, label %b, label %sized\n\
b:\n\
  %sp = getelementptr inbounds i64, ptr %shape, i64 %i\n\
  %d = load i64, ptr %sp\n\
  %acc1 = mul i64 %acc, %d\n\
  %i1 = add i64 %i, 1\n\
  br label %h\n\
sized:\n\
  %sb = mul i64 %n, 8\n\
  %db = mul i64 %acc, 4\n\
  %hb = add i64 32, %sb\n\
  %bytes = add i64 %hb, %db\n\
  %p = call ptr @malloc(i64 %bytes)\n\
  %none = icmp eq ptr %p, null\n\
  br i1 %none, label %oom, label %fill\n\
oom:\n\
  call void @__iris_dev_fail(i32 6, i32 0, i64 %bytes, i64 0, i64 0, i64 0)\n\
  ret ptr null\n\
fill:\n\
  %shp = getelementptr inbounds i8, ptr %p, i64 32\n\
  %dat = getelementptr inbounds i8, ptr %shp, i64 %sb\n\
  store ptr %dat, ptr %p\n\
  %f1 = getelementptr inbounds %dt, ptr %p, i32 0, i32 1\n\
  store ptr %shp, ptr %f1\n\
  %f2 = getelementptr inbounds %dt, ptr %p, i32 0, i32 2\n\
  store i32 %ndim, ptr %f2\n\
  %f3 = getelementptr inbounds %dt, ptr %p, i32 0, i32 3\n\
  store i64 %acc, ptr %f3\n\
  br label %ch\n\
ch:\n\
  %j = phi i64 [ 0, %fill ], [ %j1, %cb ]\n\
  %cc = icmp slt i64 %j, %n\n\
  br i1 %cc, label %cb, label %done\n\
cb:\n\
  %src = getelementptr inbounds i64, ptr %shape, i64 %j\n\
  %v = load i64, ptr %src\n\
  %dst = getelementptr inbounds i64, ptr %shp, i64 %j\n\
  store i64 %v, ptr %dst\n\
  %j1 = add i64 %j, 1\n\
  br label %ch\n\
done:\n\
  ret ptr %p\n\
}\n\n"
            .to_owned()
    });
}

/// Header field accessors, as text.
fn fields(t: &str, pre: &str) -> String {
    format!(
        "  %{pre}dp = getelementptr inbounds %dt, ptr {t}, i32 0, i32 0\n\
  %{pre}data = load ptr, ptr %{pre}dp\n\
  %{pre}sp = getelementptr inbounds %dt, ptr {t}, i32 0, i32 1\n\
  %{pre}shape = load ptr, ptr %{pre}sp\n\
  %{pre}np = getelementptr inbounds %dt, ptr {t}, i32 0, i32 2\n\
  %{pre}nd32 = load i32, ptr %{pre}np\n\
  %{pre}nd = sext i32 %{pre}nd32 to i64\n\
  %{pre}ep = getelementptr inbounds %dt, ptr {t}, i32 0, i32 3\n\
  %{pre}numel = load i64, ptr %{pre}ep\n"
    )
}

fn helper_off1(cx: &mut ModCtx) {
    cx.helper("__iris_dev_off1", |_| {
        "define internal i64 @__iris_dev_off1(ptr %t, i64 %i, i64 %loc) {\n\
entry:\n\
  %ep = getelementptr inbounds %dt, ptr %t, i32 0, i32 3\n\
  %n = load i64, ptr %ep\n\
  %neg = icmp slt i64 %i, 0\n\
  %big = icmp sge i64 %i, %n\n\
  %bad = or i1 %neg, %big\n\
  br i1 %bad, label %fail, label %ok\n\
fail:\n\
  call void @__iris_dev_fail(i32 1, i32 -1, i64 %i, i64 %n, i64 %loc, i64 0)\n\
  ret i64 -1\n\
ok:\n\
  ret i64 %i\n\
}\n\n"
            .to_owned()
    });
}

fn helper_offn(cx: &mut ModCtx) {
    cx.helper("__iris_dev_offn", |_| {
        format!(
            "define internal i64 @__iris_dev_offn(ptr %t, i32 %k, ptr %idx, i64 %loc) {{\n\
entry:\n\
{}\
  %same = icmp eq i32 %k, %nd32\n\
  br i1 %same, label %h, label %rank\n\
rank:\n\
  %k64 = sext i32 %k to i64\n\
  call void @__iris_dev_fail(i32 2, i32 0, i64 %k64, i64 %nd, i64 %loc, i64 0)\n\
  ret i64 -1\n\
h:\n\
  %d = phi i64 [ 0, %entry ], [ %d1, %next ]\n\
  %off = phi i64 [ 0, %entry ], [ %off1, %next ]\n\
  %more = icmp slt i64 %d, %nd\n\
  br i1 %more, label %b, label %done\n\
b:\n\
  %ip = getelementptr inbounds i64, ptr %idx, i64 %d\n\
  %i = load i64, ptr %ip\n\
  %dp2 = getelementptr inbounds i64, ptr %shape, i64 %d\n\
  %dim = load i64, ptr %dp2\n\
  %neg = icmp slt i64 %i, 0\n\
  %big = icmp sge i64 %i, %dim\n\
  %bad = or i1 %neg, %big\n\
  br i1 %bad, label %fail, label %next\n\
fail:\n\
  %d32 = trunc i64 %d to i32\n\
  call void @__iris_dev_fail(i32 1, i32 %d32, i64 %i, i64 %dim, i64 %loc, i64 0)\n\
  ret i64 -1\n\
next:\n\
  %sc = mul i64 %off, %dim\n\
  %off1 = add i64 %sc, %i\n\
  %d1 = add i64 %d, 1\n\
  br label %h\n\
done:\n\
  ret i64 %off\n\
}}\n\n",
            fields("%t", "")
        )
    });
}

/// A new tensor of `t`'s shape, each element `f(x)` as the host computes it.
fn helper_tunary(cx: &mut ModCtx, op: &str) -> String {
    let name = format!("__iris_dev_tunary_{op}");
    let mut expf = String::new();
    let mut logf = String::new();
    let mut tanhf = String::new();
    let mut sqrt = String::new();
    let mut fabs = String::new();
    match op {
        "sigmoid" | "exp" => expf = cx.libdevice("exp", "float", 1),
        "log" => logf = cx.libdevice("log", "float", 1),
        "tanh" => tanhf = cx.libdevice("tanh", "float", 1),
        "sqrt" => sqrt = cx.intrinsic("sqrt", "float", 1),
        "abs" => fabs = cx.intrinsic("fabs", "float", 1),
        _ => {}
    }
    let compute = match op {
        "neg" => "  %y = fneg float %x\n".to_owned(),
        "relu" => "  %pos = fcmp ogt float %x, 0.0\n  %y = select i1 %pos, float %x, float 0.0\n".to_owned(),
        "sigmoid" => format!("  %nx = fneg float %x\n  %e = call float @{expf}(float %nx)\n  %den = fadd float 1.0, %e\n  %y = fdiv float 1.0, %den\n"),
        "tanh" => format!("  %y = call float @{tanhf}(float %x)\n"),
        "exp" => format!("  %y = call float @{expf}(float %x)\n"),
        // log of a non-positive number is -inf, as on the host.
        "log" => format!(
            "  %lpos = fcmp ogt float %x, 0.0\n  %lv = call float @{logf}(float %x)\n  %y = select i1 %lpos, float %lv, float 0xFFF0000000000000\n"
        ),
        // sqrt of a negative number is 0, as on the host.
        "sqrt" => format!(
            "  %nn = fcmp oge float %x, 0.0\n  %sv = call float @{sqrt}(float %x)\n  %y = select i1 %nn, float %sv, float 0.0\n"
        ),
        _ => format!("  %y = call float @{fabs}(float %x)\n"),
    };
    cx.helper(&name, |_| {
        format!(
            "define internal ptr @{name}(ptr %t, i64 %loc) {{\n\
entry:\n\
{}\
  %o = call ptr @__iris_dev_alloc(i32 %nd32, ptr %shape)\n\
  %none = icmp eq ptr %o, null\n\
  br i1 %none, label %out, label %go\n\
go:\n\
  %odp = getelementptr inbounds %dt, ptr %o, i32 0, i32 0\n\
  %odata = load ptr, ptr %odp\n\
  br label %h\n\
h:\n\
  %i = phi i64 [ 0, %go ], [ %i1, %b ]\n\
  %more = icmp slt i64 %i, %numel\n\
  br i1 %more, label %b, label %out\n\
b:\n\
  %xp = getelementptr inbounds float, ptr %data, i64 %i\n\
  %x = load float, ptr %xp\n\
{compute}\
  %yp = getelementptr inbounds float, ptr %odata, i64 %i\n\
  store float %y, ptr %yp\n\
  %i1 = add i64 %i, 1\n\
  br label %h\n\
out:\n\
  ret ptr %o\n\
}}\n\n",
            fields("%t", "")
        )
    });
    name
}

/// Element-wise `a op b` (0 add, 1 sub, 2 mul, 3 div with x / 0 = 0, as on
/// the host), for tensors of the same size.
fn helper_tbin(cx: &mut ModCtx) {
    cx.helper("__iris_dev_tbin", |_| {
        format!(
            "define internal ptr @__iris_dev_tbin(ptr %a, ptr %b, i32 %op, i64 %loc) {{\n\
entry:\n\
{}{}\
  %same = icmp eq i64 %anumel, %bnumel\n\
  br i1 %same, label %alloc, label %fail\n\
fail:\n\
  call void @__iris_dev_fail(i32 7, i32 0, i64 %anumel, i64 %bnumel, i64 %loc, i64 0)\n\
  ret ptr null\n\
alloc:\n\
  %o = call ptr @__iris_dev_alloc(i32 %and32, ptr %ashape)\n\
  %none = icmp eq ptr %o, null\n\
  br i1 %none, label %out, label %go\n\
go:\n\
  %odp = getelementptr inbounds %dt, ptr %o, i32 0, i32 0\n\
  %odata = load ptr, ptr %odp\n\
  br label %h\n\
h:\n\
  %i = phi i64 [ 0, %go ], [ %i1, %body ]\n\
  %more = icmp slt i64 %i, %anumel\n\
  br i1 %more, label %body, label %out\n\
body:\n\
  %xp = getelementptr inbounds float, ptr %adata, i64 %i\n\
  %x = load float, ptr %xp\n\
  %wp = getelementptr inbounds float, ptr %bdata, i64 %i\n\
  %w = load float, ptr %wp\n\
  %s = fadd float %x, %w\n\
  %d = fsub float %x, %w\n\
  %m = fmul float %x, %w\n\
  %q = fdiv float %x, %w\n\
  %wz = fcmp oeq float %w, 0.0\n\
  %qq = select i1 %wz, float 0.0, float %q\n\
  %is0 = icmp eq i32 %op, 0\n\
  %is1 = icmp eq i32 %op, 1\n\
  %is2 = icmp eq i32 %op, 2\n\
  %y2 = select i1 %is2, float %m, float %qq\n\
  %y1 = select i1 %is1, float %d, float %y2\n\
  %y = select i1 %is0, float %s, float %y1\n\
  %yp = getelementptr inbounds float, ptr %odata, i64 %i\n\
  store float %y, ptr %yp\n\
  %i1 = add i64 %i, 1\n\
  br label %h\n\
out:\n\
  ret ptr %o\n\
}}\n\n",
            fields("%a", "a"),
            fields("%b", "b")
        )
    });
}

/// `t` with the `n` dimensions `shape`: the same elements, in the same order.
fn helper_reshape(cx: &mut ModCtx) {
    cx.helper("__iris_dev_reshape", |_| {
        format!(
            "define internal ptr @__iris_dev_reshape(ptr %t, i32 %n, ptr %ns, i64 %loc) {{\n\
entry:\n\
{}\
  %n64 = sext i32 %n to i64\n\
  br label %ph\n\
ph:\n\
  %k = phi i64 [ 0, %entry ], [ %k1, %pb ]\n\
  %acc = phi i64 [ 1, %entry ], [ %acc1, %pb ]\n\
  %pm = icmp slt i64 %k, %n64\n\
  br i1 %pm, label %pb, label %check\n\
pb:\n\
  %dp2 = getelementptr inbounds i64, ptr %ns, i64 %k\n\
  %dv = load i64, ptr %dp2\n\
  %acc1 = mul i64 %acc, %dv\n\
  %k1 = add i64 %k, 1\n\
  br label %ph\n\
check:\n\
  %same = icmp eq i64 %acc, %numel\n\
  br i1 %same, label %alloc, label %fail\n\
fail:\n\
  call void @__iris_dev_fail(i32 7, i32 0, i64 %numel, i64 %acc, i64 %loc, i64 0)\n\
  ret ptr null\n\
alloc:\n\
  %o = call ptr @__iris_dev_alloc(i32 %n, ptr %ns)\n\
  %none = icmp eq ptr %o, null\n\
  br i1 %none, label %out, label %go\n\
go:\n\
  %odp = getelementptr inbounds %dt, ptr %o, i32 0, i32 0\n\
  %odata = load ptr, ptr %odp\n\
  br label %h\n\
h:\n\
  %i = phi i64 [ 0, %go ], [ %i1, %b ]\n\
  %more = icmp slt i64 %i, %numel\n\
  br i1 %more, label %b, label %out\n\
b:\n\
  %xp = getelementptr inbounds float, ptr %data, i64 %i\n\
  %x = load float, ptr %xp\n\
  %yp = getelementptr inbounds float, ptr %odata, i64 %i\n\
  store float %x, ptr %yp\n\
  %i1 = add i64 %i, 1\n\
  br label %h\n\
out:\n\
  ret ptr %o\n\
}}\n\n",
            fields("%t", "")
        )
    });
}

/// `t` with its axes permuted: output axis `d` is input axis `axes[d]`, or,
/// with no axes, the reverse.
fn helper_transpose(cx: &mut ModCtx) {
    cx.helper("__iris_dev_transpose", |_| {
        format!(
            "define internal ptr @__iris_dev_transpose(ptr %t, i32 %nax, ptr %axes, i64 %loc) {{\n\
entry:\n\
{}\
  %nax64 = sext i32 %nax to i64\n\
  %given = icmp ne i32 %nax, 0\n\
  %mism = icmp ne i64 %nax64, %nd\n\
  %bad = and i1 %given, %mism\n\
  br i1 %bad, label %rank, label %scratch\n\
rank:\n\
  call void @__iris_dev_fail(i32 2, i32 0, i64 %nax64, i64 %nd, i64 %loc, i64 0)\n\
  ret ptr null\n\
scratch:\n\
  %sbytes = mul i64 %nd, 24\n\
  %sb1 = add i64 %sbytes, 8\n\
  %perm = call ptr @malloc(i64 %sb1)\n\
  %none = icmp eq ptr %perm, null\n\
  br i1 %none, label %oom, label %ready\n\
oom:\n\
  call void @__iris_dev_fail(i32 6, i32 0, i64 %sb1, i64 0, i64 %loc, i64 0)\n\
  ret ptr null\n\
ready:\n\
  %oshape = getelementptr inbounds i64, ptr %perm, i64 %nd\n\
  %stride = getelementptr inbounds i64, ptr %oshape, i64 %nd\n\
  br label %ph\n\
ph:\n\
  %d = phi i64 [ 0, %ready ], [ %d1, %pb ]\n\
  %pm = icmp slt i64 %d, %nd\n\
  br i1 %pm, label %pa, label %strides\n\
pa:\n\
  %rev0 = sub i64 %nd, 1\n\
  %rev = sub i64 %rev0, %d\n\
  %ap = getelementptr inbounds i64, ptr %axes, i64 %d\n\
  %av = load i64, ptr %ap\n\
  %ax = select i1 %given, i64 %av, i64 %rev\n\
  %axneg = icmp slt i64 %ax, 0\n\
  %axbig = icmp sge i64 %ax, %nd\n\
  %axbad = or i1 %axneg, %axbig\n\
  br i1 %axbad, label %axfail, label %pb\n\
axfail:\n\
  call void @free(ptr %perm)\n\
  call void @__iris_dev_fail(i32 2, i32 0, i64 %ax, i64 %nd, i64 %loc, i64 0)\n\
  ret ptr null\n\
pb:\n\
  %pp = getelementptr inbounds i64, ptr %perm, i64 %d\n\
  store i64 %ax, ptr %pp\n\
  %isp = getelementptr inbounds i64, ptr %shape, i64 %ax\n\
  %isv = load i64, ptr %isp\n\
  %osp = getelementptr inbounds i64, ptr %oshape, i64 %d\n\
  store i64 %isv, ptr %osp\n\
  %d1 = add i64 %d, 1\n\
  br label %ph\n\
strides:\n\
  br label %sh\n\
sh:\n\
  %s = phi i64 [ %nd, %strides ], [ %s0, %sbody ]\n\
  %acc = phi i64 [ 1, %strides ], [ %acc1, %sbody ]\n\
  %sm = icmp sgt i64 %s, 0\n\
  br i1 %sm, label %sbody, label %alloc\n\
sbody:\n\
  %s0 = sub i64 %s, 1\n\
  %stp = getelementptr inbounds i64, ptr %stride, i64 %s0\n\
  store i64 %acc, ptr %stp\n\
  %dimp = getelementptr inbounds i64, ptr %shape, i64 %s0\n\
  %dim = load i64, ptr %dimp\n\
  %acc1 = mul i64 %acc, %dim\n\
  br label %sh\n\
alloc:\n\
  %o = call ptr @__iris_dev_alloc(i32 %nd32, ptr %oshape)\n\
  %onone = icmp eq ptr %o, null\n\
  br i1 %onone, label %finish, label %go\n\
go:\n\
  %odp = getelementptr inbounds %dt, ptr %o, i32 0, i32 0\n\
  %odata = load ptr, ptr %odp\n\
  br label %h\n\
h:\n\
  %i = phi i64 [ 0, %go ], [ %i1, %emit ]\n\
  %more = icmp slt i64 %i, %numel\n\
  br i1 %more, label %dec, label %finish\n\
dec:\n\
  br label %dh\n\
dh:\n\
  %k = phi i64 [ %nd, %dec ], [ %k0, %db ]\n\
  %rem = phi i64 [ %i, %dec ], [ %rem1, %db ]\n\
  %src = phi i64 [ 0, %dec ], [ %src1, %db ]\n\
  %km = icmp sgt i64 %k, 0\n\
  br i1 %km, label %db, label %emit\n\
db:\n\
  %k0 = sub i64 %k, 1\n\
  %odimp = getelementptr inbounds i64, ptr %oshape, i64 %k0\n\
  %odim = load i64, ptr %odimp\n\
  %coord = urem i64 %rem, %odim\n\
  %rem1 = udiv i64 %rem, %odim\n\
  %pk = getelementptr inbounds i64, ptr %perm, i64 %k0\n\
  %axk = load i64, ptr %pk\n\
  %sk = getelementptr inbounds i64, ptr %stride, i64 %axk\n\
  %stk = load i64, ptr %sk\n\
  %part = mul i64 %coord, %stk\n\
  %src1 = add i64 %src, %part\n\
  br label %dh\n\
emit:\n\
  %xp = getelementptr inbounds float, ptr %data, i64 %src\n\
  %x = load float, ptr %xp\n\
  %yp = getelementptr inbounds float, ptr %odata, i64 %i\n\
  store float %x, ptr %yp\n\
  %i1 = add i64 %i, 1\n\
  br label %h\n\
finish:\n\
  call void @free(ptr %perm)\n\
  ret ptr %o\n\
}}\n\n",
            fields("%t", "")
        )
    });
}

/// `t` reduced (0 sum, 1 max, 2 mean, 3 min, 4 prod) over `axes` (all, with
/// none), the reduced axes kept as size 1 when `keep`.
fn helper_reduce(cx: &mut ModCtx) {
    cx.helper("__iris_dev_reduce", |_| {
        format!(
            "define internal ptr @__iris_dev_reduce(ptr %t, i32 %op, i32 %nax, ptr %axes, i1 %keep, i64 %loc) {{\n\
entry:\n\
{}\
  %nax64 = sext i32 %nax to i64\n\
  %sbytes = mul i64 %nd, 24\n\
  %sb1 = add i64 %sbytes, 8\n\
  %mask = call ptr @malloc(i64 %sb1)\n\
  %none = icmp eq ptr %mask, null\n\
  br i1 %none, label %oom, label %ready\n\
oom:\n\
  call void @__iris_dev_fail(i32 6, i32 0, i64 %sb1, i64 0, i64 %loc, i64 0)\n\
  ret ptr null\n\
ready:\n\
  %oshape = getelementptr inbounds i64, ptr %mask, i64 %nd\n\
  %ostride = getelementptr inbounds i64, ptr %oshape, i64 %nd\n\
  %all = icmp eq i32 %nax, 0\n\
  br label %mh\n\
mh:\n\
  %d = phi i64 [ 0, %ready ], [ %d1, %mb ]\n\
  %mm = icmp slt i64 %d, %nd\n\
  br i1 %mm, label %mb, label %axes_in\n\
mb:\n\
  %mp = getelementptr inbounds i64, ptr %mask, i64 %d\n\
  %mv = zext i1 %all to i64\n\
  store i64 %mv, ptr %mp\n\
  %d1 = add i64 %d, 1\n\
  br label %mh\n\
axes_in:\n\
  br label %ah\n\
ah:\n\
  %a = phi i64 [ 0, %axes_in ], [ %a1, %amark ]\n\
  %am = icmp slt i64 %a, %nax64\n\
  br i1 %am, label %ab, label %shape_out\n\
ab:\n\
  %ap = getelementptr inbounds i64, ptr %axes, i64 %a\n\
  %ax = load i64, ptr %ap\n\
  %axneg = icmp slt i64 %ax, 0\n\
  %axbig = icmp sge i64 %ax, %nd\n\
  %axbad = or i1 %axneg, %axbig\n\
  br i1 %axbad, label %axfail, label %amark\n\
axfail:\n\
  call void @free(ptr %mask)\n\
  call void @__iris_dev_fail(i32 2, i32 0, i64 %ax, i64 %nd, i64 %loc, i64 0)\n\
  ret ptr null\n\
amark:\n\
  %amp = getelementptr inbounds i64, ptr %mask, i64 %ax\n\
  store i64 1, ptr %amp\n\
  %a1 = add i64 %a, 1\n\
  br label %ah\n\
shape_out:\n\
  br label %oh\n\
oh:\n\
  %od = phi i64 [ 0, %shape_out ], [ %od1, %onext ]\n\
  %on = phi i64 [ 0, %shape_out ], [ %on1, %onext ]\n\
  %om = icmp slt i64 %od, %nd\n\
  br i1 %om, label %ob, label %ostrides\n\
ob:\n\
  %omp = getelementptr inbounds i64, ptr %mask, i64 %od\n\
  %omv = load i64, ptr %omp\n\
  %red = icmp ne i64 %omv, 0\n\
  %idp = getelementptr inbounds i64, ptr %shape, i64 %od\n\
  %idv = load i64, ptr %idp\n\
  %dimv = select i1 %red, i64 1, i64 %idv\n\
  %write = or i1 %keep, %red\n\
  %nwrite = xor i1 %red, true\n\
  %put = or i1 %keep, %nwrite\n\
  br i1 %put, label %oput, label %onext\n\
oput:\n\
  %opp = getelementptr inbounds i64, ptr %oshape, i64 %on\n\
  store i64 %dimv, ptr %opp\n\
  br label %onext\n\
onext:\n\
  %inc = zext i1 %put to i64\n\
  %on1 = add i64 %on, %inc\n\
  %od1 = add i64 %od, 1\n\
  br label %oh\n\
ostrides:\n\
  br label %sh\n\
sh:\n\
  %s = phi i64 [ %nd, %ostrides ], [ %s0, %sb ]\n\
  %sacc = phi i64 [ 1, %ostrides ], [ %sacc1, %sb ]\n\
  %sm = icmp sgt i64 %s, 0\n\
  br i1 %sm, label %sb, label %alloc\n\
sb:\n\
  %s0 = sub i64 %s, 1\n\
  %smp = getelementptr inbounds i64, ptr %mask, i64 %s0\n\
  %smv = load i64, ptr %smp\n\
  %sred = icmp ne i64 %smv, 0\n\
  %sdp = getelementptr inbounds i64, ptr %shape, i64 %s0\n\
  %sdv = load i64, ptr %sdp\n\
  %sstp = getelementptr inbounds i64, ptr %ostride, i64 %s0\n\
  %sval = select i1 %sred, i64 0, i64 %sacc\n\
  store i64 %sval, ptr %sstp\n\
  %smul = select i1 %sred, i64 1, i64 %sdv\n\
  %sacc1 = mul i64 %sacc, %smul\n\
  br label %sh\n\
alloc:\n\
  %on32 = trunc i64 %on to i32\n\
  %o = call ptr @__iris_dev_alloc(i32 %on32, ptr %oshape)\n\
  %onone = icmp eq ptr %o, null\n\
  br i1 %onone, label %finish, label %init\n\
init:\n\
{}\
  %isum = icmp eq i32 %op, 0\n\
  %imean = icmp eq i32 %op, 2\n\
  %imax = icmp eq i32 %op, 1\n\
  %imin = icmp eq i32 %op, 3\n\
  %izero = or i1 %isum, %imean\n\
  %iv1 = select i1 %imin, float 0x7FF0000000000000, float 1.0\n\
  %iv2 = select i1 %imax, float 0xFFF0000000000000, float %iv1\n\
  %iv = select i1 %izero, float 0.0, float %iv2\n\
  br label %ih\n\
ih:\n\
  %j = phi i64 [ 0, %init ], [ %j1, %ib ]\n\
  %jm = icmp slt i64 %j, %wnumel\n\
  br i1 %jm, label %ib, label %loop\n\
ib:\n\
  %jp = getelementptr inbounds float, ptr %wdata, i64 %j\n\
  store float %iv, ptr %jp\n\
  %j1 = add i64 %j, 1\n\
  br label %ih\n\
loop:\n\
  br label %h\n\
h:\n\
  %i = phi i64 [ 0, %loop ], [ %i1, %acc_in ]\n\
  %more = icmp slt i64 %i, %numel\n\
  br i1 %more, label %dec, label %mean\n\
dec:\n\
  br label %dh\n\
dh:\n\
  %k = phi i64 [ %nd, %dec ], [ %k0, %db ]\n\
  %rem = phi i64 [ %i, %dec ], [ %rem1, %db ]\n\
  %dst = phi i64 [ 0, %dec ], [ %dst1, %db ]\n\
  %km = icmp sgt i64 %k, 0\n\
  br i1 %km, label %db, label %acc_in\n\
db:\n\
  %k0 = sub i64 %k, 1\n\
  %kdp = getelementptr inbounds i64, ptr %shape, i64 %k0\n\
  %kdim = load i64, ptr %kdp\n\
  %coord = urem i64 %rem, %kdim\n\
  %rem1 = udiv i64 %rem, %kdim\n\
  %ksp = getelementptr inbounds i64, ptr %ostride, i64 %k0\n\
  %kst = load i64, ptr %ksp\n\
  %part = mul i64 %coord, %kst\n\
  %dst1 = add i64 %dst, %part\n\
  br label %dh\n\
acc_in:\n\
  %xp = getelementptr inbounds float, ptr %data, i64 %i\n\
  %x = load float, ptr %xp\n\
  %yp = getelementptr inbounds float, ptr %wdata, i64 %dst\n\
  %y = load float, ptr %yp\n\
  %vsum = fadd float %y, %x\n\
  %gt = fcmp ogt float %x, %y\n\
  %vmax = select i1 %gt, float %x, float %y\n\
  %lt = fcmp olt float %x, %y\n\
  %vmin = select i1 %lt, float %x, float %y\n\
  %vprod = fmul float %y, %x\n\
  %r1 = select i1 %imin, float %vmin, float %vprod\n\
  %r2 = select i1 %imax, float %vmax, float %r1\n\
  %r = select i1 %izero, float %vsum, float %r2\n\
  store float %r, ptr %yp\n\
  %i1 = add i64 %i, 1\n\
  br label %h\n\
mean:\n\
  br i1 %imean, label %mdiv, label %finish\n\
mdiv:\n\
  %cnt0 = udiv i64 %numel, %wnumel\n\
  %cnt = sitofp i64 %cnt0 to float\n\
  br label %mvh\n\
mvh:\n\
  %q = phi i64 [ 0, %mdiv ], [ %q1, %mvb ]\n\
  %qm = icmp slt i64 %q, %wnumel\n\
  br i1 %qm, label %mvb, label %finish\n\
mvb:\n\
  %qp = getelementptr inbounds float, ptr %wdata, i64 %q\n\
  %qv = load float, ptr %qp\n\
  %qd = fdiv float %qv, %cnt\n\
  store float %qd, ptr %qp\n\
  %q1 = add i64 %q, 1\n\
  br label %mvh\n\
finish:\n\
  call void @free(ptr %mask)\n\
  ret ptr %o\n\
}}\n\n",
            fields("%t", ""),
            fields("%o", "w")
        )
    });
}

/// `einsum(notation, ...)`: loops over every subscript, generated for the
/// notation; sizes come from the operands at run time, and must agree.
fn helper_einsum(cx: &mut ModCtx, notation: &str) -> Result<String, CodegenError> {
    if let Some(name) = cx.einsums.get(notation) {
        return Ok(name.clone());
    }
    let (inputs, outsub) =
        parse_einsum(notation).ok_or_else(|| err(format!("einsum notation `{notation}`")))?;
    let name = format!("__iris_dev_einsum_{}", cx.einsums.len());
    cx.einsums.insert(notation.to_owned(), name.clone());
    // Every distinct subscript: the output's first, then the summed ones.
    let mut labels: Vec<char> = outsub.clone();
    for s in &inputs {
        for c in s {
            if !labels.contains(c) {
                labels.push(*c);
            }
        }
    }
    let mut o = String::new();
    let params: Vec<String> = (0..inputs.len()).map(|k| format!("ptr %in{k}")).collect();
    let _ = writeln!(
        o,
        "define internal ptr @{name}({}, i64 %loc) {{\nentry:",
        params.join(", ")
    );
    for (k, sub) in inputs.iter().enumerate() {
        o.push_str(&fields(&format!("%in{k}"), &format!("x{k}")));
        // Each operand's rank must match its subscripts.
        let _ = writeln!(o, "  %rk{k} = icmp eq i64 %x{k}nd, {}", sub.len());
        let _ = writeln!(o, "  br i1 %rk{k}, label %rankok{k}, label %rankbad{k}");
        let _ = writeln!(o, "rankbad{k}:");
        let _ = writeln!(
            o,
            "  call void @__iris_dev_fail(i32 2, i32 0, i64 {}, i64 %x{k}nd, i64 %loc, i64 0)",
            sub.len()
        );
        let _ = writeln!(o, "  ret ptr null");
        let _ = writeln!(o, "rankok{k}:");
    }
    // The size of each subscript, from its first occurrence; later ones must agree.
    let mut size_of: HashMap<char, String> = HashMap::new();
    let mut check = 0;
    for (k, s) in inputs.iter().enumerate() {
        for (pos, c) in s.iter().enumerate() {
            let v = format!("%sz{k}_{pos}");
            let _ = writeln!(
                o,
                "  %szp{k}_{pos} = getelementptr inbounds i64, ptr %x{k}shape, i64 {pos}"
            );
            let _ = writeln!(o, "  {v} = load i64, ptr %szp{k}_{pos}");
            match size_of.get(c) {
                None => {
                    size_of.insert(*c, v);
                }
                Some(first) => {
                    let _ = writeln!(o, "  %agree{check} = icmp eq i64 {first}, {v}");
                    let _ = writeln!(
                        o,
                        "  br i1 %agree{check}, label %agreeok{check}, label %agreebad{check}"
                    );
                    let _ = writeln!(o, "agreebad{check}:");
                    let _ = writeln!(o, "  call void @__iris_dev_fail(i32 7, i32 0, i64 {first}, i64 {v}, i64 %loc, i64 0)");
                    let _ = writeln!(o, "  ret ptr null");
                    let _ = writeln!(o, "agreeok{check}:");
                    check += 1;
                }
            }
        }
    }
    // Output shape, allocated and zeroed.
    let _ = writeln!(o, "  %oshape = alloca [{} x i64]", outsub.len().max(1));
    for (d, c) in outsub.iter().enumerate() {
        let _ = writeln!(
            o,
            "  %osp{d} = getelementptr inbounds [{} x i64], ptr %oshape, i64 0, i64 {d}",
            outsub.len().max(1)
        );
        let _ = writeln!(o, "  store i64 {}, ptr %osp{d}", size_of[c]);
    }
    let _ = writeln!(
        o,
        "  %res = call ptr @__iris_dev_alloc(i32 {}, ptr %oshape)",
        outsub.len()
    );
    let _ = writeln!(o, "  %rnone = icmp eq ptr %res, null");
    let _ = writeln!(o, "  br i1 %rnone, label %done, label %zero");
    let _ = writeln!(o, "zero:");
    o.push_str(&fields("%res", "r"));
    let _ = writeln!(o, "  br label %zh");
    let _ = writeln!(o, "zh:");
    let _ = writeln!(o, "  %zi = phi i64 [ 0, %zero ], [ %zi1, %zb ]");
    let _ = writeln!(o, "  %zm = icmp slt i64 %zi, %rnumel");
    let _ = writeln!(o, "  br i1 %zm, label %zb, label %loops");
    let _ = writeln!(o, "zb:");
    let _ = writeln!(
        o,
        "  %zp = getelementptr inbounds float, ptr %rdata, i64 %zi"
    );
    let _ = writeln!(o, "  store float 0.0, ptr %zp");
    let _ = writeln!(o, "  %zi1 = add i64 %zi, 1");
    let _ = writeln!(o, "  br label %zh");
    let _ = writeln!(o, "loops:");
    // Nested loops, one per subscript.
    let l = labels.len();
    let _ = writeln!(o, "  br label %h0");
    for (n, c) in labels.iter().enumerate() {
        let from = if n == 0 {
            "loops".to_owned()
        } else {
            format!("b{}", n - 1)
        };
        let _ = writeln!(o, "h{n}:");
        let _ = writeln!(o, "  %i{n} = phi i64 [ 0, %{from} ], [ %i{n}n, %l{n} ]");
        let _ = writeln!(o, "  %m{n} = icmp slt i64 %i{n}, {}", size_of[c]);
        let exit = if n == 0 {
            "done".to_owned()
        } else {
            format!("l{}", n - 1)
        };
        let _ = writeln!(o, "  br i1 %m{n}, label %b{n}, label %{exit}");
        let _ = writeln!(o, "b{n}:");
        if n + 1 < l {
            let _ = writeln!(o, "  br label %h{}", n + 1);
        }
    }
    // Innermost: offsets from the loop indices, row-major.
    let offset = |o: &mut String, subs: &[char], pre: &str| -> String {
        let mut acc = "0".to_owned();
        for (pos, c) in subs.iter().enumerate() {
            let idx = labels.iter().position(|x| x == c).unwrap();
            let dimv = &size_of[c];
            let m = format!("%{pre}m{pos}");
            let a = format!("%{pre}a{pos}");
            let _ = writeln!(o, "  {m} = mul i64 {acc}, {dimv}");
            let _ = writeln!(o, "  {a} = add i64 {m}, %i{idx}");
            acc = a;
        }
        acc
    };
    let mut prod = "1.0".to_owned();
    for (k, s) in inputs.iter().enumerate() {
        let off = offset(&mut o, s, &format!("o{k}"));
        let _ = writeln!(
            o,
            "  %ep{k} = getelementptr inbounds float, ptr %x{k}data, i64 {off}"
        );
        let _ = writeln!(o, "  %ev{k} = load float, ptr %ep{k}");
        let _ = writeln!(o, "  %pr{k} = fmul float {prod}, %ev{k}");
        prod = format!("%pr{k}");
    }
    let roff = offset(&mut o, &outsub, "or");
    let _ = writeln!(
        o,
        "  %rp = getelementptr inbounds float, ptr %rdata, i64 {roff}"
    );
    let _ = writeln!(o, "  %rv = load float, ptr %rp");
    let _ = writeln!(o, "  %rs = fadd float %rv, {prod}");
    let _ = writeln!(o, "  store float %rs, ptr %rp");
    let _ = writeln!(o, "  br label %l{}", l - 1);
    for n in (0..l).rev() {
        let _ = writeln!(o, "l{n}:");
        let _ = writeln!(o, "  %i{n}n = add i64 %i{n}, 1");
        let _ = writeln!(o, "  br label %h{n}");
    }
    let _ = writeln!(o, "done:");
    let _ = writeln!(o, "  ret ptr %res");
    let _ = writeln!(o, "}}\n");
    let text = o;
    cx.helpers.insert(name.clone(), text);
    Ok(name)
}

/// `iris_pow_i64`: by squaring, wrapping; a negative exponent gives 0.
fn helper_pow(cx: &mut ModCtx) {
    cx.helper("__iris_dev_pow_i64", |_| {
        "define internal i64 @__iris_dev_pow_i64(i64 %base, i64 %exp) {\n\
entry:\n\
  %neg = icmp slt i64 %exp, 0\n\
  br i1 %neg, label %zero, label %h\n\
zero:\n\
  ret i64 0\n\
h:\n\
  %r = phi i64 [ 1, %entry ], [ %r1, %b ]\n\
  %bs = phi i64 [ %base, %entry ], [ %bs1, %b ]\n\
  %e = phi i64 [ %exp, %entry ], [ %e1, %b ]\n\
  %more = icmp sgt i64 %e, 0\n\
  br i1 %more, label %b, label %done\n\
b:\n\
  %odd = and i64 %e, 1\n\
  %isodd = icmp ne i64 %odd, 0\n\
  %rm = mul i64 %r, %bs\n\
  %r1 = select i1 %isodd, i64 %rm, i64 %r\n\
  %bs1 = mul i64 %bs, %bs\n\
  %e1 = ashr i64 %e, 1\n\
  br label %h\n\
done:\n\
  ret i64 %r\n\
}\n\n"
            .to_owned()
    });
}

// ---------------------------------------------------------------------------
// Offloading `par for` loops (`--gpu cuda`)
// ---------------------------------------------------------------------------

static OFFLOAD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--gpu cuda`: native builds of this process run device-code `par for`
/// loops on the GPU.
pub fn set_offload(on: bool) {
    OFFLOAD.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn offloading() -> bool {
    OFFLOAD.load(std::sync::atomic::Ordering::Relaxed)
}

fn c_bytes(s: &[u8]) -> String {
    let mut out = String::new();
    for b in s {
        match b {
            b'"' | b'\\' => {
                let _ = write!(out, "\\{b:02X}");
            }
            0x20..=0x7E => out.push(*b as char),
            other => {
                let _ = write!(out, "\\{other:02X}");
            }
        }
    }
    out
}

/// `base` (host LLVM IR for `module`) with each device-code `par for` launched
/// on the GPU through `iris_cuda_launch`, and the PTX embedded.
pub fn offload(module: &IrModule, base: &str) -> Result<String, CodegenError> {
    let device = device_module(module, DeviceTarget::Nvptx)?;
    for f in module.functions() {
        for instr in f.blocks().iter().flat_map(|b| &b.instrs) {
            if let IrInstr::ParFor { body_fn, .. } = instr {
                if let Some((_, why)) = device.host_only.iter().find(|(n, _)| n == body_fn) {
                    eprintln!(
                        "note: the `par for` in `{}` runs on the CPU: its body {why}",
                        f.name
                    );
                }
            }
        }
    }
    if device.kernels.is_empty() {
        return Ok(base.to_owned());
    }
    let ptx = compile_ptx(&device)?;
    let index: HashMap<&str, usize> = device
        .kernels
        .iter()
        .enumerate()
        .map(|(i, k)| (k.body.as_str(), i))
        .collect();

    let mut out = String::with_capacity(base.len() + ptx.len() * 2);
    for line in base.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("call void @iris_par_for(ptr @") {
            let name = rest.split(',').next().unwrap_or("");
            if let Some(k) = index.get(name) {
                let indent = &line[..line.len() - trimmed.len()];
                writeln!(
                    out,
                    "{indent}call void @iris_cuda_launch(ptr @__iris_cuda_ptx, ptr @__iris_cuda_kernel_{k}, ptr @__iris_cuda_sig_{k}, ptr @__iris_cuda_panics, i32 {},{}",
                    device.panics.len(),
                    &rest[name.len() + 1..]
                )?;
                continue;
            }
        }
        writeln!(out, "{line}")?;
    }
    writeln!(out)?;
    let mut ptx_bytes = ptx.into_bytes();
    ptx_bytes.push(0);
    writeln!(
        out,
        "@__iris_cuda_ptx = private unnamed_addr constant [{} x i8] c\"{}\"",
        ptx_bytes.len(),
        c_bytes(&ptx_bytes)
    )?;
    for (k, info) in device.kernels.iter().enumerate() {
        writeln!(
            out,
            "@__iris_cuda_kernel_{k} = private unnamed_addr constant [{} x i8] c\"{}\\00\"",
            info.kernel.len() + 1,
            c_bytes(info.kernel.as_bytes())
        )?;
        writeln!(
            out,
            "@__iris_cuda_sig_{k} = private unnamed_addr constant [{} x i8] c\"{}\\00\"",
            info.captures.len() + 1,
            info.captures
        )?;
    }
    let mut refs = Vec::new();
    for (i, p) in device.panics.iter().enumerate() {
        writeln!(
            out,
            "@__iris_cuda_panic_{i} = private unnamed_addr constant [{} x i8] c\"{}\\00\"",
            p.len() + 1,
            c_bytes(p.as_bytes())
        )?;
        refs.push(format!("ptr @__iris_cuda_panic_{i}"));
    }
    writeln!(
        out,
        "@__iris_cuda_panics = private unnamed_addr constant [{} x ptr] [{}]",
        refs.len().max(1),
        if refs.is_empty() {
            "ptr null".into()
        } else {
            refs.join(", ")
        }
    )?;
    if !base.contains("@iris_cuda_launch(") {
        writeln!(
            out,
            "declare void @iris_cuda_launch(ptr, ptr, ptr, ptr, i32, i64, i64, ptr)"
        )?;
    }
    Ok(out)
}

/// The device code of `module` built for the host CPU (`DeviceTarget::HostTest`)
/// into an executable with `harness`, C source whose `main` calls the
/// kernels -- each runs its whole range in turn -- and reads
/// `__iris_dev_error`. For testing device code where there is no GPU.
pub fn build_host_test(
    module: &IrModule,
    harness: &str,
    output: &std::path::Path,
) -> Result<(DeviceModule, PathBuf), CodegenError> {
    let device = device_module(module, DeviceTarget::HostTest)?;
    let built = super::build::build_native_artifact(
        &device.ir,
        &[("harness.c", harness)],
        &[],
        &[],
        &[],
        output,
        false,
    )?;
    Ok((device, built))
}
