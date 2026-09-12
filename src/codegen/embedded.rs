//! Allocation-free bare-metal object emission for Cortex-M and ESP32 targets.
//!
//! The embedded profile is intentionally smaller than hosted IRIS.  It accepts
//! scalar SSA, structured control flow, direct non-recursive calls, and fixed
//! stack arrays.  Anything whose hosted lowering can reach the heap is rejected
//! before LLVM sees the module.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::error::CodegenError;
use crate::ir::block::BlockId;
use crate::ir::instr::IrInstr;
use crate::ir::module::IrModule;
use crate::ir::types::IrType;

use super::llvm_c_api::compile_llvm_ir_to_object_bytes_optimized;
use super::llvm_ir::emit_llvm_ir_with_target;

const HARDWARE_REPORT_VERSION: &str = "IRIS-HW/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddedTarget {
    ArduinoUno,
    CortexM4F,
    CortexM33,
    Esp32C3,
    Esp32Xtensa,
}

impl EmbeddedTarget {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "arduino-uno" | "uno" | "atmega328p" | "avr-unknown-unknown" => Some(Self::ArduinoUno),
            "cortex-m4f" | "thumbv7em-none-eabihf" => Some(Self::CortexM4F),
            "cortex-m33" | "thumbv8m.main-none-eabihf" => Some(Self::CortexM33),
            "esp32-c3" | "riscv32-unknown-none-elf" => Some(Self::Esp32C3),
            "esp32" | "esp32-xtensa" | "xtensa-esp32-none-elf" => Some(Self::Esp32Xtensa),
            _ => None,
        }
    }

    pub fn preset(self) -> &'static str {
        match self {
            Self::ArduinoUno => "arduino-uno",
            Self::CortexM4F => "cortex-m4f",
            Self::CortexM33 => "cortex-m33",
            Self::Esp32C3 => "esp32-c3",
            Self::Esp32Xtensa => "esp32",
        }
    }

    pub fn triple(self) -> &'static str {
        match self {
            Self::ArduinoUno => "avr-unknown-unknown",
            Self::CortexM4F => "thumbv7em-none-eabihf",
            Self::CortexM33 => "thumbv8m.main-none-eabihf",
            Self::Esp32C3 => "riscv32-unknown-none-elf",
            Self::Esp32Xtensa => "xtensa-esp32-none-elf",
        }
    }

    fn elf_machine(self) -> u16 {
        match self {
            Self::ArduinoUno => 83,                  // EM_AVR
            Self::CortexM4F | Self::CortexM33 => 40, // EM_ARM
            Self::Esp32C3 => 243,                    // EM_RISCV
            Self::Esp32Xtensa => 94,                 // EM_XTENSA
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedProof {
    pub target: EmbeddedTarget,
    pub entry: String,
    pub reachable_functions: Vec<String>,
    pub fixed_array_bytes: u64,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedBundle {
    pub module_ir: PathBuf,
    pub module_object: PathBuf,
    pub runtime_object: PathBuf,
    pub harness_object: PathBuf,
    pub header: PathBuf,
    pub manifest: PathBuf,
    pub board_support: Option<PathBuf>,
    pub proof: EmbeddedProof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareValidationReport {
    pub target: EmbeddedTarget,
    pub fingerprint: String,
    pub result: i64,
    pub allocations: u64,
    pub faults: u32,
}

/// Platform calls are supplied by the board support package.  Their ABI is
/// scalar-only and each operation is required to be wait-free or bounded by
/// the BSP; none may allocate.
const PLATFORM_HOOKS: &[&str] = &[
    "iris_embedded_gpio_read",
    "iris_embedded_gpio_write",
    "iris_embedded_adc_read",
    "iris_embedded_pwm_write",
    "iris_embedded_monotonic_us",
    "iris_embedded_watchdog_kick",
    "iris_embedded_uart_write_byte",
];

fn codegen_error(detail: impl Into<String>) -> CodegenError {
    CodegenError::Unsupported {
        backend: "embedded".into(),
        detail: detail.into(),
    }
}

fn scalar_signature_type(ty: &IrType) -> bool {
    matches!(ty, IrType::Scalar(_))
}

fn instruction_kind(instr: &IrInstr) -> &'static str {
    match instr {
        IrInstr::TensorOp { .. } | IrInstr::Load { .. } | IrInstr::Store { .. } => {
            "tensor operation"
        }
        IrInstr::CallExtern { .. } => "external call",
        IrInstr::Retain { .. } | IrInstr::Release { .. } => "reference counting",
        IrInstr::MakeStruct { .. } | IrInstr::GetField { .. } => "heap-backed record",
        IrInstr::MakeTraitObject { .. } | IrInstr::DynCall { .. } => "dynamic trait object",
        IrInstr::MakeVariant { .. }
        | IrInstr::SwitchVariant { .. }
        | IrInstr::ExtractVariantField { .. }
        | IrInstr::GetVariantTag { .. } => "heap-backed choice",
        IrInstr::MakeTuple { .. } | IrInstr::GetElement { .. } => "heap-backed tuple",
        IrInstr::MakeClosure { .. } | IrInstr::CallClosure { .. } => "closure",
        IrInstr::MakeSome { .. }
        | IrInstr::MakeNone { .. }
        | IrInstr::IsSome { .. }
        | IrInstr::OptionUnwrap { .. }
        | IrInstr::MakeOk { .. }
        | IrInstr::MakeErr { .. }
        | IrInstr::IsOk { .. }
        | IrInstr::ResultUnwrap { .. }
        | IrInstr::ResultUnwrapErr { .. } => "heap-backed option/result",
        IrInstr::ChanNew { .. }
        | IrInstr::ChanSend { .. }
        | IrInstr::ChanRecv { .. }
        | IrInstr::Spawn { .. }
        | IrInstr::TaskGroupNew { .. }
        | IrInstr::TaskGroupSpawn { .. }
        | IrInstr::TaskGroupJoin { .. }
        | IrInstr::TaskGroupCancel { .. }
        | IrInstr::ParFor { .. } => "concurrency",
        IrInstr::AtomicNew { .. }
        | IrInstr::AtomicLoad { .. }
        | IrInstr::AtomicStore { .. }
        | IrInstr::AtomicAdd { .. }
        | IrInstr::MutexNew { .. }
        | IrInstr::MutexLock { .. }
        | IrInstr::MutexUnlock { .. } => "host synchronization",
        IrInstr::MakeGrad { .. }
        | IrInstr::GradValue { .. }
        | IrInstr::GradTangent { .. }
        | IrInstr::TapeRecord { .. }
        | IrInstr::Backward { .. }
        | IrInstr::TapeGrad { .. }
        | IrInstr::Sparsify { .. }
        | IrInstr::Densify { .. }
        | IrInstr::SparseNnz { .. } => "heap-backed autodiff/sparse value",
        IrInstr::ConstStr { .. }
        | IrInstr::StrLen { .. }
        | IrInstr::StrConcat { .. }
        | IrInstr::StrContains { .. }
        | IrInstr::StrStartsWith { .. }
        | IrInstr::StrEndsWith { .. }
        | IrInstr::StrToUpper { .. }
        | IrInstr::StrToLower { .. }
        | IrInstr::StrTrim { .. }
        | IrInstr::StrRepeat { .. }
        | IrInstr::ValueToStr { .. }
        | IrInstr::ParseI64 { .. }
        | IrInstr::ParseF64 { .. }
        | IrInstr::StrIndex { .. }
        | IrInstr::StrSlice { .. }
        | IrInstr::StrFind { .. }
        | IrInstr::StrReplace { .. }
        | IrInstr::StrEq { .. }
        | IrInstr::StrSplit { .. }
        | IrInstr::StrJoin { .. } => "dynamic string",
        IrInstr::Print { .. }
        | IrInstr::Panic { .. }
        | IrInstr::ReadLine { .. }
        | IrInstr::ReadI64 { .. }
        | IrInstr::ReadF64 { .. } => "host console I/O",
        IrInstr::ListNew { .. }
        | IrInstr::ListPush { .. }
        | IrInstr::ListLen { .. }
        | IrInstr::ListGet { .. }
        | IrInstr::ListSet { .. }
        | IrInstr::ListPop { .. }
        | IrInstr::MapNew { .. }
        | IrInstr::MapSet { .. }
        | IrInstr::MapGet { .. }
        | IrInstr::MapContains { .. }
        | IrInstr::MapRemove { .. }
        | IrInstr::MapLen { .. }
        | IrInstr::ListContains { .. }
        | IrInstr::ListSort { .. }
        | IrInstr::MapKeys { .. }
        | IrInstr::MapValues { .. }
        | IrInstr::ListConcat { .. }
        | IrInstr::ListSlice { .. } => "dynamic collection",
        IrInstr::FileReadAll { .. }
        | IrInstr::FileWriteAll { .. }
        | IrInstr::FileExists { .. }
        | IrInstr::FileLines { .. }
        | IrInstr::DbOpen { .. }
        | IrInstr::DbExec { .. }
        | IrInstr::DbExecParams { .. }
        | IrInstr::DbQuery { .. }
        | IrInstr::DbQueryParams { .. }
        | IrInstr::DbClose { .. }
        | IrInstr::ProcessExit { .. }
        | IrInstr::ProcessArgs { .. }
        | IrInstr::EnvVar { .. }
        | IrInstr::TcpConnect { .. }
        | IrInstr::TcpListen { .. }
        | IrInstr::TcpAccept { .. }
        | IrInstr::TcpRead { .. }
        | IrInstr::TcpWrite { .. }
        | IrInstr::TcpClose { .. }
        | IrInstr::NowMs { .. }
        | IrInstr::SleepMs { .. } => "host service",
        IrInstr::BuiltinCall { .. } => "host runtime builtin",
        IrInstr::PushHandler { .. } | IrInstr::ResumeCont { .. } => "effect handler",
        _ => "unsupported instruction",
    }
}

/// Prove that the entry's complete direct call graph stays within the
/// allocation-free embedded subset.  The proof is intentionally independent
/// of source effect annotations, so an annotation bug cannot certify heap use.
pub fn validate_embedded_module(
    module: &IrModule,
    target: EmbeddedTarget,
    entry: &str,
) -> Result<EmbeddedProof, CodegenError> {
    let functions = module
        .functions()
        .iter()
        .map(|function| (function.name.as_str(), function))
        .collect::<HashMap<_, _>>();
    let entry_fn = functions
        .get(entry)
        .ok_or_else(|| codegen_error(format!("entry function '{}' was not found", entry)))?;
    if !entry_fn.params.is_empty()
        || entry_fn.return_ty != IrType::Scalar(crate::ir::types::DType::I64)
    {
        return Err(codegen_error(format!(
            "entry '{}' must have the hardware-validation ABI `def {}() -> i64`",
            entry, entry
        )));
    }

    let mut reachable = HashSet::new();
    let mut stack = vec![entry.to_owned()];
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    let mut fixed_array_bytes = 0u64;

    while let Some(name) = stack.pop() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        let function = functions
            .get(name.as_str())
            .ok_or_else(|| codegen_error(format!("reachable function '{}' is undefined", name)))?;
        if target == EmbeddedTarget::ArduinoUno && function_has_cfg_cycle(function) {
            return Err(codegen_error(format!(
                "function '{}' contains a loop; the LLVM 17 Arduino Uno profile rejects cyclic control flow because its AVR optimizer can miscompile i64 induction values",
                name
            )));
        }
        if !function
            .params
            .iter()
            .all(|param| scalar_signature_type(&param.ty))
            || !scalar_signature_type(&function.return_ty)
        {
            return Err(codegen_error(format!(
                "reachable function '{}' has a non-scalar embedded ABI",
                name
            )));
        }

        let mut callees = Vec::new();
        for block in function.blocks() {
            for instr in &block.instrs {
                match instr {
                    IrInstr::BinOp { .. }
                    | IrInstr::ConstFloat { .. }
                    | IrInstr::ConstInt { .. }
                    | IrInstr::ConstBool { .. }
                    | IrInstr::UnaryOp { .. }
                    | IrInstr::Cast { .. }
                    | IrInstr::Br { .. }
                    | IrInstr::CondBr { .. }
                    | IrInstr::Return { .. }
                    | IrInstr::ArrayLoad { .. }
                    | IrInstr::ArrayStore { .. } => {}
                    IrInstr::AllocArray { elem_ty, size, .. } => {
                        if target == EmbeddedTarget::ArduinoUno {
                            return Err(codegen_error(format!(
                                "function '{}' uses a fixed array; the LLVM 17 Arduino Uno profile supports scalar stack values only",
                                name
                            )));
                        }
                        let bits = match elem_ty {
                            IrType::Scalar(dtype) => dtype.bit_width() as u64,
                            _ => {
                                return Err(codegen_error(format!(
                                "function '{}' allocates a fixed array with non-scalar elements",
                                name
                            )))
                            }
                        };
                        fixed_array_bytes = fixed_array_bytes
                            .checked_add((bits * *size as u64).div_ceil(8))
                            .ok_or_else(|| codegen_error("fixed-array footprint overflow"))?;
                    }
                    IrInstr::Call { callee, .. } => {
                        if !functions.contains_key(callee.as_str()) {
                            return Err(codegen_error(format!(
                                "function '{}' calls unresolved function '{}'",
                                name, callee
                            )));
                        }
                        callees.push(callee.clone());
                        stack.push(callee.clone());
                    }
                    IrInstr::CallExtern { name: external, .. }
                        if PLATFORM_HOOKS.contains(&external.as_str()) => {}
                    other => {
                        return Err(codegen_error(format!(
                            "function '{}' uses {} which is outside the allocation-free embedded profile",
                            name,
                            instruction_kind(other)
                        )));
                    }
                }
            }
        }
        edges.insert(name, callees);
    }

    reject_recursion(entry, &edges)?;
    let mut reachable_functions = reachable.into_iter().collect::<Vec<_>>();
    reachable_functions.sort();
    let mut semantic_ir = String::new();
    for name in &reachable_functions {
        let function = functions[name.as_str()];
        semantic_ir.push_str(&format!(
            "fn:{};params:{:?};ret:{};",
            function.name, function.params, function.return_ty
        ));
        for block in function.blocks() {
            semantic_ir.push_str(&format!("block:{:?};params:{:?};", block.id, block.params));
            for instr in &block.instrs {
                semantic_ir.push_str(&format!("instr:{instr:?};"));
            }
        }
    }
    let fingerprint = proof_fingerprint(
        target,
        entry,
        &reachable_functions,
        fixed_array_bytes,
        &semantic_ir,
    );
    Ok(EmbeddedProof {
        target,
        entry: entry.to_owned(),
        reachable_functions,
        fixed_array_bytes,
        fingerprint,
    })
}

fn function_has_cfg_cycle(function: &crate::ir::function::IrFunction) -> bool {
    let mut edges = HashMap::<BlockId, Vec<BlockId>>::new();
    for block in function.blocks() {
        let successors = match block.terminator() {
            Some(IrInstr::Br { target, .. }) => vec![*target],
            Some(IrInstr::CondBr {
                then_block,
                else_block,
                ..
            }) => vec![*then_block, *else_block],
            _ => Vec::new(),
        };
        edges.insert(block.id, successors);
    }

    fn visit(
        node: BlockId,
        edges: &HashMap<BlockId, Vec<BlockId>>,
        active: &mut HashSet<BlockId>,
        done: &mut HashSet<BlockId>,
    ) -> bool {
        if active.contains(&node) {
            return true;
        }
        if !done.insert(node) {
            return false;
        }
        active.insert(node);
        let cyclic = edges
            .get(&node)
            .into_iter()
            .flatten()
            .copied()
            .any(|next| visit(next, edges, active, done));
        active.remove(&node);
        cyclic
    }

    function
        .blocks()
        .first()
        .is_some_and(|entry| visit(entry.id, &edges, &mut HashSet::new(), &mut HashSet::new()))
}

fn reject_recursion(entry: &str, edges: &HashMap<String, Vec<String>>) -> Result<(), CodegenError> {
    fn visit(
        node: &str,
        edges: &HashMap<String, Vec<String>>,
        active: &mut HashSet<String>,
        done: &mut HashSet<String>,
    ) -> Result<(), CodegenError> {
        if active.contains(node) {
            return Err(codegen_error(format!(
                "recursive call cycle reaches '{}'; embedded stack bounds require an acyclic call graph",
                node
            )));
        }
        if !done.insert(node.to_owned()) {
            return Ok(());
        }
        active.insert(node.to_owned());
        if let Some(next) = edges.get(node) {
            for callee in next {
                visit(callee, edges, active, done)?;
            }
        }
        active.remove(node);
        Ok(())
    }
    visit(entry, edges, &mut HashSet::new(), &mut HashSet::new())
}

fn proof_fingerprint(
    target: EmbeddedTarget,
    entry: &str,
    functions: &[String],
    fixed_array_bytes: u64,
    semantic_ir: &str,
) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    let payload = format!(
        "{}\0{}\0{}\0{}\0{}",
        target.preset(),
        entry,
        functions.join("\0"),
        fixed_array_bytes,
        semantic_ir
    );
    for byte in payload.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Emit two relocatable objects: the compiled IRIS component and a freestanding
/// runtime containing only checked integer arithmetic, bounds failure, and the
/// hardware-validation counters.  No C compiler or hosted libc is involved.
pub fn build_embedded_bundle(
    module: &IrModule,
    output_dir: &Path,
    target: EmbeddedTarget,
    entry: &str,
) -> Result<EmbeddedBundle, CodegenError> {
    let proof = validate_embedded_module(module, target, entry)?;
    std::fs::create_dir_all(output_dir)?;

    let mut llvm_ir = emit_llvm_ir_with_target(module, Some(target.triple()))?;
    // Bare-metal board support packages own the C `main` reset entry. Keep the
    // source-level IRIS entry name in the proof while giving its object symbol
    // a collision-free ABI name that the generated validation harness calls.
    let embedded_entry = embedded_entry_symbol(&proof);
    llvm_ir = llvm_ir.replace(
        &format!("@{}(", proof.entry),
        &format!("@{}(", embedded_entry),
    );
    if contains_allocator_reference(&llvm_ir) {
        return Err(codegen_error(
            "generated reachable code references a hosted allocator despite passing embedded validation",
        ));
    }
    let module_ir = output_dir.join("iris_module.ll");
    std::fs::write(&module_ir, &llvm_ir)?;
    let emit_object = |ir: &str| {
        compile_llvm_ir_to_object_bytes_optimized(ir, Some(target.triple()), "default<O1>")
    };
    let module_bytes = emit_object(&llvm_ir)?;
    let runtime_ir = embedded_runtime_ir(target);
    let runtime_bytes = emit_object(&runtime_ir)?;
    let harness_ir = embedded_harness_ir(&proof);
    let harness_bytes = emit_object(&harness_ir)?;
    verify_elf_machine(&module_bytes, target)?;
    verify_elf_machine(&runtime_bytes, target)?;
    verify_elf_machine(&harness_bytes, target)?;
    reject_allocator_symbols(&module_bytes)?;
    reject_allocator_symbols(&runtime_bytes)?;
    reject_allocator_symbols(&harness_bytes)?;

    let module_object = output_dir.join("iris_module.o");
    let runtime_object = output_dir.join("iris_embedded_runtime.o");
    let harness_object = output_dir.join("iris_embedded_harness.o");
    let header = output_dir.join("iris_embedded.h");
    let manifest = output_dir.join("iris_embedded.json");
    let board_support = if target == EmbeddedTarget::ArduinoUno {
        let path = output_dir.join("arduino_uno_validation.c");
        std::fs::write(&path, include_str!("../runtime/arduino_uno_validation.c"))?;
        Some(path)
    } else {
        None
    };
    std::fs::write(&module_object, module_bytes)?;
    std::fs::write(&runtime_object, runtime_bytes)?;
    std::fs::write(&harness_object, harness_bytes)?;
    std::fs::write(&header, embedded_header(&proof))?;
    std::fs::write(&manifest, embedded_manifest(&proof))?;

    Ok(EmbeddedBundle {
        module_ir,
        module_object,
        runtime_object,
        harness_object,
        header,
        manifest,
        board_support,
        proof,
    })
}

fn contains_allocator_reference(llvm_ir: &str) -> bool {
    llvm_ir.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("call ")
            && ["@malloc(", "@calloc(", "@realloc(", "@free("]
                .iter()
                .any(|symbol| line.contains(symbol))
    })
}

fn reject_allocator_symbols(object: &[u8]) -> Result<(), CodegenError> {
    for symbol in [
        b"malloc".as_slice(),
        b"calloc",
        b"realloc",
        b"_Znwm",
        b"_Znam",
    ] {
        if object.windows(symbol.len()).any(|window| window == symbol) {
            return Err(codegen_error(format!(
                "emitted object contains forbidden allocator symbol '{}'",
                String::from_utf8_lossy(symbol)
            )));
        }
    }
    Ok(())
}

fn verify_elf_machine(object: &[u8], target: EmbeddedTarget) -> Result<(), CodegenError> {
    if object.len() < 20 || !object.starts_with(b"\x7fELF") {
        return Err(codegen_error(
            "embedded object is not an ELF relocatable object",
        ));
    }
    let machine = u16::from_le_bytes([object[18], object[19]]);
    if machine != target.elf_machine() {
        return Err(codegen_error(format!(
            "object architecture mismatch: target '{}' requires ELF machine {}, found {}",
            target.preset(),
            target.elf_machine(),
            machine
        )));
    }
    Ok(())
}

fn embedded_runtime_ir(target: EmbeddedTarget) -> String {
    let layout = super::llvm_ir::target_data_layout(target.triple());
    format!(
        r#"; IRIS allocation-free embedded runtime
target datalayout = "{layout}"
target triple = "{triple}"

@iris_embedded_fault_code = global i32 0, align 4
@iris_embedded_allocation_count = constant i64 0, align 8

declare {{ i64, i1 }} @llvm.sadd.with.overflow.i64(i64, i64)
declare {{ i64, i1 }} @llvm.ssub.with.overflow.i64(i64, i64)
declare {{ i64, i1 }} @llvm.smul.with.overflow.i64(i64, i64)

define void @iris_embedded_fault(i32 %code) noreturn nounwind section ".text.iris_embedded_fault" {{
entry:
  store volatile i32 %code, ptr @iris_embedded_fault_code, align 4
  br label %halt
halt:
  br label %halt
}}

define i64 @iris_add_checked(i64 %a, i64 %b) nounwind section ".text.iris_add_checked" {{
entry:
  %pair = call {{ i64, i1 }} @llvm.sadd.with.overflow.i64(i64 %a, i64 %b)
  %value = extractvalue {{ i64, i1 }} %pair, 0
  %overflow = extractvalue {{ i64, i1 }} %pair, 1
  br i1 %overflow, label %fault, label %ok
fault:
  call void @iris_embedded_fault(i32 1)
  unreachable
ok:
  ret i64 %value
}}

define i64 @iris_sub_checked(i64 %a, i64 %b) nounwind section ".text.iris_sub_checked" {{
entry:
  %pair = call {{ i64, i1 }} @llvm.ssub.with.overflow.i64(i64 %a, i64 %b)
  %value = extractvalue {{ i64, i1 }} %pair, 0
  %overflow = extractvalue {{ i64, i1 }} %pair, 1
  br i1 %overflow, label %fault, label %ok
fault:
  call void @iris_embedded_fault(i32 2)
  unreachable
ok:
  ret i64 %value
}}

define i64 @iris_mul_checked(i64 %a, i64 %b) nounwind section ".text.iris_mul_checked" {{
entry:
  %pair = call {{ i64, i1 }} @llvm.smul.with.overflow.i64(i64 %a, i64 %b)
  %value = extractvalue {{ i64, i1 }} %pair, 0
  %overflow = extractvalue {{ i64, i1 }} %pair, 1
  br i1 %overflow, label %fault, label %ok
fault:
  call void @iris_embedded_fault(i32 3)
  unreachable
ok:
  ret i64 %value
}}

define void @iris_bounds_check_abort(i64 %index, i64 %size) noreturn nounwind section ".text.iris_bounds_check_abort" {{
entry:
  call void @iris_embedded_fault(i32 4)
  unreachable
}}

define void @iris_bounds_check(i64 %index, i64 %size) nounwind section ".text.iris_bounds_check" {{
entry:
  %negative = icmp slt i64 %index, 0
  %past_end = icmp sge i64 %index, %size
  %invalid = or i1 %negative, %past_end
  br i1 %invalid, label %fault, label %ok
fault:
  call void @iris_bounds_check_abort(i64 %index, i64 %size)
  unreachable
ok:
  ret void
}}

define i64 @iris_pow_i64(i64 %base, i64 %exp) nounwind section ".text.iris_pow_i64" {{
entry:
  %negative = icmp slt i64 %exp, 0
  br i1 %negative, label %zero, label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next_i, %body ]
  %acc = phi i64 [ 1, %entry ], [ %next_acc, %body ]
  %done = icmp eq i64 %i, %exp
  br i1 %done, label %result, label %body
body:
  %next_acc = call i64 @iris_mul_checked(i64 %acc, i64 %base)
  %next_i = add i64 %i, 1
  br label %loop
result:
  ret i64 %acc
zero:
  ret i64 0
}}

define i64 @iris_min_i64(i64 %a, i64 %b) nounwind section ".text.iris_min_i64" {{
  %c = icmp slt i64 %a, %b
  %r = select i1 %c, i64 %a, i64 %b
  ret i64 %r
}}
define i64 @iris_max_i64(i64 %a, i64 %b) nounwind section ".text.iris_max_i64" {{
  %c = icmp sgt i64 %a, %b
  %r = select i1 %c, i64 %a, i64 %b
  ret i64 %r
}}
define i64 @iris_abs_i64(i64 %value) nounwind section ".text.iris_abs_i64" {{
  %negative = icmp slt i64 %value, 0
  %negated = sub i64 0, %value
  %result = select i1 %negative, i64 %negated, i64 %value
  ret i64 %result
}}
define double @iris_sign_f64(double %value) nounwind section ".text.iris_sign_f64" {{
  %positive = fcmp ogt double %value, 0.0
  %negative = fcmp olt double %value, 0.0
  %negative_value = select i1 %negative, double -1.0, double 0.0
  %result = select i1 %positive, double 1.0, double %negative_value
  ret double %result
}}
"#,
        triple = target.triple()
    )
}

fn llvm_c_string(value: &str) -> String {
    let mut escaped = String::new();
    for byte in value.bytes() {
        match byte {
            b' '..=b'~' if byte != b'"' && byte != b'\\' => escaped.push(byte as char),
            _ => escaped.push_str(&format!("\\{byte:02X}")),
        }
    }
    escaped.push_str("\\00");
    escaped
}

fn embedded_entry_symbol(proof: &EmbeddedProof) -> String {
    format!("iris_embedded_entry_{}", proof.entry)
}

fn embedded_harness_ir(proof: &EmbeddedProof) -> String {
    let version = HARDWARE_REPORT_VERSION;
    let target = proof.target.preset();
    let fingerprint = &proof.fingerprint;
    let version_len = version.len() + 1;
    let target_len = target.len() + 1;
    let fingerprint_len = fingerprint.len() + 1;
    let embedded_entry = embedded_entry_symbol(proof);
    format!(
        r#"; IRIS board-validation harness
target datalayout = "{layout}"
target triple = "{triple}"

@.iris_hw_version = private unnamed_addr constant [{version_len} x i8] c"{version_value}", align 1
@.iris_hw_target = private unnamed_addr constant [{target_len} x i8] c"{target_value}", align 1
@.iris_hw_fingerprint = private unnamed_addr constant [{fingerprint_len} x i8] c"{fingerprint_value}", align 1
@iris_embedded_fault_code = external global i32

declare i64 @{entry}()
declare void @iris_board_validation_report(ptr, ptr, ptr, i64, i64, i32)

define void @iris_embedded_validation_run() nounwind {{
entry:
  %result = call i64 @{entry}()
  %faults = load volatile i32, ptr @iris_embedded_fault_code, align 4
  call void @iris_board_validation_report(
    ptr @.iris_hw_version,
    ptr @.iris_hw_target,
    ptr @.iris_hw_fingerprint,
    i64 %result,
    i64 0,
    i32 %faults)
  ret void
}}
"#,
        layout = super::llvm_ir::target_data_layout(proof.target.triple()),
        triple = proof.target.triple(),
        version_value = llvm_c_string(version),
        target_value = llvm_c_string(target),
        fingerprint_value = llvm_c_string(fingerprint),
        entry = embedded_entry,
    )
}

fn embedded_header(proof: &EmbeddedProof) -> String {
    let embedded_entry = embedded_entry_symbol(proof);
    format!(
        "#ifndef IRIS_EMBEDDED_H\n#define IRIS_EMBEDDED_H\n\n#include <stdint.h>\n\n#define IRIS_EMBEDDED_REPORT_VERSION \"{}\"\n#define IRIS_EMBEDDED_TARGET \"{}\"\n#define IRIS_EMBEDDED_FINGERPRINT \"{}\"\n#define IRIS_EMBEDDED_FIXED_ARRAY_BYTES {}ULL\n#define IRIS_EMBEDDED_ENTRY_SYMBOL {}\n\nint64_t {}(void);\nextern volatile int32_t iris_embedded_fault_code;\nextern const int64_t iris_embedded_allocation_count;\nvoid iris_embedded_validation_run(void);\nvoid iris_board_validation_report(const char* version, const char* target, const char* fingerprint, int64_t result, int64_t allocations, int32_t faults);\n\nint64_t iris_embedded_gpio_read(int64_t pin);\nint64_t iris_embedded_gpio_write(int64_t pin, int64_t value);\nint64_t iris_embedded_adc_read(int64_t channel);\nint64_t iris_embedded_pwm_write(int64_t channel, int64_t duty);\nint64_t iris_embedded_monotonic_us(void);\nint64_t iris_embedded_watchdog_kick(void);\nint64_t iris_embedded_uart_write_byte(int64_t byte);\n\n#endif\n",
        HARDWARE_REPORT_VERSION,
        proof.target.preset(),
        proof.fingerprint,
        proof.fixed_array_bytes,
        embedded_entry,
        embedded_entry
    )
}

fn embedded_manifest(proof: &EmbeddedProof) -> String {
    let functions = proof
        .reachable_functions
        .iter()
        .map(|name| format!("\"{}\"", name.replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{\n  \"schema\": \"iris-embedded/1\",\n  \"target\": \"{}\",\n  \"triple\": \"{}\",\n  \"entry\": \"{}\",\n  \"fingerprint\": \"{}\",\n  \"allocation_free\": true,\n  \"recursive_calls\": false,\n  \"fixed_array_bytes\": {},\n  \"reachable_functions\": [{}]\n}}\n",
        proof.target.preset(),
        proof.target.triple(),
        proof.entry,
        proof.fingerprint,
        proof.fixed_array_bytes,
        functions
    )
}

/// Parse a line emitted by a real board harness.  Cross-compilation alone never
/// creates this report; callers must capture it from the flashed device.
pub fn verify_hardware_report(
    line: &str,
    proof: &EmbeddedProof,
) -> Result<HardwareValidationReport, CodegenError> {
    let mut fields = line.split_whitespace();
    if fields.next() != Some(HARDWARE_REPORT_VERSION) {
        return Err(codegen_error(
            "hardware report has an unsupported protocol version",
        ));
    }
    let values = fields
        .filter_map(|field| field.split_once('='))
        .collect::<HashMap<_, _>>();
    let target = values
        .get("target")
        .copied()
        .and_then(EmbeddedTarget::parse)
        .ok_or_else(|| codegen_error("hardware report is missing a supported target"))?;
    let fingerprint = values
        .get("fingerprint")
        .ok_or_else(|| codegen_error("hardware report is missing its build fingerprint"))?
        .to_string();
    let result = values
        .get("result")
        .ok_or_else(|| codegen_error("hardware report is missing result"))?
        .parse::<i64>()
        .map_err(|_| codegen_error("hardware report result is not an integer"))?;
    let allocations = values
        .get("allocations")
        .ok_or_else(|| codegen_error("hardware report is missing allocation count"))?
        .parse::<u64>()
        .map_err(|_| codegen_error("hardware report allocation count is not an integer"))?;
    let faults = values
        .get("faults")
        .ok_or_else(|| codegen_error("hardware report is missing fault count"))?
        .parse::<u32>()
        .map_err(|_| codegen_error("hardware report fault count is not an integer"))?;

    if target != proof.target || fingerprint != proof.fingerprint {
        return Err(codegen_error(
            "hardware report does not match the target and fingerprint of this build",
        ));
    }
    if allocations != 0 || faults != 0 || result != 0 {
        return Err(codegen_error(format!(
            "hardware validation failed: result={}, allocations={}, faults={}",
            result, allocations, faults
        )));
    }
    Ok(HardwareValidationReport {
        target,
        fingerprint,
        result,
        allocations,
        faults,
    })
}
