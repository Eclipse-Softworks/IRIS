//! Typed AST genome representation for evolutionary code synthesis in IRIS.
//!
//! A genome represents an evolvable expression tree that is guaranteed to be
//! type-safe and syntactically valid when emitted as IRIS source code.
//! Supports both scalar integer/boolean operations and vectorized SIMD operations
//! with persistent multi-register state memory.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Type of values produced and consumed by genome nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GeneType {
    I64,
    Bool,
    Vec4,
}

/// Unary operations on scalar values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnaryOp {
    Neg,
    Abs,
    Not,
}

impl UnaryOp {
    pub fn operand_type(&self) -> GeneType {
        match self {
            Self::Neg | Self::Abs => GeneType::I64,
            Self::Not => GeneType::Bool,
        }
    }

    pub fn return_type(&self) -> GeneType {
        match self {
            Self::Neg | Self::Abs => GeneType::I64,
            Self::Not => GeneType::Bool,
        }
    }
}

/// Binary operations on scalar values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinaryOp {
    // Arithmetic: i64 x i64 -> i64
    Add,
    Sub,
    Mul,
    DivChecked,
    ModChecked,
    Min,
    Max,
    // Comparison: i64 x i64 -> bool
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    // Logical: bool x bool -> bool
    And,
    Or,
}

impl BinaryOp {
    pub fn operand_type(&self) -> GeneType {
        match self {
            Self::Add
            | Self::Sub
            | Self::Mul
            | Self::DivChecked
            | Self::ModChecked
            | Self::Min
            | Self::Max
            | Self::Eq
            | Self::Ne
            | Self::Lt
            | Self::Le
            | Self::Gt
            | Self::Ge => GeneType::I64,
            Self::And | Self::Or => GeneType::Bool,
        }
    }

    pub fn return_type(&self) -> GeneType {
        match self {
            Self::Add
            | Self::Sub
            | Self::Mul
            | Self::DivChecked
            | Self::ModChecked
            | Self::Min
            | Self::Max => GeneType::I64,
            Self::Eq
            | Self::Ne
            | Self::Lt
            | Self::Le
            | Self::Gt
            | Self::Ge
            | Self::And
            | Self::Or => GeneType::Bool,
        }
    }
}

/// Vector unary operations for 4-element integer vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VecUnaryOp {
    // Vec4 -> Vec4
    Neg,
    Abs,
    // Vec4 -> i64 reductions
    Sum,
    Mean,
    MinElement,
    MaxElement,
    ArgMax,
}

impl VecUnaryOp {
    pub fn operand_type(&self) -> GeneType {
        GeneType::Vec4
    }

    pub fn return_type(&self) -> GeneType {
        match self {
            Self::Neg | Self::Abs => GeneType::Vec4,
            Self::Sum | Self::Mean | Self::MinElement | Self::MaxElement | Self::ArgMax => {
                GeneType::I64
            }
        }
    }
}

/// Vector binary operations for 4-element integer vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VecBinaryOp {
    // Element-wise: Vec4 x Vec4 -> Vec4
    Add,
    Sub,
    Mul,
    Min,
    Max,
    // Vector-scalar: Vec4 x i64 -> Vec4
    Scale,
    Shift,
    // Inner product: Vec4 x Vec4 -> i64
    Dot,
}

impl VecBinaryOp {
    pub fn lhs_type(&self) -> GeneType {
        GeneType::Vec4
    }

    pub fn rhs_type(&self) -> GeneType {
        match self {
            Self::Add | Self::Sub | Self::Mul | Self::Min | Self::Max | Self::Dot => GeneType::Vec4,
            Self::Scale | Self::Shift => GeneType::I64,
        }
    }

    pub fn return_type(&self) -> GeneType {
        match self {
            Self::Add
            | Self::Sub
            | Self::Mul
            | Self::Min
            | Self::Max
            | Self::Scale
            | Self::Shift => GeneType::Vec4,
            Self::Dot => GeneType::I64,
        }
    }
}

/// Persistent state container holding scalar registers and vector memory banks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GenomeState {
    pub scalars: [i64; 8],
    pub vectors: [[i64; 4]; 4],
}

impl GenomeState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.scalars = [0i64; 8];
        self.vectors = [[0i64; 4]; 4];
    }
}

/// Runtime value representation for multi-typed evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeneValue {
    I64(i64),
    Bool(bool),
    Vec4([i64; 4]),
}

impl GeneValue {
    pub fn as_i64(&self) -> Result<i64, String> {
        match self {
            Self::I64(v) => Ok(*v),
            _ => Err(format!("expected I64, found {:?}", self)),
        }
    }

    pub fn as_bool(&self) -> Result<bool, String> {
        match self {
            Self::Bool(v) => Ok(*v),
            _ => Err(format!("expected Bool, found {:?}", self)),
        }
    }

    pub fn as_vec4(&self) -> Result<[i64; 4], String> {
        match self {
            Self::Vec4(v) => Ok(*v),
            _ => Err(format!("expected Vec4, found {:?}", self)),
        }
    }

    pub fn gene_type(&self) -> GeneType {
        match self {
            Self::I64(_) => GeneType::I64,
            Self::Bool(_) => GeneType::Bool,
            Self::Vec4(_) => GeneType::Vec4,
        }
    }
}

/// Strongly typed AST node in an evolvable genome program.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GeneNode {
    ConstI64(i64),
    ConstBool(bool),
    ConstVec([i64; 4]),
    Var(String, GeneType),
    StateRead(u8),
    StateWrite {
        reg: u8,
        val: Box<GeneNode>,
    },
    StateVecRead(u8),
    StateVecWrite {
        reg: u8,
        val: Box<GeneNode>,
    },
    StateAccum {
        reg: u8,
        val: Box<GeneNode>,
        factor: i64,
    },
    Unary {
        op: UnaryOp,
        child: Box<GeneNode>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<GeneNode>,
        rhs: Box<GeneNode>,
    },
    VecUnary {
        op: VecUnaryOp,
        child: Box<GeneNode>,
    },
    VecBinary {
        op: VecBinaryOp,
        lhs: Box<GeneNode>,
        rhs: Box<GeneNode>,
    },
    VecPack([Box<GeneNode>; 4]),
    VecExtract {
        vec: Box<GeneNode>,
        index: u8,
    },
    IfThenElse {
        cond: Box<GeneNode>,
        then_branch: Box<GeneNode>,
        else_branch: Box<GeneNode>,
    },
}

impl GeneNode {
    pub fn gene_type(&self) -> GeneType {
        match self {
            Self::ConstI64(_) => GeneType::I64,
            Self::ConstBool(_) => GeneType::Bool,
            Self::ConstVec(_) => GeneType::Vec4,
            Self::Var(_, ty) => *ty,
            Self::StateRead(_) => GeneType::I64,
            Self::StateWrite { .. } => GeneType::I64,
            Self::StateVecRead(_) => GeneType::Vec4,
            Self::StateVecWrite { .. } => GeneType::Vec4,
            Self::StateAccum { .. } => GeneType::I64,
            Self::Unary { op, .. } => op.return_type(),
            Self::Binary { op, .. } => op.return_type(),
            Self::VecUnary { op, .. } => op.return_type(),
            Self::VecBinary { op, .. } => op.return_type(),
            Self::VecPack(_) => GeneType::Vec4,
            Self::VecExtract { .. } => GeneType::I64,
            Self::IfThenElse { then_branch, .. } => then_branch.gene_type(),
        }
    }

    /// Number of AST nodes in this subtree.
    pub fn size(&self) -> usize {
        match self {
            Self::ConstI64(_)
            | Self::ConstBool(_)
            | Self::ConstVec(_)
            | Self::Var(_, _)
            | Self::StateRead(_)
            | Self::StateVecRead(_) => 1,
            Self::StateWrite { val, .. }
            | Self::StateVecWrite { val, .. }
            | Self::StateAccum { val, .. } => 1 + val.size(),
            Self::Unary { child, .. } | Self::VecUnary { child, .. } => 1 + child.size(),
            Self::VecExtract { vec, .. } => 1 + vec.size(),
            Self::Binary { lhs, rhs, .. } | Self::VecBinary { lhs, rhs, .. } => {
                1 + lhs.size() + rhs.size()
            }
            Self::VecPack(elems) => {
                1 + elems[0].size() + elems[1].size() + elems[2].size() + elems[3].size()
            }
            Self::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => 1 + cond.size() + then_branch.size() + else_branch.size(),
        }
    }

    /// Maximum depth of this AST tree (leaf has depth 1).
    pub fn depth(&self) -> usize {
        match self {
            Self::ConstI64(_)
            | Self::ConstBool(_)
            | Self::ConstVec(_)
            | Self::Var(_, _)
            | Self::StateRead(_)
            | Self::StateVecRead(_) => 1,
            Self::StateWrite { val, .. }
            | Self::StateVecWrite { val, .. }
            | Self::StateAccum { val, .. } => 1 + val.depth(),
            Self::Unary { child, .. } | Self::VecUnary { child, .. } => 1 + child.depth(),
            Self::VecExtract { vec, .. } => 1 + vec.depth(),
            Self::Binary { lhs, rhs, .. } | Self::VecBinary { lhs, rhs, .. } => {
                1 + std::cmp::max(lhs.depth(), rhs.depth())
            }
            Self::VecPack(elems) => {
                1 + std::cmp::max(
                    std::cmp::max(elems[0].depth(), elems[1].depth()),
                    std::cmp::max(elems[2].depth(), elems[3].depth()),
                )
            }
            Self::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => {
                1 + std::cmp::max(
                    cond.depth(),
                    std::cmp::max(then_branch.depth(), else_branch.depth()),
                )
            }
        }
    }

    /// Evaluates the gene tree in a multi-typed environment with full persistent state.
    pub fn evaluate_value(
        &self,
        env: &HashMap<String, GeneValue>,
        state: &mut GenomeState,
    ) -> Result<GeneValue, String> {
        match self {
            Self::ConstI64(val) => Ok(GeneValue::I64(*val)),
            Self::ConstBool(val) => Ok(GeneValue::Bool(*val)),
            Self::ConstVec(val) => Ok(GeneValue::Vec4(*val)),
            Self::Var(name, _) => env
                .get(name)
                .cloned()
                .ok_or_else(|| format!("unbound variable '{name}'")),
            Self::StateRead(reg) => Ok(GeneValue::I64(state.scalars[(*reg as usize) % 8])),
            Self::StateWrite { reg, val } => {
                let v = val.evaluate_value(env, state)?.as_i64()?;
                state.scalars[(*reg as usize) % 8] = v;
                Ok(GeneValue::I64(v))
            }
            Self::StateVecRead(reg) => Ok(GeneValue::Vec4(state.vectors[(*reg as usize) % 4])),
            Self::StateVecWrite { reg, val } => {
                let v = val.evaluate_value(env, state)?.as_vec4()?;
                state.vectors[(*reg as usize) % 4] = v;
                Ok(GeneValue::Vec4(v))
            }
            Self::StateAccum { reg, val, factor } => {
                let v = val.evaluate_value(env, state)?.as_i64()?;
                let idx = (*reg as usize) % 8;
                let old = state.scalars[idx];
                let f = if *factor == 0 { 1 } else { *factor };
                let new_val = old + ((v - old) / f);
                state.scalars[idx] = new_val;
                Ok(GeneValue::I64(new_val))
            }
            Self::Unary { op, child } => {
                let v = child.evaluate_value(env, state)?;
                match op {
                    UnaryOp::Neg => Ok(GeneValue::I64(v.as_i64()?.wrapping_neg())),
                    UnaryOp::Abs => Ok(GeneValue::I64(v.as_i64()?.abs())),
                    UnaryOp::Not => Ok(GeneValue::Bool(!v.as_bool()?)),
                }
            }
            Self::Binary { op, lhs, rhs } => {
                let l = lhs.evaluate_value(env, state)?;
                let r = rhs.evaluate_value(env, state)?;
                match op {
                    BinaryOp::Add => Ok(GeneValue::I64(l.as_i64()?.wrapping_add(r.as_i64()?))),
                    BinaryOp::Sub => Ok(GeneValue::I64(l.as_i64()?.wrapping_sub(r.as_i64()?))),
                    BinaryOp::Mul => Ok(GeneValue::I64(l.as_i64()?.wrapping_mul(r.as_i64()?))),
                    BinaryOp::DivChecked => {
                        let rv = r.as_i64()?;
                        if rv == 0 {
                            Ok(GeneValue::I64(1))
                        } else {
                            Ok(GeneValue::I64(l.as_i64()?.wrapping_div(rv)))
                        }
                    }
                    BinaryOp::ModChecked => {
                        let rv = r.as_i64()?;
                        if rv == 0 {
                            Ok(GeneValue::I64(0))
                        } else {
                            Ok(GeneValue::I64(l.as_i64()?.wrapping_rem(rv)))
                        }
                    }
                    BinaryOp::Min => Ok(GeneValue::I64(std::cmp::min(l.as_i64()?, r.as_i64()?))),
                    BinaryOp::Max => Ok(GeneValue::I64(std::cmp::max(l.as_i64()?, r.as_i64()?))),
                    BinaryOp::Eq => Ok(GeneValue::Bool(l == r)),
                    BinaryOp::Ne => Ok(GeneValue::Bool(l != r)),
                    BinaryOp::Lt => Ok(GeneValue::Bool(l.as_i64()? < r.as_i64()?)),
                    BinaryOp::Le => Ok(GeneValue::Bool(l.as_i64()? <= r.as_i64()?)),
                    BinaryOp::Gt => Ok(GeneValue::Bool(l.as_i64()? > r.as_i64()?)),
                    BinaryOp::Ge => Ok(GeneValue::Bool(l.as_i64()? >= r.as_i64()?)),
                    BinaryOp::And => Ok(GeneValue::Bool(l.as_bool()? && r.as_bool()?)),
                    BinaryOp::Or => Ok(GeneValue::Bool(l.as_bool()? || r.as_bool()?)),
                }
            }
            Self::VecUnary { op, child } => {
                let v = child.evaluate_value(env, state)?.as_vec4()?;
                match op {
                    VecUnaryOp::Neg => Ok(GeneValue::Vec4([
                        v[0].wrapping_neg(),
                        v[1].wrapping_neg(),
                        v[2].wrapping_neg(),
                        v[3].wrapping_neg(),
                    ])),
                    VecUnaryOp::Abs => Ok(GeneValue::Vec4([
                        v[0].abs(),
                        v[1].abs(),
                        v[2].abs(),
                        v[3].abs(),
                    ])),
                    VecUnaryOp::Sum => Ok(GeneValue::I64(
                        v[0].wrapping_add(v[1])
                            .wrapping_add(v[2])
                            .wrapping_add(v[3]),
                    )),
                    VecUnaryOp::Mean => Ok(GeneValue::I64(
                        v[0].wrapping_add(v[1])
                            .wrapping_add(v[2])
                            .wrapping_add(v[3])
                            / 4,
                    )),
                    VecUnaryOp::MinElement => Ok(GeneValue::I64(std::cmp::min(
                        std::cmp::min(v[0], v[1]),
                        std::cmp::min(v[2], v[3]),
                    ))),
                    VecUnaryOp::MaxElement => Ok(GeneValue::I64(std::cmp::max(
                        std::cmp::max(v[0], v[1]),
                        std::cmp::max(v[2], v[3]),
                    ))),
                    VecUnaryOp::ArgMax => {
                        let mut best_i = 0;
                        let mut best_v = v[0];
                        for (i, &item) in v.iter().enumerate().skip(1) {
                            if item > best_v {
                                best_v = item;
                                best_i = i;
                            }
                        }
                        Ok(GeneValue::I64(best_i as i64))
                    }
                }
            }
            Self::VecBinary { op, lhs, rhs } => {
                let l = lhs.evaluate_value(env, state)?;
                let r = rhs.evaluate_value(env, state)?;
                match op {
                    VecBinaryOp::Add => {
                        let lv = l.as_vec4()?;
                        let rv = r.as_vec4()?;
                        Ok(GeneValue::Vec4([
                            lv[0].wrapping_add(rv[0]),
                            lv[1].wrapping_add(rv[1]),
                            lv[2].wrapping_add(rv[2]),
                            lv[3].wrapping_add(rv[3]),
                        ]))
                    }
                    VecBinaryOp::Sub => {
                        let lv = l.as_vec4()?;
                        let rv = r.as_vec4()?;
                        Ok(GeneValue::Vec4([
                            lv[0].wrapping_sub(rv[0]),
                            lv[1].wrapping_sub(rv[1]),
                            lv[2].wrapping_sub(rv[2]),
                            lv[3].wrapping_sub(rv[3]),
                        ]))
                    }
                    VecBinaryOp::Mul => {
                        let lv = l.as_vec4()?;
                        let rv = r.as_vec4()?;
                        Ok(GeneValue::Vec4([
                            lv[0].wrapping_mul(rv[0]),
                            lv[1].wrapping_mul(rv[1]),
                            lv[2].wrapping_mul(rv[2]),
                            lv[3].wrapping_mul(rv[3]),
                        ]))
                    }
                    VecBinaryOp::Min => {
                        let lv = l.as_vec4()?;
                        let rv = r.as_vec4()?;
                        Ok(GeneValue::Vec4([
                            std::cmp::min(lv[0], rv[0]),
                            std::cmp::min(lv[1], rv[1]),
                            std::cmp::min(lv[2], rv[2]),
                            std::cmp::min(lv[3], rv[3]),
                        ]))
                    }
                    VecBinaryOp::Max => {
                        let lv = l.as_vec4()?;
                        let rv = r.as_vec4()?;
                        Ok(GeneValue::Vec4([
                            std::cmp::max(lv[0], rv[0]),
                            std::cmp::max(lv[1], rv[1]),
                            std::cmp::max(lv[2], rv[2]),
                            std::cmp::max(lv[3], rv[3]),
                        ]))
                    }
                    VecBinaryOp::Scale => {
                        let lv = l.as_vec4()?;
                        let s = r.as_i64()?;
                        Ok(GeneValue::Vec4([
                            lv[0].wrapping_mul(s),
                            lv[1].wrapping_mul(s),
                            lv[2].wrapping_mul(s),
                            lv[3].wrapping_mul(s),
                        ]))
                    }
                    VecBinaryOp::Shift => {
                        let lv = l.as_vec4()?;
                        let s = r.as_i64()?;
                        Ok(GeneValue::Vec4([
                            lv[0].wrapping_add(s),
                            lv[1].wrapping_add(s),
                            lv[2].wrapping_add(s),
                            lv[3].wrapping_add(s),
                        ]))
                    }
                    VecBinaryOp::Dot => {
                        let lv = l.as_vec4()?;
                        let rv = r.as_vec4()?;
                        let dot = lv[0]
                            .wrapping_mul(rv[0])
                            .wrapping_add(lv[1].wrapping_mul(rv[1]))
                            .wrapping_add(lv[2].wrapping_mul(rv[2]))
                            .wrapping_add(lv[3].wrapping_mul(rv[3]));
                        Ok(GeneValue::I64(dot))
                    }
                }
            }
            Self::VecPack(elems) => {
                let v0 = elems[0].evaluate_value(env, state)?.as_i64()?;
                let v1 = elems[1].evaluate_value(env, state)?.as_i64()?;
                let v2 = elems[2].evaluate_value(env, state)?.as_i64()?;
                let v3 = elems[3].evaluate_value(env, state)?.as_i64()?;
                Ok(GeneValue::Vec4([v0, v1, v2, v3]))
            }
            Self::VecExtract { vec, index } => {
                let v = vec.evaluate_value(env, state)?.as_vec4()?;
                Ok(GeneValue::I64(v[(*index as usize) % 4]))
            }
            Self::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => {
                if cond.evaluate_value(env, state)?.as_bool()? {
                    then_branch.evaluate_value(env, state)
                } else {
                    else_branch.evaluate_value(env, state)
                }
            }
        }
    }

    /// Evaluates the gene tree with full persistent state.
    pub fn evaluate_with_full_state(
        &self,
        env: &HashMap<String, GeneValue>,
        state: &mut GenomeState,
    ) -> Result<GeneValue, String> {
        self.evaluate_value(env, state)
    }

    /// Evaluates the gene tree as an integer expression with 4 legacy registers.
    pub fn evaluate_with_state(
        &self,
        env: &HashMap<String, i64>,
        state: &mut [i64; 4],
    ) -> Result<i64, String> {
        let mut full_state = GenomeState::default();
        full_state.scalars[..4].copy_from_slice(state);
        let mut full_env = HashMap::new();
        for (k, v) in env {
            full_env.insert(k.clone(), GeneValue::I64(*v));
        }
        let res = self.evaluate_value(&full_env, &mut full_state)?.as_i64()?;
        state.copy_from_slice(&full_state.scalars[..4]);
        Ok(res)
    }

    /// Evaluates the gene tree as an integer expression with default registers.
    pub fn evaluate(&self, env: &HashMap<String, i64>) -> Result<i64, String> {
        let mut default_state = [0i64; 4];
        self.evaluate_with_state(env, &mut default_state)
    }

    /// Evaluates the gene tree as a boolean expression with 4 legacy registers.
    pub fn evaluate_bool_with_state(
        &self,
        env: &HashMap<String, i64>,
        state: &mut [i64; 4],
    ) -> Result<bool, String> {
        let mut full_state = GenomeState::default();
        full_state.scalars[..4].copy_from_slice(state);
        let mut full_env = HashMap::new();
        for (k, v) in env {
            full_env.insert(k.clone(), GeneValue::I64(*v));
        }
        let res = self.evaluate_value(&full_env, &mut full_state)?.as_bool()?;
        state.copy_from_slice(&full_state.scalars[..4]);
        Ok(res)
    }

    /// Evaluates the gene tree as a boolean expression with default registers.
    pub fn evaluate_bool(&self, env: &HashMap<String, i64>) -> Result<bool, String> {
        let mut default_state = [0i64; 4];
        self.evaluate_bool_with_state(env, &mut default_state)
    }

    /// Evaluates the gene tree given a 4-element sensory input vector `[x0, x1, x2, x3]`
    /// and returns both the output vector and scalar action choice (argmax).
    pub fn evaluate_vec4(
        &self,
        env_vec: &[i64; 4],
        state: &mut GenomeState,
    ) -> Result<([i64; 4], i64), String> {
        let mut env = HashMap::new();
        env.insert("input_vec".to_string(), GeneValue::Vec4(*env_vec));
        env.insert("input".to_string(), GeneValue::I64(env_vec[0]));
        env.insert("x".to_string(), GeneValue::I64(env_vec[0]));

        let val = self.evaluate_value(&env, state)?;
        match val {
            GeneValue::Vec4(v) => {
                let mut best_i = 0;
                let mut best_v = v[0];
                for (i, &item) in v.iter().enumerate().skip(1) {
                    if item > best_v {
                        best_v = item;
                        best_i = i;
                    }
                }
                Ok((v, best_i as i64))
            }
            GeneValue::I64(s) => {
                let action = s.rem_euclid(4);
                let mut v = [0i64; 4];
                v[action as usize] = 100;
                Ok((v, action))
            }
            GeneValue::Bool(b) => {
                let s = if b { 1 } else { 0 };
                Ok(([s, 0, 0, 0], s))
            }
        }
    }

    /// Collect all 0-based node indices that match a specific `target_type`.
    pub fn collect_indices_by_type(&self, target_type: GeneType) -> Vec<usize> {
        let mut indices = Vec::new();
        let mut current_idx = 0;
        self.collect_indices_internal(target_type, &mut current_idx, &mut indices);
        indices
    }

    fn collect_indices_internal(
        &self,
        target_type: GeneType,
        current_idx: &mut usize,
        out: &mut Vec<usize>,
    ) {
        let my_idx = *current_idx;
        *current_idx += 1;
        if self.gene_type() == target_type {
            out.push(my_idx);
        }
        match self {
            Self::ConstI64(_)
            | Self::ConstBool(_)
            | Self::ConstVec(_)
            | Self::Var(_, _)
            | Self::StateRead(_)
            | Self::StateVecRead(_) => {}
            Self::StateWrite { val, .. }
            | Self::StateVecWrite { val, .. }
            | Self::StateAccum { val, .. }
            | Self::Unary { child: val, .. }
            | Self::VecUnary { child: val, .. }
            | Self::VecExtract { vec: val, .. } => {
                val.collect_indices_internal(target_type, current_idx, out);
            }
            Self::Binary { lhs, rhs, .. } | Self::VecBinary { lhs, rhs, .. } => {
                lhs.collect_indices_internal(target_type, current_idx, out);
                rhs.collect_indices_internal(target_type, current_idx, out);
            }
            Self::VecPack(elems) => {
                for e in elems {
                    e.collect_indices_internal(target_type, current_idx, out);
                }
            }
            Self::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => {
                cond.collect_indices_internal(target_type, current_idx, out);
                then_branch.collect_indices_internal(target_type, current_idx, out);
                else_branch.collect_indices_internal(target_type, current_idx, out);
            }
        }
    }

    /// Collects all node indices where the node is a ConstI64 or ConstVec.
    pub fn collect_indices_by_variant_const_i64(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        let mut current_idx = 0;
        self.collect_const_i64_indices_internal(&mut current_idx, &mut indices);
        indices
    }

    fn collect_const_i64_indices_internal(&self, current_idx: &mut usize, out: &mut Vec<usize>) {
        let my_idx = *current_idx;
        *current_idx += 1;
        if let Self::ConstI64(_) | Self::ConstVec(_) = self {
            out.push(my_idx);
        }
        match self {
            Self::ConstI64(_)
            | Self::ConstBool(_)
            | Self::ConstVec(_)
            | Self::Var(_, _)
            | Self::StateRead(_)
            | Self::StateVecRead(_) => {}
            Self::StateWrite { val, .. }
            | Self::StateVecWrite { val, .. }
            | Self::StateAccum { val, .. }
            | Self::Unary { child: val, .. }
            | Self::VecUnary { child: val, .. }
            | Self::VecExtract { vec: val, .. } => {
                val.collect_const_i64_indices_internal(current_idx, out);
            }
            Self::Binary { lhs, rhs, .. } | Self::VecBinary { lhs, rhs, .. } => {
                lhs.collect_const_i64_indices_internal(current_idx, out);
                rhs.collect_const_i64_indices_internal(current_idx, out);
            }
            Self::VecPack(elems) => {
                for e in elems {
                    e.collect_const_i64_indices_internal(current_idx, out);
                }
            }
            Self::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => {
                cond.collect_const_i64_indices_internal(current_idx, out);
                then_branch.collect_const_i64_indices_internal(current_idx, out);
                else_branch.collect_const_i64_indices_internal(current_idx, out);
            }
        }
    }

    /// Retrieves a cloned copy of the node at `target_idx`.
    pub fn get_node_by_index(&self, target_idx: usize) -> Option<GeneNode> {
        let mut current_idx = 0;
        self.get_node_internal(target_idx, &mut current_idx)
    }

    fn get_node_internal(&self, target_idx: usize, current_idx: &mut usize) -> Option<GeneNode> {
        if *current_idx == target_idx {
            return Some(self.clone());
        }
        *current_idx += 1;
        match self {
            Self::ConstI64(_)
            | Self::ConstBool(_)
            | Self::ConstVec(_)
            | Self::Var(_, _)
            | Self::StateRead(_)
            | Self::StateVecRead(_) => None,
            Self::StateWrite { val, .. }
            | Self::StateVecWrite { val, .. }
            | Self::StateAccum { val, .. }
            | Self::Unary { child: val, .. }
            | Self::VecUnary { child: val, .. }
            | Self::VecExtract { vec: val, .. } => val.get_node_internal(target_idx, current_idx),
            Self::Binary { lhs, rhs, .. } | Self::VecBinary { lhs, rhs, .. } => lhs
                .get_node_internal(target_idx, current_idx)
                .or_else(|| rhs.get_node_internal(target_idx, current_idx)),
            Self::VecPack(elems) => elems
                .iter()
                .find_map(|e| e.get_node_internal(target_idx, current_idx)),
            Self::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => cond
                .get_node_internal(target_idx, current_idx)
                .or_else(|| then_branch.get_node_internal(target_idx, current_idx))
                .or_else(|| else_branch.get_node_internal(target_idx, current_idx)),
        }
    }

    /// Replaces the node at `target_idx` with `replacement` if types match.
    pub fn replace_node_by_index(&mut self, target_idx: usize, replacement: GeneNode) -> bool {
        let mut current_idx = 0;
        self.replace_node_internal(target_idx, &mut current_idx, replacement)
    }

    fn replace_node_internal(
        &mut self,
        target_idx: usize,
        current_idx: &mut usize,
        replacement: GeneNode,
    ) -> bool {
        if *current_idx == target_idx {
            if self.gene_type() == replacement.gene_type() {
                *self = replacement;
                return true;
            }
            return false;
        }
        *current_idx += 1;
        match self {
            Self::ConstI64(_)
            | Self::ConstBool(_)
            | Self::ConstVec(_)
            | Self::Var(_, _)
            | Self::StateRead(_)
            | Self::StateVecRead(_) => false,
            Self::StateWrite { val, .. }
            | Self::StateVecWrite { val, .. }
            | Self::StateAccum { val, .. }
            | Self::Unary { child: val, .. }
            | Self::VecUnary { child: val, .. }
            | Self::VecExtract { vec: val, .. } => {
                val.replace_node_internal(target_idx, current_idx, replacement)
            }
            Self::Binary { lhs, rhs, .. } | Self::VecBinary { lhs, rhs, .. } => {
                if lhs.replace_node_internal(target_idx, current_idx, replacement.clone()) {
                    return true;
                }
                rhs.replace_node_internal(target_idx, current_idx, replacement)
            }
            Self::VecPack(elems) => {
                for e in elems.iter_mut() {
                    if e.replace_node_internal(target_idx, current_idx, replacement.clone()) {
                        return true;
                    }
                }
                false
            }
            Self::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => {
                if cond.replace_node_internal(target_idx, current_idx, replacement.clone()) {
                    return true;
                }
                if then_branch.replace_node_internal(target_idx, current_idx, replacement.clone()) {
                    return true;
                }
                else_branch.replace_node_internal(target_idx, current_idx, replacement)
            }
        }
    }

    /// Emits clean, valid IRIS expression code.
    pub fn to_iris_expr(&self) -> String {
        match self {
            Self::ConstI64(val) => {
                if *val < 0 {
                    format!("({val})")
                } else {
                    val.to_string()
                }
            }
            Self::ConstBool(val) => val.to_string(),
            Self::ConstVec(v) => {
                format!("vec4_new({}, {}, {}, {})", v[0], v[1], v[2], v[3])
            }
            Self::Var(name, _) => name.clone(),
            Self::StateRead(reg) => format!("state_r{}", reg % 8),
            Self::StateWrite { reg, val } => {
                let inner = val.to_iris_expr();
                format!("(state_set({}, {}))", reg % 8, inner)
            }
            Self::StateVecRead(reg) => format!("state_v{}", reg % 4),
            Self::StateVecWrite { reg, val } => {
                let inner = val.to_iris_expr();
                format!("(state_vset({}, {}))", reg % 4, inner)
            }
            Self::StateAccum { reg, val, factor } => {
                let inner = val.to_iris_expr();
                format!("(state_accum({}, {}, {}))", reg % 8, inner, factor)
            }
            Self::Unary { op, child } => {
                let inner = child.to_iris_expr();
                match op {
                    UnaryOp::Neg => format!("(-{inner})"),
                    UnaryOp::Abs => [
                        "(if ",
                        &inner,
                        " < 0 { -",
                        &inner,
                        " } else { ",
                        &inner,
                        " })",
                    ]
                    .concat(),
                    UnaryOp::Not => format!("(!{inner})"),
                }
            }
            Self::Binary { op, lhs, rhs } => {
                let l = lhs.to_iris_expr();
                let r = rhs.to_iris_expr();
                match op {
                    BinaryOp::Add => format!("({l} + {r})"),
                    BinaryOp::Sub => format!("({l} - {r})"),
                    BinaryOp::Mul => format!("({l} * {r})"),
                    BinaryOp::DivChecked => {
                        ["(if ", &r, " == 0 { 1 } else { ", &l, " / ", &r, " })"].concat()
                    }
                    BinaryOp::ModChecked => {
                        ["(if ", &r, " == 0 { 0 } else { ", &l, " % ", &r, " })"].concat()
                    }
                    BinaryOp::Min => {
                        ["(if ", &l, " < ", &r, " { ", &l, " } else { ", &r, " })"].concat()
                    }
                    BinaryOp::Max => {
                        ["(if ", &l, " > ", &r, " { ", &l, " } else { ", &r, " })"].concat()
                    }
                    BinaryOp::Eq => format!("({l} == {r})"),
                    BinaryOp::Ne => format!("({l} != {r})"),
                    BinaryOp::Lt => format!("({l} < {r})"),
                    BinaryOp::Le => format!("({l} <= {r})"),
                    BinaryOp::Gt => format!("({l} > {r})"),
                    BinaryOp::Ge => format!("({l} >= {r})"),
                    BinaryOp::And => format!("({l} && {r})"),
                    BinaryOp::Or => format!("({l} || {r})"),
                }
            }
            Self::VecUnary { op, child } => {
                let c = child.to_iris_expr();
                match op {
                    VecUnaryOp::Neg => format!("vec4_scale({c}, -1)"),
                    VecUnaryOp::Abs => format!("vec4_abs({c})"),
                    VecUnaryOp::Sum => format!("vec4_sum({c})"),
                    VecUnaryOp::Mean => format!("vec4_mean({c})"),
                    VecUnaryOp::MinElement => format!("vec4_min_elem({c})"),
                    VecUnaryOp::MaxElement => format!("vec4_max_elem({c})"),
                    VecUnaryOp::ArgMax => format!("vec4_argmax({c})"),
                }
            }
            Self::VecBinary { op, lhs, rhs } => {
                let l = lhs.to_iris_expr();
                let r = rhs.to_iris_expr();
                match op {
                    VecBinaryOp::Add => format!("vec4_add({l}, {r})"),
                    VecBinaryOp::Sub => format!("vec4_sub({l}, {r})"),
                    VecBinaryOp::Mul => format!("vec4_mul({l}, {r})"),
                    VecBinaryOp::Min => format!("vec4_min({l}, {r})"),
                    VecBinaryOp::Max => format!("vec4_max({l}, {r})"),
                    VecBinaryOp::Scale => format!("vec4_scale({l}, {r})"),
                    VecBinaryOp::Shift => format!("vec4_shift({l}, {r})"),
                    VecBinaryOp::Dot => format!("vec4_dot({l}, {r})"),
                }
            }
            Self::VecPack(elems) => {
                let e0 = elems[0].to_iris_expr();
                let e1 = elems[1].to_iris_expr();
                let e2 = elems[2].to_iris_expr();
                let e3 = elems[3].to_iris_expr();
                format!("vec4_new({e0}, {e1}, {e2}, {e3})")
            }
            Self::VecExtract { vec, index } => {
                let v = vec.to_iris_expr();
                format!("list_get({v}, {})", index % 4)
            }
            Self::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => {
                let c = cond.to_iris_expr();
                let t = then_branch.to_iris_expr();
                let e = else_branch.to_iris_expr();
                ["(if ", &c, " { ", &t, " } else { ", &e, " })"].concat()
            }
        }
    }

    /// Checks if this AST subtree utilizes vectorized operations or vector state.
    pub fn uses_vectors(&self) -> bool {
        match self {
            Self::ConstVec(_)
            | Self::StateVecRead(_)
            | Self::StateVecWrite { .. }
            | Self::VecUnary { .. }
            | Self::VecBinary { .. }
            | Self::VecPack(_)
            | Self::VecExtract { .. } => true,
            Self::Var(_, ty) => *ty == GeneType::Vec4,
            Self::ConstI64(_) | Self::ConstBool(_) | Self::StateRead(_) => false,
            Self::StateWrite { val, .. } | Self::StateAccum { val, .. } => val.uses_vectors(),
            Self::Unary { child, .. } => child.uses_vectors(),
            Self::Binary { lhs, rhs, .. } => lhs.uses_vectors() || rhs.uses_vectors(),
            Self::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => cond.uses_vectors() || then_branch.uses_vectors() || else_branch.uses_vectors(),
        }
    }

    /// Checks if this AST subtree utilizes persistent scalar or vector state.
    pub fn uses_state(&self) -> bool {
        match self {
            Self::StateRead(_)
            | Self::StateWrite { .. }
            | Self::StateAccum { .. }
            | Self::StateVecRead(_)
            | Self::StateVecWrite { .. } => true,
            Self::Unary { child, .. } => child.uses_state(),
            Self::Binary { lhs, rhs, .. } => lhs.uses_state() || rhs.uses_state(),
            Self::VecUnary { child, .. } => child.uses_state(),
            Self::VecBinary { lhs, rhs, .. } => lhs.uses_state() || rhs.uses_state(),
            Self::VecPack(lanes) => lanes.iter().any(|lane| lane.uses_state()),
            Self::VecExtract { vec, .. } => vec.uses_state(),
            Self::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => cond.uses_state() || then_branch.uses_state() || else_branch.uses_state(),
            Self::ConstI64(_) | Self::ConstBool(_) | Self::ConstVec(_) | Self::Var(_, _) => false,
        }
    }

    pub fn to_iris_source(&self, func_name: &str, param_name: &str) -> String {
        let expr = self.to_iris_expr();
        let uses_vec = self.uses_vectors();
        let mut source = String::new();
        source.push_str("/// Evolved candidate generated by IRIS Genetic Programming Engine.\n\n");
        source
            .push_str("def state_set(idx: i64, new_val: i64) -> i64 {\n    return new_val\n}\n\n");
        source.push_str("def state_accum(idx: i64, new_val: i64, factor: i64) -> i64 {\n    return new_val\n}\n\n");

        if uses_vec {
            source.push_str("def vec4_new(v0: i64, v1: i64, v2: i64, v3: i64) -> list<i64> effect alloc {\n    val l: list<i64> = list(v0, v1, v2, v3);\n    return l\n}\n\n");
            source.push_str("def state_vset(idx: i64, v: list<i64>) -> list<i64> effect alloc {\n    return v\n}\n\n");
            source.push_str("def vec4_add(a: list<i64>, b: list<i64>) -> list<i64> effect alloc {\n    val l: list<i64> = list(list_get(a, 0) + list_get(b, 0), list_get(a, 1) + list_get(b, 1), list_get(a, 2) + list_get(b, 2), list_get(a, 3) + list_get(b, 3));\n    return l\n}\n\n");
            source.push_str("def vec4_sub(a: list<i64>, b: list<i64>) -> list<i64> effect alloc {\n    val l: list<i64> = list(list_get(a, 0) - list_get(b, 0), list_get(a, 1) - list_get(b, 1), list_get(a, 2) - list_get(b, 2), list_get(a, 3) - list_get(b, 3));\n    return l\n}\n\n");
            source.push_str("def vec4_mul(a: list<i64>, b: list<i64>) -> list<i64> effect alloc {\n    val l: list<i64> = list(list_get(a, 0) * list_get(b, 0), list_get(a, 1) * list_get(b, 1), list_get(a, 2) * list_get(b, 2), list_get(a, 3) * list_get(b, 3));\n    return l\n}\n\n");
            source.push_str("def vec4_min(a: list<i64>, b: list<i64>) -> list<i64> effect alloc {\n    val l: list<i64> = list(if list_get(a, 0) < list_get(b, 0) { list_get(a, 0) } else { list_get(b, 0) }, if list_get(a, 1) < list_get(b, 1) { list_get(a, 1) } else { list_get(b, 1) }, if list_get(a, 2) < list_get(b, 2) { list_get(a, 2) } else { list_get(b, 2) }, if list_get(a, 3) < list_get(b, 3) { list_get(a, 3) } else { list_get(b, 3) });\n    return l\n}\n\n");
            source.push_str("def vec4_max(a: list<i64>, b: list<i64>) -> list<i64> effect alloc {\n    val l: list<i64> = list(if list_get(a, 0) > list_get(b, 0) { list_get(a, 0) } else { list_get(b, 0) }, if list_get(a, 1) > list_get(b, 1) { list_get(a, 1) } else { list_get(b, 1) }, if list_get(a, 2) > list_get(b, 2) { list_get(a, 2) } else { list_get(b, 2) }, if list_get(a, 3) > list_get(b, 3) { list_get(a, 3) } else { list_get(b, 3) });\n    return l\n}\n\n");
            source.push_str("def vec4_scale(a: list<i64>, s: i64) -> list<i64> effect alloc {\n    val l: list<i64> = list(list_get(a, 0) * s, list_get(a, 1) * s, list_get(a, 2) * s, list_get(a, 3) * s);\n    return l\n}\n\n");
            source.push_str("def vec4_shift(a: list<i64>, s: i64) -> list<i64> effect alloc {\n    val l: list<i64> = list(list_get(a, 0) + s, list_get(a, 1) + s, list_get(a, 2) + s, list_get(a, 3) + s);\n    return l\n}\n\n");
            source.push_str("def vec4_abs(a: list<i64>) -> list<i64> effect alloc {\n    val l: list<i64> = list(if list_get(a, 0) < 0 { -list_get(a, 0) } else { list_get(a, 0) }, if list_get(a, 1) < 0 { -list_get(a, 1) } else { list_get(a, 1) }, if list_get(a, 2) < 0 { -list_get(a, 2) } else { list_get(a, 2) }, if list_get(a, 3) < 0 { -list_get(a, 3) } else { list_get(a, 3) });\n    return l\n}\n\n");
            source.push_str("def vec4_dot(a: list<i64>, b: list<i64>) -> i64 effect alloc {\n    return (list_get(a, 0) * list_get(b, 0)) + (list_get(a, 1) * list_get(b, 1)) + (list_get(a, 2) * list_get(b, 2)) + (list_get(a, 3) * list_get(b, 3))\n}\n\n");
            source.push_str("def vec4_sum(a: list<i64>) -> i64 effect alloc {\n    return list_get(a, 0) + list_get(a, 1) + list_get(a, 2) + list_get(a, 3)\n}\n\n");
            source.push_str("def vec4_mean(a: list<i64>) -> i64 effect alloc {\n    return (list_get(a, 0) + list_get(a, 1) + list_get(a, 2) + list_get(a, 3)) / 4\n}\n\n");
            source.push_str("def vec4_min_elem(a: list<i64>) -> i64 effect alloc {\n    val m0 = if list_get(a, 0) < list_get(a, 1) { list_get(a, 0) } else { list_get(a, 1) };\n    val m1 = if list_get(a, 2) < list_get(a, 3) { list_get(a, 2) } else { list_get(a, 3) };\n    return if m0 < m1 { m0 } else { m1 }\n}\n\n");
            source.push_str("def vec4_max_elem(a: list<i64>) -> i64 effect alloc {\n    val m0 = if list_get(a, 0) > list_get(a, 1) { list_get(a, 0) } else { list_get(a, 1) };\n    val m1 = if list_get(a, 2) > list_get(a, 3) { list_get(a, 2) } else { list_get(a, 3) };\n    return if m0 > m1 { m0 } else { m1 }\n}\n\n");
            source.push_str("def vec4_argmax(a: list<i64>) -> i64 effect alloc {\n    var best_i = 0;\n    var best_v = list_get(a, 0);\n    if list_get(a, 1) > best_v { best_v = list_get(a, 1); best_i = 1 };\n    if list_get(a, 2) > best_v { best_v = list_get(a, 2); best_i = 2 };\n    if list_get(a, 3) > best_v { best_v = list_get(a, 3); best_i = 3 };\n    return best_i\n}\n\n");
            source.push_str(&format!(
                "def {func_name}({param_name}: i64) -> i64 effect alloc {{\n"
            ));
        } else {
            source.push_str(&format!("def {func_name}({param_name}: i64) -> i64 {{\n"));
        }

        source.push_str("    val state_r0 = 0;\n");
        source.push_str("    val state_r1 = 0;\n");
        source.push_str("    val state_r2 = 0;\n");
        source.push_str("    val state_r3 = 0;\n");
        source.push_str("    val state_r4 = 0;\n");
        source.push_str("    val state_r5 = 0;\n");
        source.push_str("    val state_r6 = 0;\n");
        source.push_str("    val state_r7 = 0;\n");
        if uses_vec {
            source.push_str("    val state_v0 = vec4_new(0, 0, 0, 0);\n");
            source.push_str("    val state_v1 = vec4_new(0, 0, 0, 0);\n");
            source.push_str("    val state_v2 = vec4_new(0, 0, 0, 0);\n");
            source.push_str("    val state_v3 = vec4_new(0, 0, 0, 0);\n");
        }
        source.push_str(&format!("    return {expr}\n"));
        source.push_str("}\n\n");

        if uses_vec {
            source.push_str("def test_policy() -> i64 effect alloc, throw {\n");
        } else {
            source.push_str("def test_policy() -> i64 effect throw {\n");
        }
        source.push_str(&format!("    val s0 = {func_name}(0);\n"));
        source.push_str(&format!("    assert({func_name}(0) == s0);\n"));
        source.push_str(&format!("    val s1 = {func_name}(1);\n"));
        source.push_str(&format!("    assert({func_name}(1) == s1);\n"));
        source.push_str(&format!("    val sm1 = {func_name}(-1);\n"));
        source.push_str(&format!("    assert({func_name}(-1) == sm1);\n"));
        source.push_str("    return 0\n");
        source.push_str("}\n\n");

        source.push_str(
            "def test_transaction_rollback() -> i64 effect alloc, transaction, throw {\n",
        );
        source.push_str("    val state : list<i64> = list(10);\n");
        source.push_str("    transaction_begin();\n");
        source.push_str("    list_push(state, 99);\n");
        source.push_str("    transaction_rollback();\n");
        source.push_str("    assert(transaction_depth() == 0);\n");
        source.push_str("    assert(list_len(state) == 1);\n");
        source.push_str("    assert(list_get(state, 0) == 10);\n");
        source.push_str("    return 0\n");
        source.push_str("}\n");
        source
    }

    /// Emits a syntactically valid, idiomatic Python expression with default parameter name 'x'.
    pub fn to_python_expr(&self) -> String {
        self.to_python_expr_named("x")
    }

    /// Emits a syntactically valid Python expression mapping input variable to `target_var`.
    pub fn to_python_expr_named(&self, target_var: &str) -> String {
        match self {
            Self::ConstI64(val) => {
                if *val < 0 {
                    format!("({val})")
                } else {
                    val.to_string()
                }
            }
            Self::ConstBool(val) => {
                if *val {
                    "True".to_string()
                } else {
                    "False".to_string()
                }
            }
            Self::ConstVec(v) => format!("[{}, {}, {}, {}]", v[0], v[1], v[2], v[3]),
            Self::Var(name, _) => {
                if name == "input" || name == "x" {
                    target_var.to_string()
                } else {
                    name.clone()
                }
            }
            Self::StateRead(reg) => format!("_r[{}]", reg % 8),
            Self::StateWrite { reg, val } => {
                let inner = val.to_python_expr_named(target_var);
                format!(
                    "(lambda _v: (_r.__setitem__({}, _v), _v)[1])({})",
                    reg % 8,
                    inner
                )
            }
            Self::StateVecRead(reg) => format!("_vr[{}]", reg % 4),
            Self::StateVecWrite { reg, val } => {
                let inner = val.to_python_expr_named(target_var);
                format!(
                    "(lambda _v: (_vr.__setitem__({}, _v), _v)[1])({})",
                    reg % 4,
                    inner
                )
            }
            Self::StateAccum { reg, val, factor } => {
                let inner = val.to_python_expr_named(target_var);
                let f = if *factor == 0 { 1 } else { *factor };
                format!(
                    "(lambda _v: (_r.__setitem__({0}, _r[{0}] + (_v - _r[{0}]) // {1}), _r[{0}])[1])({2})",
                    reg % 8, f, inner
                )
            }
            Self::Unary { op, child } => {
                let inner = child.to_python_expr_named(target_var);
                match op {
                    UnaryOp::Neg => format!("(-{inner})"),
                    UnaryOp::Abs => format!("abs({inner})"),
                    UnaryOp::Not => format!("(not {inner})"),
                }
            }
            Self::Binary { op, lhs, rhs } => {
                let l = lhs.to_python_expr_named(target_var);
                let r = rhs.to_python_expr_named(target_var);
                match op {
                    BinaryOp::Add => format!("({l} + {r})"),
                    BinaryOp::Sub => format!("({l} - {r})"),
                    BinaryOp::Mul => format!("({l} * {r})"),
                    BinaryOp::DivChecked => {
                        format!("(1 if ({r}) == 0 else ({l} // {r}))")
                    }
                    BinaryOp::ModChecked => {
                        format!("(0 if ({r}) == 0 else ({l} % {r}))")
                    }
                    BinaryOp::Min => format!("min({l}, {r})"),
                    BinaryOp::Max => format!("max({l}, {r})"),
                    BinaryOp::Eq => format!("({l} == {r})"),
                    BinaryOp::Ne => format!("({l} != {r})"),
                    BinaryOp::Lt => format!("({l} < {r})"),
                    BinaryOp::Le => format!("({l} <= {r})"),
                    BinaryOp::Gt => format!("({l} > {r})"),
                    BinaryOp::Ge => format!("({l} >= {r})"),
                    BinaryOp::And => format!("({l} and {r})"),
                    BinaryOp::Or => format!("({l} or {r})"),
                }
            }
            Self::VecUnary { op, child } => {
                let c = child.to_python_expr_named(target_var);
                match op {
                    VecUnaryOp::Neg => format!("[-_a for _a in {c}]"),
                    VecUnaryOp::Abs => format!("[abs(_a) for _a in {c}]"),
                    VecUnaryOp::Sum => format!("sum({c})"),
                    VecUnaryOp::Mean => format!("(sum({c}) // 4)"),
                    VecUnaryOp::MinElement => format!("min({c})"),
                    VecUnaryOp::MaxElement => format!("max({c})"),
                    VecUnaryOp::ArgMax => format!("max(range(4), key=lambda _i: {c}[_i])"),
                }
            }
            Self::VecBinary { op, lhs, rhs } => {
                let l = lhs.to_python_expr_named(target_var);
                let r = rhs.to_python_expr_named(target_var);
                match op {
                    VecBinaryOp::Add => format!("[_a + _b for _a, _b in zip({l}, {r})]"),
                    VecBinaryOp::Sub => format!("[_a - _b for _a, _b in zip({l}, {r})]"),
                    VecBinaryOp::Mul => format!("[_a * _b for _a, _b in zip({l}, {r})]"),
                    VecBinaryOp::Min => format!("[min(_a, _b) for _a, _b in zip({l}, {r})]"),
                    VecBinaryOp::Max => format!("[max(_a, _b) for _a, _b in zip({l}, {r})]"),
                    VecBinaryOp::Scale => format!("[_a * ({r}) for _a in {l}]"),
                    VecBinaryOp::Shift => format!("[_a + ({r}) for _a in {l}]"),
                    VecBinaryOp::Dot => format!("sum(_a * _b for _a, _b in zip({l}, {r}))"),
                }
            }
            Self::VecPack(elems) => {
                let e0 = elems[0].to_python_expr_named(target_var);
                let e1 = elems[1].to_python_expr_named(target_var);
                let e2 = elems[2].to_python_expr_named(target_var);
                let e3 = elems[3].to_python_expr_named(target_var);
                format!("[{e0}, {e1}, {e2}, {e3}]")
            }
            Self::VecExtract { vec, index } => {
                let v = vec.to_python_expr_named(target_var);
                format!("{v}[{}]", index % 4)
            }
            Self::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => {
                let c = cond.to_python_expr_named(target_var);
                let t = then_branch.to_python_expr_named(target_var);
                let e = else_branch.to_python_expr_named(target_var);
                format!("({t} if {c} else {e})")
            }
        }
    }

    /// Emits a complete, typed Python function definition.
    pub fn to_python_func(&self, func_name: &str, param_name: &str) -> String {
        let expr = self.to_python_expr_named(param_name);
        if !self.uses_state() && !self.uses_vectors() {
            format!(
                "def {func_name}({param_name}: int) -> int:\n    \"\"\"Evolved candidate generated by IRIS Genetic Programming Engine.\"\"\"\n    return {expr}\n"
            )
        } else {
            format!(
                "def {func_name}({param_name}, _r: list = None, _vr: list = None):\n    \"\"\"Evolved candidate generated by IRIS Genetic Programming Engine.\"\"\"\n    if _r is None: _r = [0]*8\n    if _vr is None: _vr = [[0,0,0,0] for _ in range(4)]\n    return {expr}\n"
            )
        }
    }

    /// Emits an inline Python lambda expression.
    pub fn to_python_lambda(&self, param_name: &str) -> String {
        let expr = self.to_python_expr_named(param_name);
        if !self.uses_state() && !self.uses_vectors() {
            format!("lambda {param_name}: {expr}")
        } else {
            format!("lambda {param_name}, _r=[0,0,0,0,0,0,0,0], _vr=[[0,0,0,0],[0,0,0,0],[0,0,0,0],[0,0,0,0]]: {expr}")
        }
    }
}
