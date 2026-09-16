//! Compiler-hosted typed metaprogramming services.
//!
//! IRIS programs construct or obtain source, then ask the existing Rust
//! compiler to parse, type-check, inspect, and emit it. This deliberately does
//! not require a self-hosted compiler or a second, approximate type system.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::codegen::hot_swap::AbiFingerprint;
use crate::parser::ast::{AstDim, AstGenericParam, AstScalarKind, AstType};

pub const META_ANALYSIS_SCHEMA: &str = "iris-meta-analysis/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaDiagnostic {
    pub phase: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaParameter {
    pub name: String,
    pub ty: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaFunction {
    pub name: String,
    pub parameters: Vec<MetaParameter>,
    pub return_type: String,
    pub effects: Vec<String>,
    pub type_parameters: Vec<String>,
    pub is_public: bool,
    pub is_async: bool,
    pub is_const: bool,
    pub abi_sha256: Option<String>,
    pub basic_blocks: Option<usize>,
    pub instructions: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaField {
    pub name: String,
    pub ty: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaRecord {
    pub name: String,
    pub fields: Vec<MetaField>,
    pub is_public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaVariant {
    pub name: String,
    pub payload: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaChoice {
    pub name: String,
    pub variants: Vec<MetaVariant>,
    pub is_public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaAnalysis {
    pub schema: String,
    pub module_name: String,
    pub source_sha256: String,
    pub valid: bool,
    pub diagnostics: Vec<MetaDiagnostic>,
    pub functions: Vec<MetaFunction>,
    pub records: Vec<MetaRecord>,
    pub choices: Vec<MetaChoice>,
}

/// Parse and type-check source, retaining declared type/effect information and
/// attaching concrete ABI/CFG information from the optimized IR.
pub fn analyze_source(source: &str, module_name: &str) -> MetaAnalysis {
    let (ast, parse_errors) = crate::compile_with_recovery(source);
    let mut diagnostics: Vec<MetaDiagnostic> = parse_errors
        .iter()
        .map(|error| MetaDiagnostic {
            phase: "parse".to_owned(),
            message: error.to_string(),
        })
        .collect();

    let mut functions: Vec<MetaFunction> = ast
        .functions
        .iter()
        .map(|function| MetaFunction {
            name: function.name.name.clone(),
            parameters: function
                .params
                .iter()
                .map(|parameter| MetaParameter {
                    name: parameter.name.name.clone(),
                    ty: ast_type_name(&parameter.ty),
                })
                .collect(),
            return_type: ast_type_name(&function.return_ty),
            effects: function.effects.clone(),
            type_parameters: function
                .type_params
                .iter()
                .map(generic_parameter_name)
                .collect(),
            is_public: function.is_pub,
            is_async: function.is_async,
            is_const: function.is_const,
            abi_sha256: None,
            basic_blocks: None,
            instructions: None,
        })
        .collect();

    let records = ast
        .structs
        .iter()
        .map(|record| MetaRecord {
            name: record.name.name.clone(),
            fields: record
                .fields
                .iter()
                .map(|field| MetaField {
                    name: field.name.name.clone(),
                    ty: ast_type_name(&field.ty),
                })
                .collect(),
            is_public: record.is_pub,
        })
        .collect();
    let choices = ast
        .enums
        .iter()
        .map(|choice| MetaChoice {
            name: choice.name.name.clone(),
            variants: choice
                .variants
                .iter()
                .map(|variant| MetaVariant {
                    name: variant.name.name.clone(),
                    payload: variant.fields.iter().map(ast_type_name).collect(),
                })
                .collect(),
            is_public: choice.is_pub,
        })
        .collect();

    let mut valid = parse_errors.is_empty();
    if valid {
        match crate::compile_to_module(source, module_name) {
            Ok(module) => {
                for function in &mut functions {
                    let specialization_prefix = format!("{}__", function.name);
                    if let Some(lowered) = module.function_by_name(&function.name).or_else(|| {
                        module
                            .functions()
                            .iter()
                            .find(|candidate| candidate.name.starts_with(&specialization_prefix))
                    }) {
                        function.abi_sha256 = Some(AbiFingerprint::for_function(lowered).sha256);
                        function.basic_blocks = Some(lowered.blocks().len());
                        function.instructions = Some(
                            lowered
                                .blocks()
                                .iter()
                                .map(|block| block.instrs.len())
                                .sum(),
                        );
                        function.parameters = lowered
                            .params
                            .iter()
                            .map(|parameter| MetaParameter {
                                name: parameter.name.clone(),
                                ty: parameter.ty.to_string(),
                            })
                            .collect();
                        function.return_type = lowered.return_ty.to_string();
                    }
                }
            }
            Err(error) => {
                valid = false;
                diagnostics.push(MetaDiagnostic {
                    phase: "compile".to_owned(),
                    message: error.to_string(),
                });
            }
        }
    }

    MetaAnalysis {
        schema: META_ANALYSIS_SCHEMA.to_owned(),
        module_name: module_name.to_owned(),
        source_sha256: digest(source.as_bytes()),
        valid,
        diagnostics,
        functions,
        records,
        choices,
    }
}

pub fn analyze_source_json(source: &str, module_name: &str) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&analyze_source(source, module_name))
}

pub fn emit_ir(source: &str, module_name: &str) -> Result<String, crate::Error> {
    crate::compile(source, module_name, crate::EmitKind::Ir)
}

/// Apply one UTF-8-safe source edit and require the resulting program to pass
/// the complete compiler pipeline before returning it.
pub fn apply_checked_edit(
    source: &str,
    start: usize,
    end: usize,
    replacement: &str,
    module_name: &str,
) -> Result<String, MetaEditError> {
    if start > end
        || end > source.len()
        || !source.is_char_boundary(start)
        || !source.is_char_boundary(end)
    {
        return Err(MetaEditError::Range {
            start,
            end,
            source_bytes: source.len(),
        });
    }
    let mut edited = String::with_capacity(source.len() - (end - start) + replacement.len());
    edited.push_str(&source[..start]);
    edited.push_str(replacement);
    edited.push_str(&source[end..]);
    crate::compile_to_module(&edited, module_name)
        .map_err(|error| MetaEditError::Compile(error.to_string()))?;
    Ok(edited)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaEditError {
    Range {
        start: usize,
        end: usize,
        source_bytes: usize,
    },
    Compile(String),
}

impl std::fmt::Display for MetaEditError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Range {
                start,
                end,
                source_bytes,
            } => write!(
                formatter,
                "invalid UTF-8 source edit {start}..{end} for {source_bytes}-byte program"
            ),
            Self::Compile(detail) => {
                write!(formatter, "edited program failed verification: {detail}")
            }
        }
    }
}

impl std::error::Error for MetaEditError {}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn generic_parameter_name(parameter: &AstGenericParam) -> String {
    match parameter {
        AstGenericParam::Type(name, _, _) | AstGenericParam::Hkt(name, _, _, _) => name.clone(),
        AstGenericParam::Const { name, kind } => {
            format!("const {name}: {}", ast_type_name(kind))
        }
    }
}

pub fn ast_type_name(ty: &AstType) -> String {
    match ty {
        AstType::Scalar(kind, _) => scalar_name(*kind).to_owned(),
        AstType::Tensor { dtype, dims, .. } => {
            let dims = dims
                .iter()
                .map(|dimension| match dimension {
                    AstDim::Literal(value) => value.to_string(),
                    AstDim::Symbol(symbol) => symbol.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("tensor<{}, [{}]>", scalar_name(*dtype), dims)
        }
        AstType::Named(name, _) => name.clone(),
        AstType::Tuple(items, _) => format!(
            "({})",
            items
                .iter()
                .map(ast_type_name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        AstType::Array { elem, len, .. } => format!("[{}; {len}]", ast_type_name(elem)),
        AstType::Option(inner, _) => unary_type("option", inner),
        AstType::Result(ok, error, _) => {
            format!("result<{}, {}>", ast_type_name(ok), ast_type_name(error))
        }
        AstType::Chan(inner, _) => unary_type("chan", inner),
        AstType::Atomic(inner, _) => unary_type("atomic", inner),
        AstType::Mutex(inner, _) => unary_type("mutex", inner),
        AstType::Grad(inner, _) => unary_type("grad", inner),
        AstType::Sparse(inner, _) => unary_type("sparse", inner),
        AstType::List(inner, _) => unary_type("list", inner),
        AstType::Map(key, value, _) => {
            format!("map<{}, {}>", ast_type_name(key), ast_type_name(value))
        }
        AstType::WeakRef(inner, _) => unary_type("weak_ref", inner),
        AstType::Fn {
            params,
            ret,
            effects,
            ..
        } => {
            let effects = if effects.is_empty() {
                String::new()
            } else {
                format!(" effect {}", effects.join(", "))
            };
            format!(
                "({}) -> {}{effects}",
                params
                    .iter()
                    .map(ast_type_name)
                    .collect::<Vec<_>>()
                    .join(", "),
                ast_type_name(ret)
            )
        }
        AstType::Generic { name, args, .. } => format!(
            "{name}<{}>",
            args.iter()
                .map(ast_type_name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        AstType::ConstInt(value, _) => value.to_string(),
        AstType::AssocType {
            base, assoc_name, ..
        } => format!("{base}::{assoc_name}"),
        AstType::DynTrait { trait_name, .. } => format!("dyn {trait_name}"),
        AstType::MaskEffectType { effects, .. } => format!("with {}", effects.join(", ")),
        AstType::Ref(inner, _) => format!("&{}", ast_type_name(inner)),
        AstType::RefMut(inner, _) => format!("&mut {}", ast_type_name(inner)),
        AstType::Slice(inner, _) => format!("[{}]", ast_type_name(inner)),
    }
}

fn unary_type(name: &str, inner: &AstType) -> String {
    format!("{name}<{}>", ast_type_name(inner))
}

fn scalar_name(kind: AstScalarKind) -> &'static str {
    match kind {
        AstScalarKind::F32 => "f32",
        AstScalarKind::F64 => "f64",
        AstScalarKind::I32 => "i32",
        AstScalarKind::I64 => "i64",
        AstScalarKind::Bool => "bool",
        AstScalarKind::U8 => "u8",
        AstScalarKind::I8 => "i8",
        AstScalarKind::U32 => "u32",
        AstScalarKind::U64 => "u64",
        AstScalarKind::USize => "usize",
    }
}
