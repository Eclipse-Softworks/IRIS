//! CPU SIMD for IRIS.
//!
//! Applied to the LLVM IR the native backend emits (`simdize`), so the
//! result is the complete program, built like any other (`--simd`):
//!
//! 1. **Explicit vector kernels for `par for`.** A body that works
//!    element-wise -- it reads and writes tensors only at the loop index and
//!    otherwise computes on numbers -- gets a companion
//!    `{body}_simd(lo, hi, env)` that runs `VF` iterations at a time as
//!    `<VF x T>` operations, and the loop is dispatched through
//!    `iris_par_for_chunked`, which gives each thread one contiguous range.
//!
//!    The kernel keeps the scalar body's meaning exactly. Every check the
//!    scalar code makes -- tensor bounds, integer overflow, division by zero
//!    -- is made for the whole vector before anything is stored, and a
//!    vector that would fail runs its iterations through the scalar body
//!    instead, which stops with the error at the right one. Stores wait until
//!    the checks pass and are forwarded to later loads of the same tensor; a
//!    written tensor that shares its storage with another one the body reads
//!    runs scalar. The last `< VF` iterations of each range run the scalar
//!    body. A body that is not element-wise (it calls functions, branches,
//!    indexes at other positions) runs scalar, as it does without `--simd`.
//!
//! 2. **Loop-vectorizer hints** on every other loop: each back-edge carries
//!    its own `!llvm.loop` node enabling LLVM's vectorizer, with a width for
//!    the element type the function computes on.
//!
//! Functions are compiled for the target's vector ISA: AVX2 + FMA
//! (`x86-64-v3`) on x86-64, NEON on AArch64.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write;

use crate::codegen::llvm_ir::{emit_llvm_ir, llvm_type_complete};
use crate::error::CodegenError;
use crate::ir::function::IrFunction;
use crate::ir::instr::{BinOp, IrInstr, ScalarUnaryOp};
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};
use crate::ir::value::ValueId;

// ---------------------------------------------------------------------------
// Target vector ISA
// ---------------------------------------------------------------------------

/// The vector ISA a target triple gets.
#[derive(Debug, Clone, Copy)]
pub struct VectorIsa {
    /// `"target-cpu"`, when the ISA names one.
    pub cpu: Option<&'static str>,
    /// `"target-features"`.
    pub features: &'static str,
    /// Vector register width in bits.
    pub bits: usize,
    /// For the header comment.
    pub name: &'static str,
}

pub fn vector_isa(triple: &str) -> VectorIsa {
    if triple.starts_with("x86_64") {
        VectorIsa {
            cpu: Some("x86-64-v3"),
            features: "+avx2,+fma",
            bits: 256,
            name: "x86-64-v3 (AVX2 + FMA)",
        }
    } else if triple.starts_with("aarch64") || triple.starts_with("arm64") {
        VectorIsa {
            cpu: None,
            features: "+neon",
            bits: 128,
            name: "AArch64 (NEON)",
        }
    } else if triple.starts_with("wasm32") {
        VectorIsa {
            cpu: None,
            features: "+simd128",
            bits: 128,
            name: "WebAssembly SIMD128",
        }
    } else {
        VectorIsa {
            cpu: None,
            features: "",
            bits: 128,
            name: "generic (LLVM legalises the vectors)",
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--simd`: every native build of this process is vectorised.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// `--emit simd`: the module's LLVM IR, vectorised.
pub fn emit_simd(module: &IrModule) -> Result<String, CodegenError> {
    let ir = emit_llvm_ir(module)?;
    if enabled() {
        // `emit_llvm_ir` applied it already.
        return Ok(ir);
    }
    simdize(module, &ir)
}

/// Vectorise `base`, LLVM IR emitted for `module` by any of the native
/// backend's entry points (plain, binary, eval).
pub fn simdize(module: &IrModule, base: &str) -> Result<String, CodegenError> {
    let triple = base
        .lines()
        .find_map(|l| {
            l.strip_prefix("target triple = \"")
                .and_then(|r| r.strip_suffix('"'))
        })
        .unwrap_or("x86_64-pc-windows-gnu");
    let isa = vector_isa(triple);

    // The kernels, one per element-wise `par for` body.
    let mut kernels: Vec<Kernel> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for func in module.functions() {
        for block in func.blocks() {
            for instr in &block.instrs {
                let IrInstr::ParFor { body_fn, .. } = instr else {
                    continue;
                };
                if !seen.insert(body_fn.clone()) {
                    continue;
                }
                if let Some(body) = module.function_by_name(body_fn) {
                    if let Some(k) = vector_kernel(body, &isa)? {
                        kernels.push(k);
                    }
                }
            }
        }
    }
    let vectorised: HashSet<&str> = kernels.iter().map(|k| k.body.as_str()).collect();

    // Unnamed metadata numbers already taken.
    let mut next_md = base
        .lines()
        .filter_map(|l| l.strip_prefix('!'))
        .filter_map(|l| l.split(|c: char| !c.is_ascii_digit()).next())
        .filter_map(|n| n.parse::<u32>().ok())
        .max()
        .map_or(0, |m| m + 1);
    let mut metadata: Vec<String> = Vec::new();
    let mut width_md: HashMap<usize, u32> = HashMap::new();

    let mut out = String::new();
    writeln!(out, "; IRIS SIMD/Vectorization IR")?;
    writeln!(out, "; Target: {}", isa.name)?;
    writeln!(
        out,
        "; {} `par for` bodies as explicit vector kernels; loops elsewhere carry LLVM vectorizer width hints.\n",
        kernels.len()
    )?;

    let mut labels: HashSet<String> = HashSet::new();
    let mut width: Option<usize> = None;
    for line in base.lines() {
        if line.starts_with("define ") && line.trim_end().ends_with('{') {
            let head = line.trim_end().trim_end_matches('{').trim_end();
            writeln!(out, "{head} #0 {{")?;
            labels.clear();
            width = defined_name(line)
                .and_then(|n| {
                    module
                        .function_by_name(n.trim_start_matches(super::llvm_ir::USER_SYMBOL_PREFIX))
                })
                .and_then(|f| loop_width(f, &isa));
            continue;
        }
        if let Some(label) = line.strip_suffix(':') {
            if !label.starts_with(' ') && !label.starts_with(';') {
                labels.insert(label.to_owned());
            }
        }
        let trimmed = line.trim_start();
        // A width hint enables the vectorizer without forcing it, so a loop
        // it cannot vectorize (most with checked integer arithmetic, whose
        // overflow exits it does not handle) compiles without a warning.
        if let (true, Some(w)) = (
            trimmed.starts_with("br ") && !line.contains("!llvm.loop"),
            width,
        ) {
            let back_edge = line
                .split("label %")
                .skip(1)
                .map(|t| {
                    t.split(|c: char| c == ',' || c.is_whitespace())
                        .next()
                        .unwrap_or("")
                })
                .any(|t| labels.contains(t));
            if back_edge {
                let id = next_md;
                next_md += 1;
                let wid = *width_md.entry(w).or_insert_with(|| {
                    let n = next_md;
                    next_md += 1;
                    metadata.push(format!(
                        "!{n} = !{{!\"llvm.loop.vectorize.width\", i32 {w}}}"
                    ));
                    n
                });
                metadata.push(format!("!{id} = distinct !{{!{id}, !{wid}}}"));
                writeln!(out, "{line}, !llvm.loop !{id}")?;
                continue;
            }
        }
        if let Some(rest) = trimmed.strip_prefix("call void @iris_par_for(ptr @") {
            let name = rest.split(',').next().unwrap_or("");
            if vectorised.contains(name) {
                let indent = &line[..line.len() - trimmed.len()];
                writeln!(
                    out,
                    "{indent}call void @iris_par_for_chunked(ptr @{name}_simd,{}",
                    &rest[name.len() + 1..]
                )?;
                continue;
            }
        }
        writeln!(out, "{line}")?;
    }

    writeln!(out)?;
    writeln!(out, "attributes #0 = {{")?;
    if let Some(cpu) = isa.cpu {
        writeln!(out, "  \"target-cpu\"=\"{cpu}\"")?;
    }
    if !isa.features.is_empty() {
        writeln!(out, "  \"target-features\"=\"{}\"", isa.features)?;
    }
    writeln!(out, "}}")?;

    let mut decls: BTreeSet<String> = BTreeSet::new();
    for k in &kernels {
        writeln!(out)?;
        out.push_str(&k.text);
        decls.extend(k.decls.iter().cloned());
    }
    if !decls.is_empty() {
        writeln!(out)?;
    }
    for d in decls {
        let name = d
            .split('@')
            .nth(1)
            .and_then(|r| r.split('(').next())
            .unwrap_or("");
        if !base.contains(&format!("@{name}(")) {
            writeln!(out, "{d}")?;
        }
    }

    writeln!(out)?;
    for m in metadata {
        writeln!(out, "{m}")?;
    }
    Ok(out)
}

/// The name in a `define ... @name(` line.
fn defined_name(line: &str) -> Option<&str> {
    let rest = &line[line.find('@')? + 1..];
    let rest = rest.strip_prefix('"').unwrap_or(rest);
    rest.split(['(', '"']).next()
}

/// A vectorizer width for a function's loops: the lanes of its widest
/// arithmetic type, if it does arithmetic.
fn loop_width(func: &IrFunction, isa: &VectorIsa) -> Option<usize> {
    let mut widest = 0;
    for block in func.blocks() {
        for instr in &block.instrs {
            let ty = match instr {
                IrInstr::BinOp { ty, .. } | IrInstr::UnaryOp { ty, .. } => ty,
                _ => continue,
            };
            if let IrType::Scalar(d) = ty {
                if let Some(b) = lane_bits(*d) {
                    if *d != DType::Bool {
                        widest = widest.max(b);
                    }
                }
            }
        }
    }
    (widest > 0).then(|| (isa.bits / widest).max(2))
}

// ---------------------------------------------------------------------------
// Vector kernels
// ---------------------------------------------------------------------------

/// A vector kernel for one `par for` body.
struct Kernel {
    body: String,
    text: String,
    decls: BTreeSet<String>,
}

fn lane_bits(d: DType) -> Option<usize> {
    match d {
        DType::F64 | DType::I64 | DType::U64 | DType::USize => Some(64),
        DType::F32 | DType::I32 | DType::U32 => Some(32),
        DType::I8 | DType::U8 | DType::Bool => Some(8),
    }
}

fn is_float(d: DType) -> bool {
    matches!(d, DType::F32 | DType::F64)
}

fn is_unsigned(d: DType) -> bool {
    matches!(d, DType::U8 | DType::U32 | DType::U64 | DType::USize)
}

fn scalar_dtype(ty: &IrType) -> Option<DType> {
    match ty {
        IrType::Scalar(d) if lane_bits(*d).is_some() => Some(*d),
        _ => None,
    }
}

/// How a body uses one of its captures.
#[derive(Clone, Copy, PartialEq)]
enum CaptureUse {
    /// A tensor read (and, if `written`, written) at the loop index.
    Tensor { written: bool },
    /// A number used as an operand.
    Scalar(DType),
}

/// What a body needs, if it is element-wise.
struct Plan {
    vf: usize,
    /// Capture slot (body param index - 1) to its use.
    captures: Vec<(usize, CaptureUse)>,
    /// The loop variable is used as a number, not only as the index.
    lanes: bool,
}

/// The walk over a body that decides whether it is element-wise.
struct Scan<'f> {
    func: &'f IrFunction,
    loop_var: ValueId,
    /// Capture param to its slot in the closure environment.
    param_slot: HashMap<ValueId, usize>,
    captures: HashMap<usize, CaptureUse>,
    lanes: bool,
    widest: usize,
    defined: HashSet<ValueId>,
    /// Constants, whose lane width counts only once a computation uses them
    /// (a body's `return 0` does not make its lanes 64-bit).
    consts: HashMap<ValueId, IrType>,
}

impl Scan<'_> {
    /// An operand: the loop variable, a number captured, or a value the
    /// body computed.
    fn operand(&mut self, v: ValueId) -> Option<()> {
        if v == self.loop_var {
            self.lanes = true;
            self.widest = self.widest.max(64);
            return Some(());
        }
        if let Some(&slot) = self.param_slot.get(&v) {
            let d = scalar_dtype(&self.func.params[slot + 1].ty)?;
            if matches!(self.captures.get(&slot), Some(CaptureUse::Tensor { .. })) {
                return None;
            }
            self.captures.insert(slot, CaptureUse::Scalar(d));
            self.widest = self.widest.max(lane_bits(d)?);
            return Some(());
        }
        if let Some(ty) = self.consts.get(&v).cloned() {
            self.width(&ty)?;
        }
        self.defined.contains(&v).then_some(())
    }

    /// The slot of a tensor capture indexed at the loop variable.
    fn tensor(&mut self, t: ValueId, indices: &[ValueId], write: bool) -> Option<usize> {
        if indices != [self.loop_var] {
            return None;
        }
        let slot = *self.param_slot.get(&t)?;
        if !matches!(self.func.params[slot + 1].ty, IrType::Tensor { .. }) {
            return None;
        }
        let written = match self.captures.get(&slot) {
            Some(CaptureUse::Scalar(_)) => return None,
            Some(CaptureUse::Tensor { written }) => *written || write,
            None => write,
        };
        self.captures.insert(slot, CaptureUse::Tensor { written });
        Some(slot)
    }

    fn width(&mut self, ty: &IrType) -> Option<DType> {
        let d = scalar_dtype(ty)?;
        self.widest = self.widest.max(lane_bits(d)?);
        Some(d)
    }
}

/// The value ids of a function's arguments: its entry block's params.
fn arg_ids(func: &IrFunction) -> Vec<ValueId> {
    func.blocks()
        .first()
        .map(|b| b.params.iter().map(|p| p.id).collect())
        .unwrap_or_default()
}

fn plan(func: &IrFunction, isa: &VectorIsa) -> Option<Plan> {
    let [block] = func.blocks() else { return None };
    let args = arg_ids(func);
    if !matches!(block.instrs.last(), Some(IrInstr::Return { .. }))
        || args.is_empty()
        || args.len() != func.params.len()
    {
        return None;
    }
    let mut scan = Scan {
        func,
        loop_var: args[0],
        param_slot: args
            .iter()
            .enumerate()
            .skip(1)
            .map(|(i, p)| (*p, i - 1))
            .collect(),
        captures: HashMap::new(),
        lanes: false,
        widest: 0,
        defined: HashSet::new(),
        consts: HashMap::new(),
    };
    let ty_of = |v: ValueId| func.value_type(v).and_then(scalar_dtype);
    for instr in &block.instrs {
        match instr {
            IrInstr::ConstInt { result, ty, .. } | IrInstr::ConstFloat { result, ty, .. } => {
                scalar_dtype(ty)?;
                scan.consts.insert(*result, ty.clone());
                scan.defined.insert(*result);
            }
            IrInstr::ConstBool { result, .. } => {
                scan.defined.insert(*result);
            }
            IrInstr::Load {
                result,
                tensor,
                indices,
                result_ty,
            } => {
                scan.tensor(*tensor, indices, false)?;
                scan.width(result_ty)?;
                scan.defined.insert(*result);
            }
            IrInstr::Store {
                tensor,
                indices,
                value,
            } => {
                scan.operand(*value)?;
                ty_of(*value)?;
                scan.tensor(*tensor, indices, true)?;
            }
            IrInstr::BinOp {
                result,
                op,
                lhs,
                rhs,
                ty,
            } => {
                scan.operand(*lhs)?;
                scan.operand(*rhs)?;
                let (a, b) = (ty_of(*lhs)?, ty_of(*rhs)?);
                if a != b || !binop_supported(*op, a) {
                    return None;
                }
                scan.width(ty)?;
                scan.defined.insert(*result);
            }
            IrInstr::UnaryOp {
                result,
                op,
                operand,
                ty,
            } => {
                scan.operand(*operand)?;
                let d = ty_of(*operand)?;
                if d != scan.width(ty)? || !unary_supported(*op, d) {
                    return None;
                }
                scan.defined.insert(*result);
            }
            IrInstr::Cast {
                result,
                operand,
                from_ty,
                to_ty,
            } => {
                scan.operand(*operand)?;
                let from = scan.width(from_ty)?;
                scan.width(to_ty)?;
                if ty_of(*operand) != Some(from) {
                    return None;
                }
                scan.defined.insert(*result);
            }
            IrInstr::Return { .. } => {}
            _ => return None,
        }
    }
    let widest = scan.widest.max(32);
    let vf = (isa.bits / widest).clamp(2, 16);
    let mut captures: Vec<(usize, CaptureUse)> = scan.captures.into_iter().collect();
    captures.sort_by_key(|(slot, _)| *slot);
    Some(Plan {
        vf,
        captures,
        lanes: scan.lanes,
    })
}

fn binop_supported(op: BinOp, d: DType) -> bool {
    if d == DType::Bool {
        return matches!(
            op,
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::CmpEq | BinOp::CmpNe
        );
    }
    if is_float(d) {
        return !matches!(
            op,
            BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
                | BinOp::Shl
                | BinOp::Shr
                | BinOp::WrapAdd
                | BinOp::WrapSub
                | BinOp::WrapMul
        );
    }
    true
}

fn unary_supported(op: ScalarUnaryOp, d: DType) -> bool {
    match op {
        ScalarUnaryOp::Not => d == DType::Bool,
        ScalarUnaryOp::BitNot => !is_float(d) && d != DType::Bool,
        ScalarUnaryOp::Neg | ScalarUnaryOp::Abs | ScalarUnaryOp::Sign => d != DType::Bool,
        _ => is_float(d),
    }
}

/// Emits a kernel. Names: `%k{n}` temporaries, `%v{n}` blocks.
struct Emit<'a> {
    func: &'a IrFunction,
    vf: usize,
    text: String,
    tmp: u32,
    block: u32,
    decls: BTreeSet<String>,
    /// Vector values: name and lane type.
    vals: HashMap<ValueId, (String, DType)>,
    /// Capture slot to its data pointer.
    data: HashMap<usize, String>,
    /// Capture slot to the `<VF x float>` last stored to it this iteration.
    pending: HashMap<usize, String>,
    /// The stores, in program order: slot and `<VF x float>`.
    stores: Vec<(usize, String)>,
}

impl<'a> Emit<'a> {
    fn t(&mut self) -> String {
        self.tmp += 1;
        format!("%k{}", self.tmp)
    }

    fn line(&mut self, s: impl AsRef<str>) {
        self.text.push_str("  ");
        self.text.push_str(s.as_ref());
        self.text.push('\n');
    }

    fn scalar(d: DType) -> String {
        llvm_type_complete(&IrType::Scalar(d)).unwrap_or_else(|_| "i64".into())
    }

    fn vty(&self, d: DType) -> String {
        format!("<{} x {}>", self.vf, Self::scalar(d))
    }

    /// LLVM's name for the vector type in an intrinsic: `v8f32`, `v4i64`.
    fn vsfx(&self, d: DType) -> String {
        let s = match d {
            DType::F32 => "f32".to_owned(),
            DType::F64 => "f64".to_owned(),
            DType::Bool => "i1".to_owned(),
            other => Self::scalar(other),
        };
        format!("v{}{}", self.vf, s)
    }

    /// A vector constant with every lane `c` (an LLVM scalar literal).
    fn splat_const(&self, d: DType, c: &str) -> String {
        let s = Self::scalar(d);
        let lanes: Vec<String> = (0..self.vf).map(|_| format!("{s} {c}")).collect();
        format!("<{}>", lanes.join(", "))
    }

    /// A vector of a scalar value in every lane.
    fn splat(&mut self, d: DType, v: &str) -> String {
        let (vt, s) = (self.vty(d), Self::scalar(d));
        let ins = self.t();
        let out = self.t();
        self.line(format!("{ins} = insertelement {vt} poison, {s} {v}, i64 0"));
        self.line(format!(
            "{out} = shufflevector {vt} {ins}, {vt} poison, <{} x i32> zeroinitializer",
            self.vf
        ));
        out
    }

    fn declare(&mut self, d: String) {
        self.decls.insert(d);
    }

    /// Call a vector intrinsic of `args.len()` operands of lane type `d`.
    fn intrinsic(&mut self, name: &str, d: DType, args: &[&str]) -> String {
        let vt = self.vty(d);
        let full = format!("llvm.{name}.{}", self.vsfx(d));
        let params = vec![vt.clone(); args.len()].join(", ");
        self.declare(format!("declare {vt} @{full}({params})"));
        let r = self.t();
        let a: Vec<String> = args.iter().map(|x| format!("{vt} {x}")).collect();
        self.line(format!("{r} = call {vt} @{full}({})", a.join(", ")));
        r
    }

    /// Any lane of an `<VF x i1>` set.
    fn any(&mut self, mask: &str) -> String {
        let full = format!("llvm.vector.reduce.or.v{}i1", self.vf);
        self.declare(format!("declare i1 @{full}(<{} x i1>)", self.vf));
        let r = self.t();
        self.line(format!("{r} = call i1 @{full}(<{} x i1> {mask})", self.vf));
        r
    }

    /// End the current block: to the scalar path when `fail`, on otherwise.
    fn bail_if(&mut self, fail: &str) {
        self.block += 1;
        let next = self.block;
        self.line(format!("br i1 {fail}, label %scalar, label %v{next}"));
        self.text.push_str(&format!("v{next}:\n"));
    }

    /// A scalar function applied lane by lane: `f(T) -> T` or `f(T, T) -> T`.
    fn per_lane(&mut self, f: &str, d: DType, args: &[&str]) -> String {
        let (vt, s) = (self.vty(d), Self::scalar(d));
        let mut acc = "poison".to_owned();
        for lane in 0..self.vf {
            let mut call_args = Vec::new();
            for a in args {
                let e = self.t();
                self.line(format!("{e} = extractelement {vt} {a}, i64 {lane}"));
                call_args.push(format!("{s} {e}"));
            }
            let c = self.t();
            self.line(format!("{c} = call {s} @{f}({})", call_args.join(", ")));
            let next = self.t();
            self.line(format!(
                "{next} = insertelement {vt} {acc}, {s} {c}, i64 {lane}"
            ));
            acc = next;
        }
        acc
    }

    fn val(&self, v: ValueId) -> Result<(String, DType), CodegenError> {
        self.vals
            .get(&v)
            .cloned()
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "simd".into(),
                detail: format!(
                    "value %{} of `{}` used before it is defined",
                    v.0, self.func.name
                ),
            })
    }

    /// `<VF x float>` storage to the element type `d`, as the scalar code
    /// converts `iris_tensor_get_at`'s double.
    fn convert_from_storage(&mut self, f: &str, d: DType) -> String {
        if d == DType::F32 {
            return f.to_owned();
        }
        let wide = self.t();
        self.line(format!(
            "{wide} = fpext <{} x float> {f} to <{} x double>",
            self.vf, self.vf
        ));
        if d == DType::F64 {
            return wide;
        }
        let r = self.t();
        let conv = if is_unsigned(d) || d == DType::Bool {
            "fptoui"
        } else {
            "fptosi"
        };
        let vt = self.vty(d);
        self.line(format!(
            "{r} = {conv} <{} x double> {wide} to {vt}",
            self.vf
        ));
        r
    }

    /// An element-type vector to `<VF x float>` storage, as the scalar code
    /// widens to double and the runtime narrows to float.
    fn convert_to_storage(&mut self, v: &str, d: DType) -> String {
        if d == DType::F32 {
            return v.to_owned();
        }
        let wide = if d == DType::F64 {
            v.to_owned()
        } else {
            let w = self.t();
            let conv = if is_unsigned(d) || d == DType::Bool {
                "uitofp"
            } else {
                "sitofp"
            };
            let vt = self.vty(d);
            self.line(format!("{w} = {conv} {vt} {v} to <{} x double>", self.vf));
            w
        };
        let r = self.t();
        self.line(format!(
            "{r} = fptrunc <{} x double> {wide} to <{} x float>",
            self.vf, self.vf
        ));
        r
    }

    fn binop(
        &mut self,
        op: BinOp,
        d: DType,
        a: &str,
        b: &str,
    ) -> Result<(String, DType), CodegenError> {
        let vt = self.vty(d);
        let s = Self::scalar(d);
        let unsigned = is_unsigned(d);
        let checked = s == "i64" || s == "i32";
        let bits = lane_bits(d).unwrap_or(64);
        let min = if s == "i64" {
            "-9223372036854775808"
        } else {
            "-2147483648"
        };
        let r = |e: &mut Self, inst: String| {
            let r = e.t();
            e.line(format!("{r} = {inst}"));
            r
        };
        let cmp = |e: &mut Self, pred: &str| {
            let k = if is_float(d) { "fcmp" } else { "icmp" };
            let x = e.t();
            e.line(format!("{x} = {k} {pred} {vt} {a}, {b}"));
            (x, DType::Bool)
        };
        Ok(match op {
            BinOp::CmpEq => cmp(self, if is_float(d) { "oeq" } else { "eq" }),
            BinOp::CmpNe => cmp(self, if is_float(d) { "une" } else { "ne" }),
            BinOp::CmpLt => cmp(
                self,
                if is_float(d) {
                    "olt"
                } else if unsigned {
                    "ult"
                } else {
                    "slt"
                },
            ),
            BinOp::CmpLe => cmp(
                self,
                if is_float(d) {
                    "ole"
                } else if unsigned {
                    "ule"
                } else {
                    "sle"
                },
            ),
            BinOp::CmpGt => cmp(
                self,
                if is_float(d) {
                    "ogt"
                } else if unsigned {
                    "ugt"
                } else {
                    "sgt"
                },
            ),
            BinOp::CmpGe => cmp(
                self,
                if is_float(d) {
                    "oge"
                } else if unsigned {
                    "uge"
                } else {
                    "sge"
                },
            ),
            _ if is_float(d) => {
                let v = match op {
                    BinOp::Add => r(self, format!("fadd {vt} {a}, {b}")),
                    BinOp::Sub => r(self, format!("fsub {vt} {a}, {b}")),
                    BinOp::Mul => r(self, format!("fmul {vt} {a}, {b}")),
                    BinOp::Div => r(self, format!("fdiv {vt} {a}, {b}")),
                    BinOp::Mod => r(self, format!("frem {vt} {a}, {b}")),
                    BinOp::FloorDiv => {
                        let q = r(self, format!("fdiv {vt} {a}, {b}"));
                        self.intrinsic("floor", d, &[&q])
                    }
                    BinOp::Pow => self.intrinsic("pow", d, &[a, b]),
                    BinOp::Min => self.intrinsic("minnum", d, &[a, b]),
                    BinOp::Max => self.intrinsic("maxnum", d, &[a, b]),
                    other => return Err(unsupported(self.func, format!("`{other:?}` on `{s}`"))),
                };
                (v, d)
            }
            BinOp::BitAnd => (r(self, format!("and {vt} {a}, {b}")), d),
            BinOp::BitOr => (r(self, format!("or {vt} {a}, {b}")), d),
            BinOp::BitXor => (r(self, format!("xor {vt} {a}, {b}")), d),
            BinOp::WrapAdd => (r(self, format!("add {vt} {a}, {b}")), d),
            BinOp::WrapSub => (r(self, format!("sub {vt} {a}, {b}")), d),
            BinOp::WrapMul => (r(self, format!("mul {vt} {a}, {b}")), d),
            BinOp::Add | BinOp::Sub | BinOp::Mul if checked => {
                let name = match op {
                    BinOp::Add => "sadd",
                    BinOp::Sub => "ssub",
                    _ => "smul",
                };
                let full = format!("llvm.{name}.with.overflow.{}", self.vsfx(d));
                let pair = format!("{{ {vt}, <{} x i1> }}", self.vf);
                self.declare(format!("declare {pair} @{full}({vt}, {vt})"));
                let res = r(self, format!("call {pair} @{full}({vt} {a}, {vt} {b})"));
                let ov = r(self, format!("extractvalue {pair} {res}, 1"));
                let fail = self.any(&ov);
                self.bail_if(&fail);
                (r(self, format!("extractvalue {pair} {res}, 0")), d)
            }
            BinOp::Add => (r(self, format!("add {vt} {a}, {b}")), d),
            BinOp::Sub => (r(self, format!("sub {vt} {a}, {b}")), d),
            BinOp::Mul => (r(self, format!("mul {vt} {a}, {b}")), d),
            BinOp::Div | BinOp::FloorDiv | BinOp::Mod if checked => {
                let zero = self.splat_const(d, "0");
                let z = r(self, format!("icmp eq {vt} {b}, {zero}"));
                let minv = self.splat_const(d, min);
                let neg1 = self.splat_const(d, "-1");
                let m = r(self, format!("icmp eq {vt} {a}, {minv}"));
                let n = r(self, format!("icmp eq {vt} {b}, {neg1}"));
                let edge = r(self, format!("and <{} x i1> {m}, {n}", self.vf));
                // MIN % -1 is 0, not an overflow; MIN / -1 and MIN // -1 are.
                let bad = if op == BinOp::Mod {
                    z.clone()
                } else {
                    r(self, format!("or <{} x i1> {z}, {edge}", self.vf))
                };
                let fail = self.any(&bad);
                self.bail_if(&fail);
                match op {
                    BinOp::Div => (r(self, format!("sdiv {vt} {a}, {b}")), d),
                    BinOp::Mod => {
                        let one = self.splat_const(d, "1");
                        let safe = r(
                            self,
                            format!("select <{} x i1> {edge}, {vt} {one}, {vt} {b}", self.vf),
                        );
                        (r(self, format!("srem {vt} {a}, {safe}")), d)
                    }
                    _ => {
                        let q = r(self, format!("sdiv {vt} {a}, {b}"));
                        let rem = r(self, format!("srem {vt} {a}, {b}"));
                        let rnz = r(self, format!("icmp ne {vt} {rem}, {zero}"));
                        let x = r(self, format!("xor {vt} {rem}, {b}"));
                        let neg = r(self, format!("icmp slt {vt} {x}, {zero}"));
                        let adj = r(self, format!("and <{} x i1> {rnz}, {neg}", self.vf));
                        let dlt = r(self, format!("zext <{} x i1> {adj} to {vt}", self.vf));
                        (r(self, format!("sub {vt} {q}, {dlt}")), d)
                    }
                }
            }
            BinOp::Div | BinOp::FloorDiv => {
                let inst = if unsigned { "udiv" } else { "sdiv" };
                (r(self, format!("{inst} {vt} {a}, {b}")), d)
            }
            BinOp::Mod => {
                let inst = if unsigned { "urem" } else { "srem" };
                (r(self, format!("{inst} {vt} {a}, {b}")), d)
            }
            BinOp::Min | BinOp::Max => {
                let name = match (op, unsigned) {
                    (BinOp::Min, false) => "smin",
                    (BinOp::Min, true) => "umin",
                    (_, false) => "smax",
                    (_, true) => "umax",
                };
                (self.intrinsic(name, d, &[a, b]), d)
            }
            BinOp::Shl | BinOp::Shr => {
                let mask = self.splat_const(d, &(bits - 1).to_string());
                let amt = r(self, format!("and {vt} {b}, {mask}"));
                let inst = match (op, unsigned) {
                    (BinOp::Shl, _) => "shl",
                    (_, true) => "lshr",
                    _ => "ashr",
                };
                (r(self, format!("{inst} {vt} {a}, {amt}")), d)
            }
            BinOp::Pow if s == "i64" => (self.per_lane("iris_pow_i64", d, &[a, b]), d),
            BinOp::Pow => {
                // `iris_pow_i64` at 64 bits, narrowed back, as the scalar code does.
                let ext = if unsigned { "zext" } else { "sext" };
                let w = format!("<{} x i64>", self.vf);
                let a64 = r(self, format!("{ext} {vt} {a} to {w}"));
                let b64 = r(self, format!("{ext} {vt} {b} to {w}"));
                let p = self.per_lane("iris_pow_i64", DType::I64, &[&a64, &b64]);
                (r(self, format!("trunc {w} {p} to {vt}")), d)
            }
        })
    }

    fn unary(&mut self, op: ScalarUnaryOp, d: DType, x: &str) -> String {
        let vt = self.vty(d);
        let float = is_float(d);
        let r = |e: &mut Self, inst: String| {
            let r = e.t();
            e.line(format!("{r} = {inst}"));
            r
        };
        match op {
            ScalarUnaryOp::Neg if float => r(self, format!("fneg {vt} {x}")),
            ScalarUnaryOp::Neg => r(self, format!("sub {vt} zeroinitializer, {x}")),
            ScalarUnaryOp::Not => {
                let t = self.splat_const(DType::Bool, "true");
                r(self, format!("xor {vt} {x}, {t}"))
            }
            ScalarUnaryOp::BitNot => {
                let m = self.splat_const(d, "-1");
                r(self, format!("xor {vt} {x}, {m}"))
            }
            ScalarUnaryOp::Abs if float => self.intrinsic("fabs", d, &[x]),
            ScalarUnaryOp::Abs => {
                let full = format!("llvm.abs.{}", self.vsfx(d));
                self.declare(format!("declare {vt} @{full}({vt}, i1)"));
                r(self, format!("call {vt} @{full}({vt} {x}, i1 false)"))
            }
            ScalarUnaryOp::Sqrt => self.intrinsic("sqrt", d, &[x]),
            ScalarUnaryOp::Floor => self.intrinsic("floor", d, &[x]),
            ScalarUnaryOp::Ceil => self.intrinsic("ceil", d, &[x]),
            ScalarUnaryOp::Round => self.intrinsic("round", d, &[x]),
            ScalarUnaryOp::Sin => self.intrinsic("sin", d, &[x]),
            ScalarUnaryOp::Cos => self.intrinsic("cos", d, &[x]),
            ScalarUnaryOp::Exp => self.intrinsic("exp", d, &[x]),
            ScalarUnaryOp::Log => self.intrinsic("log", d, &[x]),
            ScalarUnaryOp::Log2 => self.intrinsic("log2", d, &[x]),
            ScalarUnaryOp::Tan => {
                let f = if d == DType::F32 { "tanf" } else { "tan" };
                self.per_lane(f, d, &[x])
            }
            ScalarUnaryOp::Sign => {
                let (one, minus, zero) = if float {
                    ("1.0", "-1.0", "0.0")
                } else {
                    ("1", "-1", "0")
                };
                let z = self.splat_const(d, zero);
                let (gt, lt) = if float {
                    ("fcmp ogt", "fcmp olt")
                } else {
                    ("icmp sgt", "icmp slt")
                };
                let pos = r(self, format!("{gt} {vt} {x}, {z}"));
                let neg = r(self, format!("{lt} {vt} {x}, {z}"));
                let m1 = self.splat_const(d, minus);
                let p1 = self.splat_const(d, one);
                let mid = r(
                    self,
                    format!("select <{} x i1> {neg}, {vt} {m1}, {vt} {z}", self.vf),
                );
                r(
                    self,
                    format!("select <{} x i1> {pos}, {vt} {p1}, {vt} {mid}", self.vf),
                )
            }
        }
    }

    fn cast(&mut self, from: DType, to: DType, x: &str) -> String {
        if from == to {
            return x.to_owned();
        }
        let (fv, tv) = (self.vty(from), self.vty(to));
        let r = |e: &mut Self, inst: String| {
            let r = e.t();
            e.line(format!("{r} = {inst}"));
            r
        };
        match (is_float(from), is_float(to)) {
            (true, false) if to == DType::Bool => {
                let z = self.splat_const(from, "0.0");
                r(self, format!("fcmp une {fv} {x}, {z}"))
            }
            (true, false) => {
                let f = if is_unsigned(to) { "fptoui" } else { "fptosi" };
                let full = format!("llvm.{f}.sat.{}.{}", self.vsfx(to), self.vsfx(from));
                self.declare(format!("declare {tv} @{full}({fv})"));
                r(self, format!("call {tv} @{full}({fv} {x})"))
            }
            (false, true) => {
                let f = if is_unsigned(from) || from == DType::Bool {
                    "uitofp"
                } else {
                    "sitofp"
                };
                r(self, format!("{f} {fv} {x} to {tv}"))
            }
            (true, true) => {
                let f = if from == DType::F32 {
                    "fpext"
                } else {
                    "fptrunc"
                };
                r(self, format!("{f} {fv} {x} to {tv}"))
            }
            (false, false) => {
                let (fb, tb) = (int_bits(from), int_bits(to));
                if fb == tb {
                    x.to_owned()
                } else if fb > tb {
                    r(self, format!("trunc {fv} {x} to {tv}"))
                } else if is_unsigned(from) || from == DType::Bool {
                    r(self, format!("zext {fv} {x} to {tv}"))
                } else {
                    r(self, format!("sext {fv} {x} to {tv}"))
                }
            }
        }
    }
}

fn int_bits(d: DType) -> usize {
    match d {
        DType::Bool => 1,
        other => lane_bits(other).unwrap_or(64),
    }
}

fn unsupported(func: &IrFunction, detail: String) -> CodegenError {
    CodegenError::Unsupported {
        backend: "simd".into(),
        detail: format!("{detail} in `{}`", func.name),
    }
}

/// LLVM's literal for a float constant of lane type `d`: hex of the double
/// it equals, exact for `f32` too.
fn float_literal(v: f64, d: DType) -> String {
    let v = if d == DType::F32 { v as f32 as f64 } else { v };
    format!("0x{:016X}", v.to_bits())
}

/// The kernel for an element-wise body, or `None` for a body that is not.
fn vector_kernel(func: &IrFunction, isa: &VectorIsa) -> Result<Option<Kernel>, CodegenError> {
    let Some(plan) = plan(func, isa) else {
        return Ok(None);
    };
    let vf = plan.vf;
    let args = arg_ids(func);
    let mut e = Emit {
        func,
        vf,
        text: String::new(),
        tmp: 0,
        block: 0,
        decls: BTreeSet::new(),
        vals: HashMap::new(),
        data: HashMap::new(),
        pending: HashMap::new(),
        stores: Vec::new(),
    };
    let name = &func.name;
    e.text.push_str(&format!(
        "; `par for` body `{name}`, {vf} iterations at a time; the rest of the range runs `@{name}`.\n"
    ));
    e.text.push_str(&format!(
        "define void @{name}_simd(i64 %lo, i64 %hi, ptr %env) #0 {{\nentry:\n"
    ));

    // The captures: tensors' storage and length, numbers unboxed as the
    // scalar body unboxes them.
    let mut numels: Vec<String> = Vec::new();
    let mut written: Vec<usize> = Vec::new();
    let mut read: Vec<usize> = Vec::new();
    let mut splats: Vec<(usize, DType, String)> = Vec::new();
    for (slot, use_) in &plan.captures {
        let raw = e.t();
        e.line(format!(
            "{raw} = call ptr @iris_closure_get_capture(ptr %env, i32 {slot})"
        ));
        match use_ {
            CaptureUse::Tensor { written: w } => {
                let data = e.t();
                e.line(format!("{data} = load ptr, ptr {raw}"));
                let np = e.t();
                e.line(format!("{np} = getelementptr inbounds {{ ptr, ptr, i32, i64 }}, ptr {raw}, i32 0, i32 3"));
                let n = e.t();
                e.line(format!("{n} = load i64, ptr {np}"));
                numels.push(n);
                e.data.insert(*slot, data);
                if *w {
                    written.push(*slot);
                } else {
                    read.push(*slot);
                }
            }
            CaptureUse::Scalar(d) => {
                let v = match d {
                    DType::I64 | DType::U64 | DType::USize => {
                        let v = e.t();
                        e.line(format!("{v} = call i64 @iris_unbox_i64(ptr {raw})"));
                        v
                    }
                    DType::F64 => {
                        let v = e.t();
                        e.line(format!("{v} = call double @iris_unbox_f64(ptr {raw})"));
                        v
                    }
                    DType::F32 => {
                        let w = e.t();
                        e.line(format!("{w} = call double @iris_unbox_f64(ptr {raw})"));
                        let v = e.t();
                        e.line(format!("{v} = fptrunc double {w} to float"));
                        v
                    }
                    DType::Bool => {
                        let w = e.t();
                        e.line(format!("{w} = call i32 @iris_unbox_bool(ptr {raw})"));
                        let v = e.t();
                        e.line(format!("{v} = trunc i32 {w} to i1"));
                        v
                    }
                    other => {
                        let w = e.t();
                        e.line(format!("{w} = call i64 @iris_unbox_i64(ptr {raw})"));
                        let v = e.t();
                        e.line(format!("{v} = trunc i64 {w} to {}", Emit::scalar(*other)));
                        v
                    }
                };
                splats.push((*slot, *d, v));
            }
        }
    }
    // A written tensor whose storage another accessed one shares: scalar.
    let mut alias = "false".to_owned();
    for w in &written {
        for o in written.iter().chain(read.iter()).filter(|o| *o != w) {
            let (dw, dov) = (e.data[w].clone(), e.data[o].clone());
            let same = e.t();
            e.line(format!("{same} = icmp eq ptr {dw}, {dov}"));
            let acc = e.t();
            e.line(format!("{acc} = or i1 {alias}, {same}"));
            alias = acc;
        }
    }
    for (slot, d, v) in &splats {
        let s = e.splat(*d, v);
        e.vals.insert(args[slot + 1], (s, *d));
    }
    e.line("br label %vhead");

    // The vector loop.
    e.text.push_str("vhead:\n");
    e.line("%base = phi i64 [ %lo, %entry ], [ %vnext, %vstore ]");
    e.line(format!("%vnext = add i64 %base, {vf}"));
    e.line("%vfits = icmp sle i64 %vnext, %hi");
    e.line("br i1 %vfits, label %vbody, label %scalar");
    e.text.push_str("vbody:\n");
    // Every tensor access is at the loop index: in range for the whole
    // vector when 0 <= base and base + VF <= length.
    let mut ok = e.t();
    e.line(format!("{ok} = icmp sge i64 %base, 0"));
    for n in &numels {
        let fits = e.t();
        e.line(format!("{fits} = icmp sle i64 %vnext, {n}"));
        let acc = e.t();
        e.line(format!("{acc} = and i1 {ok}, {fits}"));
        ok = acc;
    }
    let fail = e.t();
    e.line(format!("{fail} = xor i1 {ok}, true"));
    let fail_or_alias = e.t();
    e.line(format!("{fail_or_alias} = or i1 {fail}, {alias}"));
    e.bail_if(&fail_or_alias);

    if plan.lanes {
        let b = e.splat(DType::I64, "%base");
        let steps: Vec<String> = (0..vf).map(|i| format!("i64 {i}")).collect();
        let l = e.t();
        e.line(format!(
            "{l} = add <{vf} x i64> {b}, <{}>",
            steps.join(", ")
        ));
        e.vals.insert(args[0], (l, DType::I64));
    }

    for instr in &func.blocks()[0].instrs {
        match instr {
            IrInstr::ConstInt { result, value, ty } => {
                let d = scalar_dtype(ty).unwrap_or(DType::I64);
                let c = if d == DType::Bool {
                    (*value != 0).to_string()
                } else {
                    value.to_string()
                };
                let v = e.splat_const(d, &c);
                e.vals.insert(*result, (v, d));
            }
            IrInstr::ConstFloat { result, value, ty } => {
                let d = scalar_dtype(ty).unwrap_or(DType::F64);
                let v = e.splat_const(d, &float_literal(*value, d));
                e.vals.insert(*result, (v, d));
            }
            IrInstr::ConstBool { result, value } => {
                let v = e.splat_const(DType::Bool, if *value { "true" } else { "false" });
                e.vals.insert(*result, (v, DType::Bool));
            }
            IrInstr::Load {
                result,
                tensor,
                result_ty,
                ..
            } => {
                let slot = args.iter().position(|p| p == tensor).unwrap_or(1) - 1;
                let d = scalar_dtype(result_ty).unwrap_or(DType::F32);
                let stored = match e.pending.get(&slot) {
                    Some(v) => v.clone(),
                    None => {
                        let data = e.data[&slot].clone();
                        let p = e.t();
                        e.line(format!(
                            "{p} = getelementptr inbounds float, ptr {data}, i64 %base"
                        ));
                        let l = e.t();
                        e.line(format!("{l} = load <{vf} x float>, ptr {p}, align 4"));
                        l
                    }
                };
                let v = e.convert_from_storage(&stored, d);
                e.vals.insert(*result, (v, d));
            }
            IrInstr::Store { tensor, value, .. } => {
                let slot = args.iter().position(|p| p == tensor).unwrap_or(1) - 1;
                let (v, d) = e.val(*value)?;
                let f = e.convert_to_storage(&v, d);
                e.pending.insert(slot, f.clone());
                e.stores.push((slot, f));
            }
            IrInstr::BinOp {
                result,
                op,
                lhs,
                rhs,
                ..
            } => {
                let (a, d) = e.val(*lhs)?;
                let (b, _) = e.val(*rhs)?;
                let v = e.binop(*op, d, &a, &b)?;
                e.vals.insert(*result, v);
            }
            IrInstr::UnaryOp {
                result,
                op,
                operand,
                ..
            } => {
                let (x, d) = e.val(*operand)?;
                let v = e.unary(*op, d, &x);
                e.vals.insert(*result, (v, d));
            }
            IrInstr::Cast {
                result,
                operand,
                to_ty,
                ..
            } => {
                let (x, from) = e.val(*operand)?;
                let to = scalar_dtype(to_ty).unwrap_or(from);
                let v = e.cast(from, to, &x);
                e.vals.insert(*result, (v, to));
            }
            _ => {}
        }
    }
    e.line("br label %vstore");

    // Every check passed: the stores, in program order.
    e.text.push_str("vstore:\n");
    for (slot, f) in std::mem::take(&mut e.stores) {
        let data = e.data[&slot].clone();
        let p = e.t();
        e.line(format!(
            "{p} = getelementptr inbounds float, ptr {data}, i64 %base"
        ));
        e.line(format!("store <{vf} x float> {f}, ptr {p}, align 4"));
    }
    e.line("br label %vhead");

    // From `%base` to the end, one iteration at a time.
    e.text.push_str("scalar:\n");
    e.line("br label %shead");
    e.text.push_str("shead:\n");
    e.line("%si = phi i64 [ %base, %scalar ], [ %snext, %sbody ]");
    e.line("%sdone = icmp sge i64 %si, %hi");
    e.line("br i1 %sdone, label %exit, label %sbody");
    e.text.push_str("sbody:\n");
    e.line(format!("call i64 @{name}(i64 %si, ptr %env)"));
    e.line("%snext = add i64 %si, 1");
    e.line("br label %shead");
    e.text.push_str("exit:\n");
    e.line("ret void");
    e.text.push_str("}\n");

    Ok(Some(Kernel {
        body: name.clone(),
        text: e.text,
        decls: e.decls,
    }))
}
