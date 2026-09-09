//! Supported in-process JIT compilation for IRIS.
//!
//! IRIS IR is emitted as LLVM IR, compiled to object bytes through LLVM-C, and
//! installed directly into LLVM ORC/LLJIT. No compiler subprocess, temporary
//! executable, or child process participates in JIT execution.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use crate::codegen::llvm_orc::{is_orc_jit_available, OrcJitEngine, OrcJitModule};
use crate::error::CodegenError;
use crate::ir::module::IrModule;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JitKey {
    pub module_name: String,
    pub function_name: String,
    pub ir_hash: u64,
}

#[derive(Debug, Clone)]
pub struct JitResult {
    pub output: String,
    pub tier: JitTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitTier {
    Orc,
}

impl std::fmt::Display for JitTier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Orc => formatter.write_str("native ORC (in-process)"),
        }
    }
}

struct CachedJit {
    result: JitResult,
    _generation: Arc<OrcJitModule>,
}

/// Long-lived in-process compiler and resident-code cache.
pub struct JitCompiler {
    cache: HashMap<JitKey, CachedJit>,
}

impl JitCompiler {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Compile and execute `main`, or the first zero-argument function.
    pub fn compile_and_run(&mut self, module: &IrModule) -> Result<JitResult, CodegenError> {
        let function = module
            .functions()
            .iter()
            .find(|function| function.name == "main" && function.params.is_empty())
            .or_else(|| {
                module
                    .functions()
                    .iter()
                    .find(|function| function.params.is_empty())
            })
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "jit".into(),
                detail: "no zero-argument function found to JIT-compile".into(),
            })?;
        let key = JitKey {
            module_name: module.name.clone(),
            function_name: function.name.clone(),
            ir_hash: hash_module(module),
        };
        if let Some(cached) = self.cache.get(&key) {
            return Ok(cached.result.clone());
        }

        // Each cached source module owns an ORC session. This gives modules a
        // real namespace even when they export the same IRIS function names.
        // Hot swapping uses a separate shared-session manager with generation
        // mangling and stable dispatch slots.
        let engine = OrcJitEngine::new()?;
        let generation = Arc::new(engine.load_module(module)?);
        let value = generation.call_zero_arg(&function.name)?;
        let result = JitResult {
            output: format!("{}\n", value),
            tier: JitTier::Orc,
        };
        self.cache.insert(
            key,
            CachedJit {
                result: result.clone(),
                _generation: generation,
            },
        );
        Ok(result)
    }

    pub fn cached_generation_count(&self) -> usize {
        self.cache.len()
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

impl Default for JitCompiler {
    fn default() -> Self {
        Self::new()
    }
}

pub fn emit_jit(module: &IrModule) -> Result<String, CodegenError> {
    let mut compiler = JitCompiler::new();
    let result = compiler.compile_and_run(module)?;
    let mut output = String::new();
    use std::fmt::Write;
    writeln!(output, "; IRIS in-process JIT")?;
    writeln!(output, "; Module: {}", module.name)?;
    writeln!(output, "; IR hash: {:016x}", hash_module(module))?;
    writeln!(output, "; Execution tier: {}", result.tier)?;
    writeln!(output, "; ORC available: {}", is_orc_jit_available())?;
    writeln!(output, ";")?;
    writeln!(output, "; Resident functions:")?;
    for function in module.functions() {
        let params = function
            .params
            .iter()
            .map(|param| format!("{}: {}", param.name, param.ty))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            output,
            ";   {} ({}) -> {}",
            function.name, params, function.return_ty
        )?;
    }
    writeln!(output, ";")?;
    writeln!(output, "; Execution result:")?;
    for line in result.output.lines() {
        writeln!(output, ";   {}", line)?;
    }
    writeln!(output)?;
    output.push_str(&result.output);
    Ok(output)
}

pub fn emit_jit_plan(module: &IrModule) -> Result<String, CodegenError> {
    let mut output = String::new();
    use std::fmt::Write;
    writeln!(output, "; IRIS in-process JIT compilation plan")?;
    writeln!(output, "; Module: {}", module.name)?;
    writeln!(output, "; IR hash: {:016x}", hash_module(module))?;
    writeln!(output, "; ORC available: {}", is_orc_jit_available())?;
    writeln!(output)?;
    writeln!(output, "; Pipeline:")?;
    writeln!(output, ";   1. IRIS IR -> LLVM IR")?;
    writeln!(output, ";   2. LLVM-C -> in-memory native object")?;
    writeln!(output, ";   3. ORC resource-tracked installation")?;
    writeln!(output, ";   4. Typed in-process function invocation")?;
    writeln!(output)?;
    writeln!(output, "; Functions compiled:")?;
    for function in module.functions() {
        let entry = if function.params.is_empty() {
            "[ENTRY] "
        } else {
            ""
        };
        let params = function
            .params
            .iter()
            .map(|param| format!("{}: {}", param.name, param.ty))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            output,
            ";   {}{} ({}) -> {}",
            entry, function.name, params, function.return_ty
        )?;
    }
    Ok(output)
}

fn hash_module(module: &IrModule) -> u64 {
    use crate::codegen::printer::emit_ir_text;
    let ir = emit_ir_text(module).unwrap_or_default();
    hash_str(&ir)
}

fn hash_str(source: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}
