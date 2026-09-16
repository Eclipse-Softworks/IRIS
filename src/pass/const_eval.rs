//! Compile-Time Constant Evaluation (CTFE) engine for IRIS.
//!
//! Evaluates constant expressions at compile time, supporting:
//! - Arithmetic and logic operations on scalars (`i64`, `f64`, `bool`, `str`)
//! - Array length expressions `[T; N]` and compile-time shape calculations
//! - Global constant (`const`) definitions and topological dependency resolution
//! - Compile-time assertions and array bounds validation

use crate::error::PassError;
use crate::parser::ast::{
    AstBinOp, AstBlock, AstExpr, AstFunction, AstModule, AstScalarKind, AstStmt, AstType,
    AstUnaryOp,
};
use std::collections::{HashMap, HashSet};

/// A compile-time evaluated constant value.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Tuple(Vec<ConstValue>),
    Array(Vec<ConstValue>),
}

impl ConstValue {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            ConstValue::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_usize(&self) -> Option<usize> {
        match self {
            ConstValue::Int(n) if *n >= 0 => Some(*n as usize),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ConstValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            ConstValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ConstValue::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            ConstValue::Int(_) => "i64",
            ConstValue::Float(_) => "f64",
            ConstValue::Bool(_) => "bool",
            ConstValue::Str(_) => "str",
            ConstValue::Tuple(_) => "tuple",
            ConstValue::Array(_) => "array",
        }
    }
}

/// Errors that occur during compile-time constant evaluation.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstEvalError {
    NonConstExpr(String),
    TypeMismatch {
        expected: &'static str,
        found: String,
    },
    DivisionByZero,
    NegativeArrayLength(i64),
    UndefinedIdentifier(String),
    CycleDetected(String),
    IndexOutOfBounds {
        index: usize,
        len: usize,
    },
    Custom(String),
}

impl std::fmt::Display for ConstEvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConstEvalError::NonConstExpr(desc) => {
                write!(f, "expression is not a compile-time constant: {}", desc)
            }
            ConstEvalError::TypeMismatch { expected, found } => {
                write!(
                    f,
                    "type mismatch in const evaluation: expected {}, found {}",
                    expected, found
                )
            }
            ConstEvalError::DivisionByZero => {
                write!(f, "attempted to divide by zero in constant expression")
            }
            ConstEvalError::NegativeArrayLength(len) => {
                write!(f, "array length must be non-negative, found {}", len)
            }
            ConstEvalError::UndefinedIdentifier(name) => {
                write!(
                    f,
                    "undefined constant or symbol in constant expression: '{}'",
                    name
                )
            }
            ConstEvalError::CycleDetected(cycle) => {
                write!(
                    f,
                    "circular dependency detected in constant definition: {}",
                    cycle
                )
            }
            ConstEvalError::IndexOutOfBounds { index, len } => {
                write!(
                    f,
                    "index out of bounds in const evaluation: index {} length {}",
                    index, len
                )
            }
            ConstEvalError::Custom(msg) => write!(f, "const evaluation error: {}", msg),
        }
    }
}

impl std::error::Error for ConstEvalError {}

/// Compile-time constant evaluator environment.
#[derive(Debug, Clone, Default)]
pub struct ConstEvaluator {
    env: HashMap<String, ConstValue>,
    functions: HashMap<String, AstFunction>,
    call_depth: usize,
}

impl ConstEvaluator {
    pub fn new() -> Self {
        Self {
            env: HashMap::new(),
            functions: HashMap::new(),
            call_depth: 0,
        }
    }

    pub fn with_env(env: HashMap<String, ConstValue>) -> Self {
        Self {
            env,
            functions: HashMap::new(),
            call_depth: 0,
        }
    }

    pub fn with_module(module: &AstModule) -> Self {
        let mut eval = Self::new();
        for func in &module.functions {
            if func.is_const {
                eval.register_function(func.clone());
            }
        }
        for c in &module.consts {
            if let Ok(val) = eval.eval_expr(&c.value) {
                eval.bind(c.name.name.clone(), val);
            }
        }
        eval
    }

    pub fn register_function(&mut self, func: AstFunction) {
        self.functions.insert(func.name.name.clone(), func);
    }

    pub fn bind(&mut self, name: impl Into<String>, val: ConstValue) {
        self.env.insert(name.into(), val);
    }

    pub fn get(&self, name: &str) -> Option<&ConstValue> {
        self.env.get(name)
    }

    /// Evaluates an AST expression into a `ConstValue`.
    pub fn eval_expr(&self, expr: &AstExpr) -> Result<ConstValue, ConstEvalError> {
        match expr {
            AstExpr::IntLit { value, .. } => Ok(ConstValue::Int(*value)),
            AstExpr::FloatLit { value, .. } => Ok(ConstValue::Float(*value)),
            AstExpr::BoolLit { value, .. } => Ok(ConstValue::Bool(*value)),
            AstExpr::StringLit { value, .. } => Ok(ConstValue::Str(value.clone())),
            AstExpr::Ident(ident) => self
                .env
                .get(&ident.name)
                .cloned()
                .ok_or_else(|| ConstEvalError::UndefinedIdentifier(ident.name.clone())),
            AstExpr::UnaryOp {
                op, expr: inner, ..
            } => {
                let v = self.eval_expr(inner)?;
                match (op, v) {
                    (AstUnaryOp::Neg, ConstValue::Int(n)) => Ok(ConstValue::Int(-n)),
                    (AstUnaryOp::Neg, ConstValue::Float(f)) => Ok(ConstValue::Float(-f)),
                    (AstUnaryOp::Not, ConstValue::Bool(b)) => Ok(ConstValue::Bool(!b)),
                    (op, other) => Err(ConstEvalError::TypeMismatch {
                        expected: match op {
                            AstUnaryOp::Neg => "number",
                            AstUnaryOp::Not => "bool",
                        },
                        found: other.type_name().to_string(),
                    }),
                }
            }
            AstExpr::BinOp { op, lhs, rhs, .. } => {
                // Short-circuiting for logical operators
                if *op == AstBinOp::And {
                    let l = self.eval_expr(lhs)?;
                    let b = l.as_bool().ok_or_else(|| ConstEvalError::TypeMismatch {
                        expected: "bool",
                        found: l.type_name().to_string(),
                    })?;
                    if !b {
                        return Ok(ConstValue::Bool(false));
                    }
                    let r = self.eval_expr(rhs)?;
                    let rb = r.as_bool().ok_or_else(|| ConstEvalError::TypeMismatch {
                        expected: "bool",
                        found: r.type_name().to_string(),
                    })?;
                    return Ok(ConstValue::Bool(rb));
                }

                if *op == AstBinOp::Or {
                    let l = self.eval_expr(lhs)?;
                    let b = l.as_bool().ok_or_else(|| ConstEvalError::TypeMismatch {
                        expected: "bool",
                        found: l.type_name().to_string(),
                    })?;
                    if b {
                        return Ok(ConstValue::Bool(true));
                    }
                    let r = self.eval_expr(rhs)?;
                    let rb = r.as_bool().ok_or_else(|| ConstEvalError::TypeMismatch {
                        expected: "bool",
                        found: r.type_name().to_string(),
                    })?;
                    return Ok(ConstValue::Bool(rb));
                }

                let left = self.eval_expr(lhs)?;
                let right = self.eval_expr(rhs)?;

                match (op, left, right) {
                    // Int arithmetic
                    (AstBinOp::Add, ConstValue::Int(a), ConstValue::Int(b)) => {
                        Ok(ConstValue::Int(a.wrapping_add(b)))
                    }
                    (AstBinOp::Sub, ConstValue::Int(a), ConstValue::Int(b)) => {
                        Ok(ConstValue::Int(a.wrapping_sub(b)))
                    }
                    (AstBinOp::Mul, ConstValue::Int(a), ConstValue::Int(b)) => {
                        Ok(ConstValue::Int(a.wrapping_mul(b)))
                    }
                    (AstBinOp::Div, ConstValue::Int(a), ConstValue::Int(b)) => {
                        if b == 0 {
                            Err(ConstEvalError::DivisionByZero)
                        } else {
                            Ok(ConstValue::Int(a.wrapping_div(b)))
                        }
                    }
                    (AstBinOp::Mod, ConstValue::Int(a), ConstValue::Int(b)) => {
                        if b == 0 {
                            Err(ConstEvalError::DivisionByZero)
                        } else {
                            Ok(ConstValue::Int(a.wrapping_rem(b)))
                        }
                    }

                    // Float arithmetic
                    (AstBinOp::Add, ConstValue::Float(a), ConstValue::Float(b)) => {
                        Ok(ConstValue::Float(a + b))
                    }
                    (AstBinOp::Sub, ConstValue::Float(a), ConstValue::Float(b)) => {
                        Ok(ConstValue::Float(a - b))
                    }
                    (AstBinOp::Mul, ConstValue::Float(a), ConstValue::Float(b)) => {
                        Ok(ConstValue::Float(a * b))
                    }
                    (AstBinOp::Div, ConstValue::Float(a), ConstValue::Float(b)) => {
                        Ok(ConstValue::Float(a / b))
                    }

                    // String concatenation
                    (AstBinOp::Add, ConstValue::Str(a), ConstValue::Str(b)) => {
                        let mut s = a;
                        s.push_str(&b);
                        Ok(ConstValue::Str(s))
                    }

                    // Comparisons
                    (AstBinOp::CmpEq, a, b) => Ok(ConstValue::Bool(a == b)),
                    (AstBinOp::CmpNe, a, b) => Ok(ConstValue::Bool(a != b)),
                    (AstBinOp::CmpLt, ConstValue::Int(a), ConstValue::Int(b)) => {
                        Ok(ConstValue::Bool(a < b))
                    }
                    (AstBinOp::CmpLe, ConstValue::Int(a), ConstValue::Int(b)) => {
                        Ok(ConstValue::Bool(a <= b))
                    }
                    (AstBinOp::CmpGt, ConstValue::Int(a), ConstValue::Int(b)) => {
                        Ok(ConstValue::Bool(a > b))
                    }
                    (AstBinOp::CmpGe, ConstValue::Int(a), ConstValue::Int(b)) => {
                        Ok(ConstValue::Bool(a >= b))
                    }
                    (AstBinOp::CmpLt, ConstValue::Float(a), ConstValue::Float(b)) => {
                        Ok(ConstValue::Bool(a < b))
                    }
                    (AstBinOp::CmpLe, ConstValue::Float(a), ConstValue::Float(b)) => {
                        Ok(ConstValue::Bool(a <= b))
                    }
                    (AstBinOp::CmpGt, ConstValue::Float(a), ConstValue::Float(b)) => {
                        Ok(ConstValue::Bool(a > b))
                    }
                    (AstBinOp::CmpGe, ConstValue::Float(a), ConstValue::Float(b)) => {
                        Ok(ConstValue::Bool(a >= b))
                    }

                    (op, l, r) => Err(ConstEvalError::TypeMismatch {
                        expected: "compatible operands for binary op",
                        found: format!("{} {:?} {}", l.type_name(), op, r.type_name()),
                    }),
                }
            }
            AstExpr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                let cond_val = self.eval_expr(cond)?;
                let is_true = cond_val
                    .as_bool()
                    .ok_or_else(|| ConstEvalError::TypeMismatch {
                        expected: "bool",
                        found: cond_val.type_name().to_string(),
                    })?;

                if is_true {
                    self.eval_block(then_block)
                } else if let Some(alt) = else_block {
                    self.eval_block(alt)
                } else {
                    Ok(ConstValue::Tuple(vec![]))
                }
            }
            AstExpr::Block(block) => self.eval_block(block),
            AstExpr::Tuple { elements, .. } => {
                let vals: Result<Vec<_>, _> = elements.iter().map(|e| self.eval_expr(e)).collect();
                Ok(ConstValue::Tuple(vals?))
            }
            AstExpr::TupleIndex { base, index, .. } => {
                let base_val = self.eval_expr(base)?;
                match base_val {
                    ConstValue::Tuple(elements) => {
                        if *index < elements.len() {
                            Ok(elements[*index].clone())
                        } else {
                            Err(ConstEvalError::IndexOutOfBounds {
                                index: *index,
                                len: elements.len(),
                            })
                        }
                    }
                    other => Err(ConstEvalError::TypeMismatch {
                        expected: "tuple",
                        found: other.type_name().to_string(),
                    }),
                }
            }
            AstExpr::ArrayLit { elems, .. } => {
                let vals: Result<Vec<_>, _> = elems.iter().map(|e| self.eval_expr(e)).collect();
                Ok(ConstValue::Array(vals?))
            }
            AstExpr::Index { base, indices, .. } => {
                let base_val = self.eval_expr(base)?;
                if indices.len() != 1 {
                    return Err(ConstEvalError::NonConstExpr(
                        "multi-dimensional indexing not supported in CTFE".into(),
                    ));
                }
                let idx_val = self.eval_expr(&indices[0])?;
                let idx = idx_val
                    .as_usize()
                    .ok_or_else(|| ConstEvalError::TypeMismatch {
                        expected: "usize",
                        found: idx_val.type_name().to_string(),
                    })?;

                match base_val {
                    ConstValue::Array(elements) => {
                        if idx < elements.len() {
                            Ok(elements[idx].clone())
                        } else {
                            Err(ConstEvalError::IndexOutOfBounds {
                                index: idx,
                                len: elements.len(),
                            })
                        }
                    }
                    other => Err(ConstEvalError::TypeMismatch {
                        expected: "array",
                        found: other.type_name().to_string(),
                    }),
                }
            }
            AstExpr::Cast {
                expr: inner, ty, ..
            } => {
                let v = self.eval_expr(inner)?;
                match (v, ty) {
                    (
                        ConstValue::Int(n),
                        AstType::Scalar(crate::parser::ast::AstScalarKind::F64, _),
                    ) => Ok(ConstValue::Float(n as f64)),
                    (
                        ConstValue::Int(n),
                        AstType::Scalar(crate::parser::ast::AstScalarKind::F32, _),
                    ) => Ok(ConstValue::Float(n as f64)),
                    (ConstValue::Float(f), AstType::Scalar(k, _)) if is_int_scalar(k) => {
                        Ok(ConstValue::Int(f as i64))
                    }
                    (ConstValue::Int(n), AstType::Scalar(k, _)) if is_int_scalar(k) => {
                        Ok(ConstValue::Int(n))
                    }
                    (other, _) => Ok(other),
                }
            }
            AstExpr::Call { callee, args, .. } => {
                if callee.name == "static_assert" || callee.name == "const_assert" {
                    if args.is_empty() {
                        return Err(ConstEvalError::Custom(
                            "static_assert requires at least 1 boolean argument".into(),
                        ));
                    }
                    let cond = self.eval_expr(&args[0])?;
                    let passed = cond.as_bool().ok_or_else(|| ConstEvalError::TypeMismatch {
                        expected: "bool",
                        found: cond.type_name().to_string(),
                    })?;
                    if !passed {
                        let msg = if args.len() > 1 {
                            match self.eval_expr(&args[1])? {
                                ConstValue::Str(s) => s,
                                other => format!("static assertion failed: {:?}", other),
                            }
                        } else {
                            "static assertion failed".to_string()
                        };
                        return Err(ConstEvalError::Custom(msg));
                    }
                    return Ok(ConstValue::Bool(true));
                }

                if self.call_depth >= 256 {
                    return Err(ConstEvalError::Custom(
                        "CTFE recursion depth limit exceeded (256)".into(),
                    ));
                }

                let func = self.functions.get(&callee.name).cloned().ok_or_else(|| {
                    ConstEvalError::UndefinedIdentifier(format!("const function '{}'", callee.name))
                })?;

                if !func.is_const {
                    return Err(ConstEvalError::NonConstExpr(format!(
                        "function '{}' is not declared with 'const def'",
                        callee.name
                    )));
                }

                if args.len() != func.params.len() {
                    return Err(ConstEvalError::Custom(format!(
                        "const function '{}' expects {} arguments, got {}",
                        callee.name,
                        func.params.len(),
                        args.len()
                    )));
                }

                let mut arg_vals = Vec::with_capacity(args.len());
                for arg in args {
                    arg_vals.push(self.eval_expr(arg)?);
                }

                let mut call_eval = self.clone();
                call_eval.call_depth += 1;
                for (param, val) in func.params.iter().zip(arg_vals) {
                    call_eval.bind(param.name.name.clone(), val);
                }

                call_eval.eval_block(&func.body)
            }
            other => Err(ConstEvalError::NonConstExpr(format!(
                "{:?}",
                std::mem::discriminant(other)
            ))),
        }
    }

    /// Evaluates a block in a scoped sub-environment.
    pub fn eval_block(&self, block: &AstBlock) -> Result<ConstValue, ConstEvalError> {
        let mut scoped_eval = self.clone();
        scoped_eval.eval_block_mut(block)
    }

    /// Evaluates a block mutating the current environment.
    pub fn eval_block_mut(&mut self, block: &AstBlock) -> Result<ConstValue, ConstEvalError> {
        for stmt in &block.stmts {
            match stmt {
                AstStmt::Let { name, init, .. } => {
                    let val = self.eval_expr(init)?;
                    self.bind(name.name.clone(), val);
                }
                AstStmt::LetTuple { names, init, .. } => {
                    let val = self.eval_expr(init)?;
                    if let ConstValue::Tuple(elements) = val {
                        if elements.len() != names.len() {
                            return Err(ConstEvalError::Custom(format!(
                                "tuple destructuring mismatch: expected {} values, got {}",
                                names.len(),
                                elements.len()
                            )));
                        }
                        for (ident, v) in names.iter().zip(elements) {
                            self.bind(ident.name.clone(), v);
                        }
                    } else {
                        return Err(ConstEvalError::TypeMismatch {
                            expected: "tuple",
                            found: val.type_name().to_string(),
                        });
                    }
                }
                AstStmt::Assign {
                    target, value, op, ..
                } => {
                    let new_val = self.eval_expr(value)?;
                    if let AstExpr::Ident(ident) = target.as_ref() {
                        let final_val = if let Some(bin_op) = op {
                            let curr = self.get(&ident.name).cloned().ok_or_else(|| {
                                ConstEvalError::UndefinedIdentifier(ident.name.clone())
                            })?;
                            match (bin_op, curr, new_val) {
                                (
                                    crate::ir::instr::BinOp::Add,
                                    ConstValue::Int(a),
                                    ConstValue::Int(b),
                                ) => ConstValue::Int(a.wrapping_add(b)),
                                (
                                    crate::ir::instr::BinOp::Sub,
                                    ConstValue::Int(a),
                                    ConstValue::Int(b),
                                ) => ConstValue::Int(a.wrapping_sub(b)),
                                (
                                    crate::ir::instr::BinOp::Mul,
                                    ConstValue::Int(a),
                                    ConstValue::Int(b),
                                ) => ConstValue::Int(a.wrapping_mul(b)),
                                _ => {
                                    return Err(ConstEvalError::NonConstExpr(
                                        "unsupported assignment op in CTFE".into(),
                                    ))
                                }
                            }
                        } else {
                            new_val
                        };
                        self.bind(ident.name.clone(), final_val);
                    } else {
                        return Err(ConstEvalError::NonConstExpr(
                            "non-identifier assignment target in CTFE".into(),
                        ));
                    }
                }
                AstStmt::While { cond, body, .. } => {
                    let mut iterations = 0;
                    while self.eval_expr(cond)?.as_bool().unwrap_or(false) {
                        iterations += 1;
                        if iterations > 100_000 {
                            return Err(ConstEvalError::Custom(
                                "CTFE loop limit exceeded (100,000 iterations)".into(),
                            ));
                        }
                        self.eval_block_mut(body)?;
                    }
                }
                AstStmt::ForRange {
                    var,
                    start,
                    end,
                    inclusive,
                    step,
                    body,
                    ..
                } => {
                    let s = self.eval_expr(start)?.as_int().ok_or_else(|| {
                        ConstEvalError::TypeMismatch {
                            expected: "i64",
                            found: "non-int".into(),
                        }
                    })?;
                    let e = self.eval_expr(end)?.as_int().ok_or_else(|| {
                        ConstEvalError::TypeMismatch {
                            expected: "i64",
                            found: "non-int".into(),
                        }
                    })?;
                    let st = if let Some(step_expr) = step {
                        self.eval_expr(step_expr)?.as_int().unwrap_or(1)
                    } else {
                        1
                    };
                    let mut curr = s;
                    let mut iterations = 0;
                    while if *inclusive { curr <= e } else { curr < e } {
                        iterations += 1;
                        if iterations > 100_000 {
                            return Err(ConstEvalError::Custom(
                                "CTFE loop limit exceeded (100,000 iterations)".into(),
                            ));
                        }
                        self.bind(var.name.clone(), ConstValue::Int(curr));
                        self.eval_block_mut(body)?;
                        curr += st;
                    }
                }
                AstStmt::Return { value, .. } => {
                    if let Some(val_expr) = value {
                        return self.eval_expr(val_expr);
                    } else {
                        return Ok(ConstValue::Tuple(vec![]));
                    }
                }
                AstStmt::Expr(expr) => {
                    let _ = self.eval_expr(expr)?;
                }
                _ => {
                    return Err(ConstEvalError::NonConstExpr(
                        "statement cannot be evaluated in CTFE".into(),
                    ))
                }
            }
        }

        if let Some(tail) = &block.tail {
            self.eval_expr(tail)
        } else {
            Ok(ConstValue::Tuple(vec![]))
        }
    }

    /// Evaluates an expression as an integer length for arrays (`[T; N]`).
    pub fn eval_array_len(&self, expr: &AstExpr) -> Result<usize, ConstEvalError> {
        let val = self.eval_expr(expr)?;
        match val {
            ConstValue::Int(n) if n >= 0 => Ok(n as usize),
            ConstValue::Int(n) => Err(ConstEvalError::NegativeArrayLength(n)),
            other => Err(ConstEvalError::TypeMismatch {
                expected: "non-negative integer",
                found: other.type_name().to_string(),
            }),
        }
    }
}

/// A compiler pass that resolves global `const` declarations and evaluates
/// array sizes / shape parameters across the `AstModule`.
#[derive(Default)]
pub struct ConstEvalPass;

impl ConstEvalPass {
    pub fn new() -> Self {
        Self
    }

    /// Analyzes an `AstModule`, evaluates all top-level `const` declarations in dependency
    /// order, and updates constant generic array dimensions in-place.
    pub fn run(&mut self, module: &mut AstModule) -> Result<ConstEvaluator, PassError> {
        let mut evaluator = ConstEvaluator::new();
        let mut pending: HashMap<String, &AstExpr> = HashMap::new();

        for c in &module.consts {
            pending.insert(c.name.name.clone(), &c.value);
        }

        let mut resolved: HashSet<String> = HashSet::new();
        let mut in_progress: Vec<String> = Vec::new();

        fn resolve_const(
            name: &str,
            pending: &HashMap<String, &AstExpr>,
            evaluator: &mut ConstEvaluator,
            resolved: &mut HashSet<String>,
            in_progress: &mut Vec<String>,
        ) -> Result<(), PassError> {
            if resolved.contains(name) {
                return Ok(());
            }
            if let Some(pos) = in_progress.iter().position(|x| x == name) {
                let cycle = in_progress[pos..].join(" -> ") + " -> " + name;
                return Err(PassError::ConstEvalError {
                    detail: format!("Circular constant dependency: {}", cycle),
                });
            }

            let expr = pending.get(name).ok_or_else(|| PassError::ConstEvalError {
                detail: format!("Undefined constant: {}", name),
            })?;

            in_progress.push(name.to_string());

            // Collect free identifiers in `expr` that match pending constants
            let deps = collect_free_idents(expr);
            for dep in deps {
                if pending.contains_key(&dep) {
                    resolve_const(&dep, pending, evaluator, resolved, in_progress)?;
                }
            }

            in_progress.pop();

            let val = evaluator
                .eval_expr(expr)
                .map_err(|e| PassError::ConstEvalError {
                    detail: format!("Error evaluating constant '{}': {}", name, e),
                })?;

            evaluator.bind(name, val);
            resolved.insert(name.to_string());
            Ok(())
        }

        let const_names: Vec<String> = pending.keys().cloned().collect();
        for name in const_names {
            resolve_const(
                &name,
                &pending,
                &mut evaluator,
                &mut resolved,
                &mut in_progress,
            )?;
        }

        // Simplify array types in structs and functions
        for s in &mut module.structs {
            for f in &mut s.fields {
                simplify_type(&mut f.ty, &evaluator);
            }
        }

        for func in &mut module.functions {
            for p in &mut func.params {
                simplify_type(&mut p.ty, &evaluator);
            }
            simplify_type(&mut func.return_ty, &evaluator);
            simplify_block(&mut func.body, &evaluator);
        }

        Ok(evaluator)
    }
}

fn collect_free_idents(expr: &AstExpr) -> Vec<String> {
    let mut idents = Vec::new();
    match expr {
        AstExpr::Ident(id) => idents.push(id.name.clone()),
        AstExpr::UnaryOp { expr, .. } => idents.extend(collect_free_idents(expr)),
        AstExpr::BinOp { lhs, rhs, .. } => {
            idents.extend(collect_free_idents(lhs));
            idents.extend(collect_free_idents(rhs));
        }
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            idents.extend(collect_free_idents(cond));
            for stmt in &then_block.stmts {
                if let AstStmt::Expr(e) = stmt {
                    idents.extend(collect_free_idents(e));
                }
            }
            if let Some(t) = &then_block.tail {
                idents.extend(collect_free_idents(t));
            }
            if let Some(eb) = else_block {
                for stmt in &eb.stmts {
                    if let AstStmt::Expr(e) = stmt {
                        idents.extend(collect_free_idents(e));
                    }
                }
                if let Some(t) = &eb.tail {
                    idents.extend(collect_free_idents(t));
                }
            }
        }
        AstExpr::Tuple { elements, .. } => {
            for el in elements {
                idents.extend(collect_free_idents(el));
            }
        }
        AstExpr::ArrayLit { elems, .. } => {
            for el in elems {
                idents.extend(collect_free_idents(el));
            }
        }
        _ => {}
    }
    idents
}

fn simplify_type(ty: &mut AstType, evaluator: &ConstEvaluator) {
    match ty {
        AstType::Array {
            elem,
            len,
            len_expr,
            ..
        } => {
            simplify_type(elem, evaluator);
            if *len == 0 {
                if let Some(expr) = len_expr {
                    if let Ok(eval_len) = evaluator.eval_array_len(expr) {
                        *len = eval_len;
                    }
                }
            }
        }
        AstType::Tuple(elems, _) => {
            for e in elems {
                simplify_type(e, evaluator);
            }
        }
        AstType::Option(inner, _)
        | AstType::Chan(inner, _)
        | AstType::Atomic(inner, _)
        | AstType::Mutex(inner, _)
        | AstType::Grad(inner, _)
        | AstType::Sparse(inner, _)
        | AstType::List(inner, _)
        | AstType::WeakRef(inner, _)
        | AstType::Ref(inner, _)
        | AstType::RefMut(inner, _)
        | AstType::Slice(inner, _) => {
            simplify_type(inner, evaluator);
        }
        AstType::Result(ok, err, _) | AstType::Map(ok, err, _) => {
            simplify_type(ok, evaluator);
            simplify_type(err, evaluator);
        }
        AstType::Fn { params, ret, .. } => {
            for p in params {
                simplify_type(p, evaluator);
            }
            simplify_type(ret, evaluator);
        }
        _ => {}
    }
}

fn simplify_block(block: &mut AstBlock, evaluator: &ConstEvaluator) {
    for stmt in &mut block.stmts {
        if let AstStmt::Let { ty: Some(ty), .. } = stmt {
            simplify_type(ty, evaluator);
        }
    }
}

fn is_int_scalar(k: &AstScalarKind) -> bool {
    matches!(
        k,
        AstScalarKind::I8
            | AstScalarKind::U8
            | AstScalarKind::I32
            | AstScalarKind::U32
            | AstScalarKind::I64
            | AstScalarKind::U64
            | AstScalarKind::USize
    )
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ast::Ident;
    use crate::parser::lexer::Span;

    fn dummy_span() -> Span {
        Span::at(0)
    }

    fn int_lit(n: i64) -> AstExpr {
        AstExpr::IntLit {
            value: n,
            span: dummy_span(),
        }
    }

    fn bin_op(op: AstBinOp, lhs: AstExpr, rhs: AstExpr) -> AstExpr {
        AstExpr::BinOp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            span: dummy_span(),
        }
    }

    #[test]
    fn test_ctfe_arithmetic() {
        let eval = ConstEvaluator::new();
        // 2 + 3 * 4 = 14
        let expr = bin_op(
            AstBinOp::Add,
            int_lit(2),
            bin_op(AstBinOp::Mul, int_lit(3), int_lit(4)),
        );
        let res = eval.eval_expr(&expr).expect("eval succeeds");
        assert_eq!(res, ConstValue::Int(14));
    }

    #[test]
    fn test_ctfe_division_by_zero() {
        let eval = ConstEvaluator::new();
        let expr = bin_op(AstBinOp::Div, int_lit(10), int_lit(0));
        let err = eval.eval_expr(&expr).unwrap_err();
        assert_eq!(err, ConstEvalError::DivisionByZero);
    }

    #[test]
    fn test_ctfe_array_len_and_env() {
        let mut eval = ConstEvaluator::new();
        eval.bind("BUFFER_SIZE", ConstValue::Int(128));
        eval.bind("FACTOR", ConstValue::Int(2));

        // BUFFER_SIZE * FACTOR = 256
        let expr = bin_op(
            AstBinOp::Mul,
            AstExpr::Ident(Ident {
                name: "BUFFER_SIZE".into(),
                span: dummy_span(),
            }),
            AstExpr::Ident(Ident {
                name: "FACTOR".into(),
                span: dummy_span(),
            }),
        );

        let len = eval.eval_array_len(&expr).expect("array len evaluated");
        assert_eq!(len, 256);
    }

    #[test]
    fn test_ctfe_boolean_logic() {
        let eval = ConstEvaluator::new();
        // 10 > 5 && 3 <= 3 -> true
        let expr = bin_op(
            AstBinOp::And,
            bin_op(AstBinOp::CmpGt, int_lit(10), int_lit(5)),
            bin_op(AstBinOp::CmpLe, int_lit(3), int_lit(3)),
        );
        let res = eval.eval_expr(&expr).expect("eval succeeds");
        assert_eq!(res, ConstValue::Bool(true));
    }

    #[test]
    fn test_ctfe_const_function_and_static_assert() {
        use crate::parser::ast::{AstBlock, AstFunction, AstParam, AstType};
        let mut eval = ConstEvaluator::new();

        // const def square(x: i64) -> i64 { return x * x }
        let square_fn = AstFunction {
            name: Ident {
                name: "square".into(),
                span: dummy_span(),
            },
            is_pub: false,
            type_params: vec![],
            params: vec![AstParam {
                name: Ident {
                    name: "x".into(),
                    span: dummy_span(),
                },
                ty: AstType::Scalar(AstScalarKind::I64, dummy_span()),
                default: None,
            }],
            return_ty: AstType::Scalar(AstScalarKind::I64, dummy_span()),
            effects: vec![],
            body: AstBlock {
                stmts: vec![AstStmt::Return {
                    value: Some(Box::new(bin_op(
                        AstBinOp::Mul,
                        AstExpr::Ident(Ident {
                            name: "x".into(),
                            span: dummy_span(),
                        }),
                        AstExpr::Ident(Ident {
                            name: "x".into(),
                            span: dummy_span(),
                        }),
                    ))),
                    span: dummy_span(),
                }],
                tail: None,
                span: dummy_span(),
            },
            span: dummy_span(),
            is_async: false,
            is_const: true,
            attrs: vec![],
            doc_comment: None,
        };
        eval.register_function(square_fn);

        // square(7) -> 49
        let call_expr = AstExpr::Call {
            callee: Ident {
                name: "square".into(),
                span: dummy_span(),
            },
            args: vec![int_lit(7)],
            named_args: vec![],
            span: dummy_span(),
        };
        assert_eq!(eval.eval_expr(&call_expr).unwrap(), ConstValue::Int(49));

        // static_assert(square(7) == 49)
        let assert_ok = AstExpr::Call {
            callee: Ident {
                name: "static_assert".into(),
                span: dummy_span(),
            },
            args: vec![bin_op(AstBinOp::CmpEq, call_expr.clone(), int_lit(49))],
            named_args: vec![],
            span: dummy_span(),
        };
        assert_eq!(eval.eval_expr(&assert_ok).unwrap(), ConstValue::Bool(true));

        // static_assert(square(7) == 50) -> fails!
        let assert_fail = AstExpr::Call {
            callee: Ident {
                name: "static_assert".into(),
                span: dummy_span(),
            },
            args: vec![bin_op(AstBinOp::CmpEq, call_expr, int_lit(50))],
            named_args: vec![],
            span: dummy_span(),
        };
        assert!(eval.eval_expr(&assert_fail).is_err());
    }

    #[test]
    fn test_ctfe_loop_evaluation() {
        let eval = ConstEvaluator::new();
        // let mut sum = 0; for i in 1..=5 { sum += i }; sum -> 15
        let block = AstBlock {
            stmts: vec![
                AstStmt::Let {
                    name: Ident {
                        name: "sum".into(),
                        span: dummy_span(),
                    },
                    ty: None,
                    init: Box::new(int_lit(0)),
                    is_var: true,
                    span: dummy_span(),
                },
                AstStmt::ForRange {
                    label: None,
                    var: Ident {
                        name: "i".into(),
                        span: dummy_span(),
                    },
                    start: Box::new(int_lit(1)),
                    end: Box::new(int_lit(5)),
                    inclusive: true,
                    step: None,
                    body: AstBlock {
                        stmts: vec![AstStmt::Assign {
                            target: Box::new(AstExpr::Ident(Ident {
                                name: "sum".into(),
                                span: dummy_span(),
                            })),
                            op: Some(crate::ir::instr::BinOp::Add),
                            value: Box::new(AstExpr::Ident(Ident {
                                name: "i".into(),
                                span: dummy_span(),
                            })),
                            span: dummy_span(),
                        }],
                        tail: None,
                        span: dummy_span(),
                    },
                    span: dummy_span(),
                },
            ],
            tail: Some(Box::new(AstExpr::Ident(Ident {
                name: "sum".into(),
                span: dummy_span(),
            }))),
            span: dummy_span(),
        };

        assert_eq!(eval.eval_block(&block).unwrap(), ConstValue::Int(15));
    }
}
