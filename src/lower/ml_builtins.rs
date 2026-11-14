//! Built-in ML/AI list and tensor math intrinsics expanded directly to core SSA IR ops.

use crate::error::LowerError;
use crate::ir::instr::{BinOp, IrInstr, ScalarUnaryOp};
use crate::ir::types::{DType, IrType};
use crate::ir::value::ValueId;
use crate::parser::ast::AstExpr;
use crate::parser::lexer::Span;

use super::{builtin_arity_error, Lowerer};

impl<'m> Lowerer<'m> {
    // -----------------------------------------------------------------------
    // ML/AI intrinsics (Phases 77–80)
    // All are macro-expanded to existing IR ops — no new IrInstr variants.
    // -----------------------------------------------------------------------

    // ── Phase 77: Array creation ────────────────────────────────────────────

    pub(super) fn lower_ml_zeros(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "zeros(n) expects 1 argument",
                args.len(),
                span,
            ));
        }
        let (n_val, _) = self.lower_expr(&args[0])?;
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let list_ty = IrType::List(Box::new(f64_ty.clone()));

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );
        let zero_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: zero_f,
                value: 0.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some("zeros_hdr"));
        let body = self.builder.create_block(Some("zeros_body"));
        let merge = self.builder.create_block(Some("zeros_merge"));
        let idx = self
            .builder
            .add_block_param(hdr, Some("zi"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("zi_fin"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: n_val,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx],
            },
            None,
        );

        self.builder.set_current_block(body);
        self.builder.push_instr(
            IrInstr::ListPush {
                list: result,
                value: zero_f,
            },
            None,
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((result, list_ty))
    }

    pub(super) fn lower_ml_ones(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "ones(n) expects 1 argument",
                args.len(),
                span,
            ));
        }
        let (n_val, _) = self.lower_expr(&args[0])?;
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let list_ty = IrType::List(Box::new(f64_ty.clone()));

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );
        let one_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: one_f,
                value: 1.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some("ones_hdr"));
        let body = self.builder.create_block(Some("ones_body"));
        let merge = self.builder.create_block(Some("ones_merge"));
        let idx = self
            .builder
            .add_block_param(hdr, Some("oi"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("oi_fin"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: n_val,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx],
            },
            None,
        );

        self.builder.set_current_block(body);
        self.builder.push_instr(
            IrInstr::ListPush {
                list: result,
                value: one_f,
            },
            None,
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((result, list_ty))
    }

    pub(super) fn lower_ml_fill(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 2 {
            return Err(builtin_arity_error(
                "fill(n, v) expects 2 arguments",
                args.len(),
                span,
            ));
        }
        let (n_val, _) = self.lower_expr(&args[0])?;
        let (val_raw, val_ty) = self.lower_expr(&args[1])?;
        let f64_ty = IrType::Scalar(DType::F64);
        // Coerce fill value to f64 if needed
        let val_v = if val_ty != f64_ty {
            let coerced = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result: coerced,
                    operand: val_raw,
                    from_ty: val_ty,
                    to_ty: f64_ty.clone(),
                },
                Some(f64_ty.clone()),
            );
            coerced
        } else {
            val_raw
        };
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let list_ty = IrType::List(Box::new(f64_ty.clone()));

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some("fill_hdr"));
        let body = self.builder.create_block(Some("fill_body"));
        let merge = self.builder.create_block(Some("fill_merge"));
        let idx = self
            .builder
            .add_block_param(hdr, Some("fi"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("fi_fin"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: n_val,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx],
            },
            None,
        );

        self.builder.set_current_block(body);
        self.builder.push_instr(
            IrInstr::ListPush {
                list: result,
                value: val_v,
            },
            None,
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((result, list_ty))
    }

    pub(super) fn lower_ml_linspace(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // linspace(start: f64, end: f64, n: i64) -> list<f64>
        if args.len() != 3 {
            return Err(builtin_arity_error(
                "linspace(start, end, n) expects 3 arguments",
                args.len(),
                span,
            ));
        }
        let (start_raw, start_ty) = self.lower_expr(&args[0])?;
        let (end_raw, end_ty) = self.lower_expr(&args[1])?;
        let (n_val, _) = self.lower_expr(&args[2])?;
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let list_ty = IrType::List(Box::new(f64_ty.clone()));
        // Coerce start/end to f64
        let start_v = if start_ty != f64_ty {
            let c = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result: c,
                    operand: start_raw,
                    from_ty: start_ty,
                    to_ty: f64_ty.clone(),
                },
                Some(f64_ty.clone()),
            );
            c
        } else {
            start_raw
        };
        let end_v = if end_ty != f64_ty {
            let c = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result: c,
                    operand: end_raw,
                    from_ty: end_ty,
                    to_ty: f64_ty.clone(),
                },
                Some(f64_ty.clone()),
            );
            c
        } else {
            end_raw
        };

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );

        // step = (end - start) / (n - 1)
        let range = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: range,
                op: BinOp::Sub,
                lhs: end_v,
                rhs: start_v,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        // n_f = cast(n, f64)
        let n_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::Cast {
                result: n_f,
                operand: n_val,
                from_ty: i64_ty.clone(),
                to_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let one_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: one_f,
                value: 1.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let n_m1_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: n_m1_f,
                op: BinOp::Sub,
                lhs: n_f,
                rhs: one_f,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let step = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: step,
                op: BinOp::Div,
                lhs: range,
                rhs: n_m1_f,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );

        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some("lsp_hdr"));
        let body = self.builder.create_block(Some("lsp_body"));
        let merge = self.builder.create_block(Some("lsp_merge"));
        let idx = self
            .builder
            .add_block_param(hdr, Some("lsp_i"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("lsp_i_fin"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: n_val,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx],
            },
            None,
        );

        self.builder.set_current_block(body);
        // val = start + i * step
        let idx_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::Cast {
                result: idx_f,
                operand: idx,
                from_ty: i64_ty.clone(),
                to_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let offset = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: offset,
                op: BinOp::Mul,
                lhs: idx_f,
                rhs: step,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: val,
                op: BinOp::Add,
                lhs: start_v,
                rhs: offset,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::ListPush {
                list: result,
                value: val,
            },
            None,
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((result, list_ty))
    }

    pub(super) fn lower_ml_arange(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // arange(start: f64, end: f64, step: f64) -> list<f64>
        if args.len() != 3 {
            return Err(builtin_arity_error(
                "arange(start, end, step) expects 3 arguments",
                args.len(),
                span,
            ));
        }
        let (start_raw, start_ty) = self.lower_expr(&args[0])?;
        let (end_raw, end_ty) = self.lower_expr(&args[1])?;
        let (step_raw, step_ty) = self.lower_expr(&args[2])?;
        let f64_ty = IrType::Scalar(DType::F64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let list_ty = IrType::List(Box::new(f64_ty.clone()));

        // Coerce all inputs to f64
        let coerce = |builder: &mut crate::ir::module::IrFunctionBuilder,
                      v: ValueId,
                      ty: IrType|
         -> ValueId {
            if ty == f64_ty {
                v
            } else {
                let c = builder.fresh_value();
                builder.push_instr(
                    IrInstr::Cast {
                        result: c,
                        operand: v,
                        from_ty: ty,
                        to_ty: f64_ty.clone(),
                    },
                    Some(f64_ty.clone()),
                );
                c
            }
        };
        let start_v = coerce(&mut self.builder, start_raw, start_ty);
        let end_v = coerce(&mut self.builder, end_raw, end_ty);
        let step_v = coerce(&mut self.builder, step_raw, step_ty);

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );

        // Loop: cur = start; while cur < end { push(cur); cur += step }
        let hdr = self.builder.create_block(Some("arange_hdr"));
        let body = self.builder.create_block(Some("arange_body"));
        let merge = self.builder.create_block(Some("arange_merge"));
        let cur = self
            .builder
            .add_block_param(hdr, Some("ar_cur"), f64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("ar_fin"), f64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![start_v],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: cur,
                rhs: end_v,
                ty: f64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![cur],
            },
            None,
        );

        self.builder.set_current_block(body);
        self.builder.push_instr(
            IrInstr::ListPush {
                list: result,
                value: cur,
            },
            None,
        );
        let next_cur = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_cur,
                op: BinOp::Add,
                lhs: cur,
                rhs: step_v,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_cur],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((result, list_ty))
    }

    // ── Phase 78: Array reductions ──────────────────────────────────────────

    /// Shared loop body for f64 accumulator reductions.
    /// Returns (acc_fin, f64_ty) — caller emits the loop with custom acc update.
    fn ml_reduce_loop(
        &mut self,
        prefix: &str,
        list_val: ValueId,
        elem_ty: IrType,
        acc_init: ValueId,
    ) -> (
        crate::ir::block::BlockId,
        crate::ir::block::BlockId,
        crate::ir::block::BlockId,
        ValueId,
        ValueId,
        ValueId,
        ValueId,
    ) {
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let f64_ty = elem_ty.clone();

        let len = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len,
                list: list_val,
            },
            Some(i64_ty.clone()),
        );
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some(&format!("{}_hdr", prefix)));
        let body = self.builder.create_block(Some(&format!("{}_body", prefix)));
        let merge = self
            .builder
            .create_block(Some(&format!("{}_merge", prefix)));

        let idx = self
            .builder
            .add_block_param(hdr, Some(&format!("{}_i", prefix)), i64_ty.clone());
        let acc =
            self.builder
                .add_block_param(hdr, Some(&format!("{}_acc", prefix)), f64_ty.clone());
        let _idx_fin =
            self.builder
                .add_block_param(merge, Some(&format!("{}_if", prefix)), i64_ty.clone());
        let acc_fin =
            self.builder
                .add_block_param(merge, Some(&format!("{}_af", prefix)), f64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init, acc_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: len,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx, acc],
            },
            None,
        );

        // Return values needed by caller to populate body
        (hdr, body, merge, idx, acc, acc_fin, len)
    }

    pub(super) fn lower_ml_list_sum(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list_sum(v) expects 1 argument",
                args.len(),
                span,
            ));
        }
        let (v_val, v_ty) = self.lower_expr(&args[0])?;
        let elem_ty = match &v_ty {
            IrType::List(e) => *e.clone(),
            _ => IrType::Scalar(DType::F64),
        };
        let i64_ty = IrType::Scalar(DType::I64);

        let acc_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: acc_init,
                value: 0.0,
                ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );

        let (hdr, body, merge, idx, acc, acc_fin, _len) =
            self.ml_reduce_loop("sum", v_val, elem_ty.clone(), acc_init);

        self.builder.set_current_block(body);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: v_val,
                index: idx,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let new_acc = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: new_acc,
                op: BinOp::Add,
                lhs: acc,
                rhs: elem,
                ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i, new_acc],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((acc_fin, elem_ty))
    }

    pub(super) fn lower_ml_list_mean(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list_mean(v) expects 1 argument",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        // Reuse list_sum then divide by len
        let (sum_v, _) = self.lower_ml_list_sum(args, span)?;
        let v_val = self.lower_expr(&args[0])?.0; // re-lower (already computed, but we need len)
        let len_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_v,
                list: v_val,
            },
            Some(i64_ty.clone()),
        );
        let len_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::Cast {
                result: len_f,
                operand: len_v,
                from_ty: i64_ty.clone(),
                to_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let mean_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: mean_v,
                op: BinOp::Div,
                lhs: sum_v,
                rhs: len_f,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        Ok((mean_v, f64_ty))
    }

    pub(super) fn lower_ml_list_max_val(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list_max_val(v) expects 1 argument",
                args.len(),
                span,
            ));
        }
        let (v_val, v_ty) = self.lower_expr(&args[0])?;
        let elem_ty = match &v_ty {
            IrType::List(e) => *e.clone(),
            _ => IrType::Scalar(DType::F64),
        };
        let i64_ty = IrType::Scalar(DType::I64);

        // Initialize with first element
        let zero_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: zero_i,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let acc_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: acc_init,
                list: v_val,
                index: zero_i,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );

        let (hdr, body, merge, idx, acc, acc_fin, _len) =
            self.ml_reduce_loop("max", v_val, elem_ty.clone(), acc_init);

        self.builder.set_current_block(body);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: v_val,
                index: idx,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let new_acc = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: new_acc,
                op: BinOp::Max,
                lhs: acc,
                rhs: elem,
                ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i, new_acc],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((acc_fin, elem_ty))
    }

    pub(super) fn lower_ml_list_min_val(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list_min_val(v) expects 1 argument",
                args.len(),
                span,
            ));
        }
        let (v_val, v_ty) = self.lower_expr(&args[0])?;
        let elem_ty = match &v_ty {
            IrType::List(e) => *e.clone(),
            _ => IrType::Scalar(DType::F64),
        };
        let i64_ty = IrType::Scalar(DType::I64);

        let zero_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: zero_i,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let acc_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: acc_init,
                list: v_val,
                index: zero_i,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );

        let (hdr, body, merge, idx, acc, acc_fin, _len) =
            self.ml_reduce_loop("min", v_val, elem_ty.clone(), acc_init);

        self.builder.set_current_block(body);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: v_val,
                index: idx,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let new_acc = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: new_acc,
                op: BinOp::Min,
                lhs: acc,
                rhs: elem,
                ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i, new_acc],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((acc_fin, elem_ty))
    }

    pub(super) fn lower_ml_list_std(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // std(v) = sqrt(mean((v - mean(v))^2))
        // Computed in two passes via two calls to ml_list_sum and arithmetic
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list_std(v) expects 1 argument",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let elem_ty = f64_ty.clone();

        let (v_val, _v_ty) = self.lower_expr(&args[0])?;

        // mean
        let len_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_v,
                list: v_val,
            },
            Some(i64_ty.clone()),
        );
        let len_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::Cast {
                result: len_f,
                operand: len_v,
                from_ty: i64_ty.clone(),
                to_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );

        // sum for mean
        let acc0 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: acc0,
                value: 0.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let (h1, b1, m1, i1, a1, sum_v, _) =
            self.ml_reduce_loop("std_sum", v_val, elem_ty.clone(), acc0);
        self.builder.set_current_block(b1);
        let e1 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: e1,
                list: v_val,
                index: i1,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let na1 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: na1,
                op: BinOp::Add,
                lhs: a1,
                rhs: e1,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let one1 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one1,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let ni1 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: ni1,
                op: BinOp::Add,
                lhs: i1,
                rhs: one1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: h1,
                args: vec![ni1, na1],
            },
            None,
        );
        self.builder.set_current_block(m1);

        let mean_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: mean_v,
                op: BinOp::Div,
                lhs: sum_v,
                rhs: len_f,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );

        // sum of squared deviations
        let acc2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: acc2,
                value: 0.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let (h2, b2, m2, i2, a2, var_sum, _) =
            self.ml_reduce_loop("std_var", v_val, elem_ty.clone(), acc2);
        self.builder.set_current_block(b2);
        let e2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: e2,
                list: v_val,
                index: i2,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let diff = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: diff,
                op: BinOp::Sub,
                lhs: e2,
                rhs: mean_v,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let sq = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: sq,
                op: BinOp::Mul,
                lhs: diff,
                rhs: diff,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let na2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: na2,
                op: BinOp::Add,
                lhs: a2,
                rhs: sq,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let one2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one2,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let ni2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: ni2,
                op: BinOp::Add,
                lhs: i2,
                rhs: one2,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: h2,
                args: vec![ni2, na2],
            },
            None,
        );
        self.builder.set_current_block(m2);

        let var = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: var,
                op: BinOp::Div,
                lhs: var_sum,
                rhs: len_f,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let std_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::UnaryOp {
                result: std_v,
                op: ScalarUnaryOp::Sqrt,
                operand: var,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );

        let _ = span;
        Ok((std_v, f64_ty))
    }

    pub(super) fn lower_ml_list_norm(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // norm(v) = sqrt(sum(v[i]^2))
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list_norm(v) expects 1 argument",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let elem_ty = f64_ty.clone();
        let (v_val, _) = self.lower_expr(&args[0])?;

        let acc0 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: acc0,
                value: 0.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let (hdr, body, merge, idx, acc, sum_sq, _) =
            self.ml_reduce_loop("norm", v_val, elem_ty.clone(), acc0);

        self.builder.set_current_block(body);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: v_val,
                index: idx,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let sq = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: sq,
                op: BinOp::Mul,
                lhs: elem,
                rhs: elem,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let new_acc = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: new_acc,
                op: BinOp::Add,
                lhs: acc,
                rhs: sq,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i, new_acc],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let norm_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::UnaryOp {
                result: norm_v,
                op: ScalarUnaryOp::Sqrt,
                operand: sum_sq,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let _ = span;
        Ok((norm_v, f64_ty))
    }

    pub(super) fn lower_ml_list_dot(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // dot(a, b) = sum(a[i]*b[i])
        if args.len() != 2 {
            return Err(builtin_arity_error(
                "list_dot(a, b) expects 2 arguments",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let elem_ty = f64_ty.clone();
        let (a_val, _) = self.lower_expr(&args[0])?;
        let (b_val, _) = self.lower_expr(&args[1])?;

        let acc0 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: acc0,
                value: 0.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let (hdr, body, merge, idx, acc, dot_v, _) =
            self.ml_reduce_loop("dot", a_val, elem_ty.clone(), acc0);

        self.builder.set_current_block(body);
        let ea = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: ea,
                list: a_val,
                index: idx,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let eb = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: eb,
                list: b_val,
                index: idx,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let prod = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: prod,
                op: BinOp::Mul,
                lhs: ea,
                rhs: eb,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let new_acc = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: new_acc,
                op: BinOp::Add,
                lhs: acc,
                rhs: prod,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i, new_acc],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((dot_v, f64_ty))
    }

    // ── Phase 79: Elementwise ops ───────────────────────────────────────────

    /// Generic elementwise binary op on two lists: result[i] = op(a[i], b[i]).
    pub(super) fn lower_ml_list_binop(
        &mut self,
        args: &[AstExpr],
        span: Span,
        op: BinOp,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 2 {
            return Err(builtin_arity_error(
                "elementwise list op expects 2 arguments",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let elem_ty = f64_ty.clone();
        let list_ty = IrType::List(Box::new(f64_ty.clone()));

        let (a_val, _) = self.lower_expr(&args[0])?;
        let (b_val, _) = self.lower_expr(&args[1])?;

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result,
                elem_ty: elem_ty.clone(),
            },
            Some(list_ty.clone()),
        );
        let len_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_v,
                list: a_val,
            },
            Some(i64_ty.clone()),
        );
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some("lbop_hdr"));
        let body = self.builder.create_block(Some("lbop_body"));
        let merge = self.builder.create_block(Some("lbop_merge"));
        let idx = self
            .builder
            .add_block_param(hdr, Some("lbop_i"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("lbop_if"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: len_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx],
            },
            None,
        );

        self.builder.set_current_block(body);
        let ea = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: ea,
                list: a_val,
                index: idx,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let eb = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: eb,
                list: b_val,
                index: idx,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let out = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: out,
                op,
                lhs: ea,
                rhs: eb,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::ListPush {
                list: result,
                value: out,
            },
            None,
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((result, list_ty))
    }

    pub(super) fn lower_ml_list_scale(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // list_scale(v, s) = [v[i] * s for i in 0..len(v)]
        if args.len() != 2 {
            return Err(builtin_arity_error(
                "list_scale(v, s) expects 2 arguments",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let list_ty = IrType::List(Box::new(f64_ty.clone()));

        let (v_val, _) = self.lower_expr(&args[0])?;
        let (s_raw, s_ty) = self.lower_expr(&args[1])?;
        // Coerce scalar to f64
        let s_val = if s_ty != f64_ty {
            let c = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result: c,
                    operand: s_raw,
                    from_ty: s_ty,
                    to_ty: f64_ty.clone(),
                },
                Some(f64_ty.clone()),
            );
            c
        } else {
            s_raw
        };

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );
        let len_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_v,
                list: v_val,
            },
            Some(i64_ty.clone()),
        );
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some("scl_hdr"));
        let body = self.builder.create_block(Some("scl_body"));
        let merge = self.builder.create_block(Some("scl_merge"));
        let idx = self
            .builder
            .add_block_param(hdr, Some("scl_i"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("scl_if"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: len_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx],
            },
            None,
        );

        self.builder.set_current_block(body);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: v_val,
                index: idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let out = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: out,
                op: BinOp::Mul,
                lhs: elem,
                rhs: s_val,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::ListPush {
                list: result,
                value: out,
            },
            None,
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((result, list_ty))
    }

    pub(super) fn lower_ml_list_relu(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // relu(v) = [max(0, v[i]) for i in 0..len(v)]
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list_relu(v) expects 1 argument",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let list_ty = IrType::List(Box::new(f64_ty.clone()));

        let (v_val, _) = self.lower_expr(&args[0])?;
        let zero_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: zero_f,
                value: 0.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );
        let len_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_v,
                list: v_val,
            },
            Some(i64_ty.clone()),
        );
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some("relu_hdr"));
        let body = self.builder.create_block(Some("relu_body"));
        let merge = self.builder.create_block(Some("relu_merge"));
        let idx = self
            .builder
            .add_block_param(hdr, Some("relu_i"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("relu_if"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: len_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx],
            },
            None,
        );

        self.builder.set_current_block(body);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: v_val,
                index: idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let out = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: out,
                op: BinOp::Max,
                lhs: elem,
                rhs: zero_f,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::ListPush {
                list: result,
                value: out,
            },
            None,
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((result, list_ty))
    }

    pub(super) fn lower_ml_list_sigmoid(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // sigmoid(v) = [1 / (1 + exp(-v[i])) for i]
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list_sigmoid(v) expects 1 argument",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let list_ty = IrType::List(Box::new(f64_ty.clone()));

        let (v_val, _) = self.lower_expr(&args[0])?;
        let one_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: one_f,
                value: 1.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );
        let len_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_v,
                list: v_val,
            },
            Some(i64_ty.clone()),
        );
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some("sig_hdr"));
        let body = self.builder.create_block(Some("sig_body"));
        let merge = self.builder.create_block(Some("sig_merge"));
        let idx = self
            .builder
            .add_block_param(hdr, Some("sig_i"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("sig_if"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: len_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx],
            },
            None,
        );

        self.builder.set_current_block(body);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: v_val,
                index: idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let neg_e = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::UnaryOp {
                result: neg_e,
                op: ScalarUnaryOp::Neg,
                operand: elem,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let exp_e = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::UnaryOp {
                result: exp_e,
                op: ScalarUnaryOp::Exp,
                operand: neg_e,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let denom = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: denom,
                op: BinOp::Add,
                lhs: one_f,
                rhs: exp_e,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let out = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: out,
                op: BinOp::Div,
                lhs: one_f,
                rhs: denom,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::ListPush {
                list: result,
                value: out,
            },
            None,
        );
        let one_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one_i,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one_i,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((result, list_ty))
    }

    pub(super) fn lower_ml_list_softmax(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // softmax(v): exp-shift stable softmax
        //   max_v = max(v)
        //   exp_v = [exp(v[i] - max_v) for i]
        //   sum_e = sum(exp_v)
        //   result = [e / sum_e for e in exp_v]
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list_softmax(v) expects 1 argument",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let list_ty = IrType::List(Box::new(f64_ty.clone()));

        let (v_val, _) = self.lower_expr(&args[0])?;

        // max_v using list_max_val pattern (inline to avoid re-lowering args)
        let zero_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: zero_i,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let max_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: max_init,
                list: v_val,
                index: zero_i,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let acc0 = max_init;
        let (h1, b1, m1, i1, a1, max_v, len_v) =
            self.ml_reduce_loop("smx_max", v_val, f64_ty.clone(), acc0);
        self.builder.set_current_block(b1);
        let e1 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: e1,
                list: v_val,
                index: i1,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let na1 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: na1,
                op: BinOp::Max,
                lhs: a1,
                rhs: e1,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let one1 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one1,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let ni1 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: ni1,
                op: BinOp::Add,
                lhs: i1,
                rhs: one1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: h1,
                args: vec![ni1, na1],
            },
            None,
        );
        self.builder.set_current_block(m1);

        // exp_v: new list of exp(v[i] - max_v)
        let exp_list = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result: exp_list,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );
        let idx_init2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init2,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let h2 = self.builder.create_block(Some("smx_exp_hdr"));
        let b2 = self.builder.create_block(Some("smx_exp_body"));
        let m2 = self.builder.create_block(Some("smx_exp_merge"));
        let i2 = self
            .builder
            .add_block_param(h2, Some("smx_ei"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(m2, Some("smx_eif"), i64_ty.clone());
        self.builder.push_instr(
            IrInstr::Br {
                target: h2,
                args: vec![idx_init2],
            },
            None,
        );
        self.builder.set_current_block(h2);
        let c2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: c2,
                op: BinOp::CmpLt,
                lhs: i2,
                rhs: len_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond: c2,
                then_block: b2,
                then_args: vec![],
                else_block: m2,
                else_args: vec![i2],
            },
            None,
        );
        self.builder.set_current_block(b2);
        let ve2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: ve2,
                list: v_val,
                index: i2,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let shifted = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: shifted,
                op: BinOp::Sub,
                lhs: ve2,
                rhs: max_v,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let expv = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::UnaryOp {
                result: expv,
                op: ScalarUnaryOp::Exp,
                operand: shifted,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::ListPush {
                list: exp_list,
                value: expv,
            },
            None,
        );
        let one2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one2,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let ni2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: ni2,
                op: BinOp::Add,
                lhs: i2,
                rhs: one2,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: h2,
                args: vec![ni2],
            },
            None,
        );
        self.builder.set_current_block(m2);

        // sum_exp: sum of exp_list
        let acc_s = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: acc_s,
                value: 0.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let (h3, b3, m3, i3, a3, sum_exp, _) =
            self.ml_reduce_loop("smx_sum", exp_list, f64_ty.clone(), acc_s);
        self.builder.set_current_block(b3);
        let e3 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: e3,
                list: exp_list,
                index: i3,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let na3 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: na3,
                op: BinOp::Add,
                lhs: a3,
                rhs: e3,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let one3 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one3,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let ni3 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: ni3,
                op: BinOp::Add,
                lhs: i3,
                rhs: one3,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: h3,
                args: vec![ni3, na3],
            },
            None,
        );
        self.builder.set_current_block(m3);

        // normalize: result[i] = exp_list[i] / sum_exp
        let out_list = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result: out_list,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );
        let idx_init4 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init4,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let h4 = self.builder.create_block(Some("smx_norm_hdr"));
        let b4 = self.builder.create_block(Some("smx_norm_body"));
        let m4 = self.builder.create_block(Some("smx_norm_merge"));
        let i4 = self
            .builder
            .add_block_param(h4, Some("smx_ni"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(m4, Some("smx_nif"), i64_ty.clone());
        self.builder.push_instr(
            IrInstr::Br {
                target: h4,
                args: vec![idx_init4],
            },
            None,
        );
        self.builder.set_current_block(h4);
        let c4 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: c4,
                op: BinOp::CmpLt,
                lhs: i4,
                rhs: len_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond: c4,
                then_block: b4,
                then_args: vec![],
                else_block: m4,
                else_args: vec![i4],
            },
            None,
        );
        self.builder.set_current_block(b4);
        let ev4 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: ev4,
                list: exp_list,
                index: i4,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let norm_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: norm_v,
                op: BinOp::Div,
                lhs: ev4,
                rhs: sum_exp,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::ListPush {
                list: out_list,
                value: norm_v,
            },
            None,
        );
        let one4 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one4,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let ni4 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: ni4,
                op: BinOp::Add,
                lhs: i4,
                rhs: one4,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: h4,
                args: vec![ni4],
            },
            None,
        );
        self.builder.set_current_block(m4);

        let _ = span;
        Ok((out_list, list_ty))
    }

    // ── Phase 80: Loss functions and training ───────────────────────────────

    pub(super) fn lower_ml_mse_loss(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // mse_loss(pred, target) = mean((pred[i] - target[i])^2)
        if args.len() != 2 {
            return Err(builtin_arity_error(
                "mse_loss(pred, target) expects 2 arguments",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);

        let (pred_v, _) = self.lower_expr(&args[0])?;
        let (target_v, _) = self.lower_expr(&args[1])?;

        let len_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_v,
                list: pred_v,
            },
            Some(i64_ty.clone()),
        );
        let acc0 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: acc0,
                value: 0.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );

        let (hdr, body, merge, idx, acc, sum_sq, _) =
            self.ml_reduce_loop("mse", pred_v, f64_ty.clone(), acc0);

        self.builder.set_current_block(body);
        let p_e = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: p_e,
                list: pred_v,
                index: idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let t_e = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: t_e,
                list: target_v,
                index: idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let diff = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: diff,
                op: BinOp::Sub,
                lhs: p_e,
                rhs: t_e,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let sq = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: sq,
                op: BinOp::Mul,
                lhs: diff,
                rhs: diff,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let new_acc = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: new_acc,
                op: BinOp::Add,
                lhs: acc,
                rhs: sq,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i, new_acc],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let len_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::Cast {
                result: len_f,
                operand: len_v,
                from_ty: i64_ty.clone(),
                to_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let mse = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: mse,
                op: BinOp::Div,
                lhs: sum_sq,
                rhs: len_f,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );

        let _ = span;
        Ok((mse, f64_ty))
    }

    pub(super) fn lower_ml_cross_entropy(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // cross_entropy(probs: list<f64>, targets: list<f64>) = -mean(targets * log(probs + eps))
        if args.len() != 2 {
            return Err(builtin_arity_error(
                "cross_entropy(probs, targets) expects 2 arguments",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);

        let (probs_v, _) = self.lower_expr(&args[0])?;
        let (target_v, _) = self.lower_expr(&args[1])?;

        let len_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_v,
                list: probs_v,
            },
            Some(i64_ty.clone()),
        );

        // eps = 1e-9 to avoid log(0)
        let eps = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: eps,
                value: 1e-9,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );

        let acc_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: acc_init,
                value: 0.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some("ce_hdr"));
        let body = self.builder.create_block(Some("ce_body"));
        let merge = self.builder.create_block(Some("ce_merge"));
        let idx = self
            .builder
            .add_block_param(hdr, Some("ce_i"), i64_ty.clone());
        let acc = self
            .builder
            .add_block_param(hdr, Some("ce_acc"), f64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("ce_if"), i64_ty.clone());
        let sum_v = self
            .builder
            .add_block_param(merge, Some("ce_s"), f64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init, acc_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: len_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx, acc],
            },
            None,
        );

        self.builder.set_current_block(body);
        let p_e = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: p_e,
                list: probs_v,
                index: idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let t_e = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: t_e,
                list: target_v,
                index: idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let p_eps = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: p_eps,
                op: BinOp::Add,
                lhs: p_e,
                rhs: eps,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let log_p = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::UnaryOp {
                result: log_p,
                op: ScalarUnaryOp::Log,
                operand: p_eps,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let t_log = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: t_log,
                op: BinOp::Mul,
                lhs: t_e,
                rhs: log_p,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let new_acc = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: new_acc,
                op: BinOp::Add,
                lhs: acc,
                rhs: t_log,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i, new_acc],
            },
            None,
        );

        // merge: result = -sum / len
        self.builder.set_current_block(merge);
        let len_f = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::Cast {
                result: len_f,
                operand: len_v,
                from_ty: i64_ty.clone(),
                to_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let mean_sum = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: mean_sum,
                op: BinOp::Div,
                lhs: sum_v,
                rhs: len_f,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let neg_ce = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::UnaryOp {
                result: neg_ce,
                op: ScalarUnaryOp::Neg,
                operand: mean_sum,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );

        let _ = span;
        Ok((neg_ce, f64_ty))
    }

    pub(super) fn lower_ml_list_axpy(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // axpy(alpha, x, y) = [alpha*x[i] + y[i] for i]  (BLAS: y = alpha*x + y)
        if args.len() != 3 {
            return Err(builtin_arity_error(
                "list_axpy(alpha, x, y) expects 3 arguments",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let list_ty = IrType::List(Box::new(f64_ty.clone()));

        let (alpha_raw, alpha_ty) = self.lower_expr(&args[0])?;
        let (a_val, _) = self.lower_expr(&args[1])?;
        let (b_val, _) = self.lower_expr(&args[2])?;
        // Coerce alpha to f64
        let s_val = if alpha_ty != f64_ty {
            let c = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result: c,
                    operand: alpha_raw,
                    from_ty: alpha_ty,
                    to_ty: f64_ty.clone(),
                },
                Some(f64_ty.clone()),
            );
            c
        } else {
            alpha_raw
        };

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );
        let len_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_v,
                list: a_val,
            },
            Some(i64_ty.clone()),
        );
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some("axpy_hdr"));
        let body = self.builder.create_block(Some("axpy_body"));
        let merge = self.builder.create_block(Some("axpy_merge"));
        let idx = self
            .builder
            .add_block_param(hdr, Some("axpy_i"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("axpy_if"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: len_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx],
            },
            None,
        );

        self.builder.set_current_block(body);
        let ea = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: ea,
                list: a_val,
                index: idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let eb = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: eb,
                list: b_val,
                index: idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        // out = alpha*x[i] + y[i]
        let sa = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: sa,
                op: BinOp::Mul,
                lhs: s_val,
                rhs: ea,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let out = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: out,
                op: BinOp::Add,
                lhs: sa,
                rhs: eb,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::ListPush {
                list: result,
                value: out,
            },
            None,
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i],
            },
            None,
        );

        self.builder.set_current_block(merge);
        let _ = span;
        Ok((result, list_ty))
    }

    pub(super) fn lower_ml_sgd_step(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // sgd_step(params, grads, lr): in-place params[i] -= lr * grads[i]. Returns unit (i64 0).
        if args.len() != 3 {
            return Err(builtin_arity_error(
                "sgd_step(params, grads, lr) expects 3 arguments",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);

        let (params_v, _) = self.lower_expr(&args[0])?;
        let (grads_v, _) = self.lower_expr(&args[1])?;
        let (lr_raw, lr_ty) = self.lower_expr(&args[2])?;
        // Coerce lr to f64
        let lr_val = if lr_ty != f64_ty {
            let c = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result: c,
                    operand: lr_raw,
                    from_ty: lr_ty,
                    to_ty: f64_ty.clone(),
                },
                Some(f64_ty.clone()),
            );
            c
        } else {
            lr_raw
        };

        let len_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_v,
                list: params_v,
            },
            Some(i64_ty.clone()),
        );
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let hdr = self.builder.create_block(Some("sgd_hdr"));
        let body = self.builder.create_block(Some("sgd_body"));
        let merge = self.builder.create_block(Some("sgd_merge"));
        let idx = self
            .builder
            .add_block_param(hdr, Some("sgd_i"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(merge, Some("sgd_if"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![idx_init],
            },
            None,
        );
        self.builder.set_current_block(hdr);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx,
                rhs: len_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: merge,
                else_args: vec![idx],
            },
            None,
        );

        self.builder.set_current_block(body);
        let p_e = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: p_e,
                list: params_v,
                index: idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let g_e = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: g_e,
                list: grads_v,
                index: idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let lr_g = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: lr_g,
                op: BinOp::Mul,
                lhs: lr_val,
                rhs: g_e,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let new_p = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: new_p,
                op: BinOp::Sub,
                lhs: p_e,
                rhs: lr_g,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::ListSet {
                list: params_v,
                index: idx,
                value: new_p,
            },
            None,
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: idx,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: hdr,
                args: vec![next_i],
            },
            None,
        );

        self.builder.set_current_block(merge);
        // Return unit (i64 0)
        let unit = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: unit,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let _ = span;
        Ok((unit, i64_ty))
    }

    // ── Phase 82: BLAS matmul ────────────────────────────────────────────────

    /// matmul(a, m, k, b, n) → list<f64> of length m*n
    /// A is m×k (row-major flat list), B is k×n → C is m×n.
    pub(super) fn lower_ml_matmul(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 5 {
            return Err(builtin_arity_error(
                "matmul(a, m, k, b, n) expects 5 arguments",
                args.len(),
                span,
            ));
        }
        let f64_ty = IrType::Scalar(DType::F64);
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let list_ty = IrType::List(Box::new(f64_ty.clone()));

        let (a_v, _) = self.lower_expr(&args[0])?;
        let (m_raw, m_ty) = self.lower_expr(&args[1])?;
        let (k_raw, k_ty) = self.lower_expr(&args[2])?;
        let (b_v, _) = self.lower_expr(&args[3])?;
        let (n_raw, n_ty) = self.lower_expr(&args[4])?;

        // Coerce m, k, n to i64 if needed
        let m_v = if m_ty != i64_ty {
            let c = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result: c,
                    operand: m_raw,
                    from_ty: m_ty,
                    to_ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            c
        } else {
            m_raw
        };
        let k_v = if k_ty != i64_ty {
            let c = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result: c,
                    operand: k_raw,
                    from_ty: k_ty,
                    to_ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            c
        } else {
            k_raw
        };
        let n_v = if n_ty != i64_ty {
            let c = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result: c,
                    operand: n_raw,
                    from_ty: n_ty,
                    to_ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            c
        } else {
            n_raw
        };

        // Allocate result list C
        let c_v = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result: c_v,
                elem_ty: f64_ty.clone(),
            },
            Some(list_ty.clone()),
        );

        // Outer loop: i in 0..m
        let zero = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: zero,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let i_hdr = self.builder.create_block(Some("mm_i_hdr"));
        let i_body = self.builder.create_block(Some("mm_i_body"));
        let i_merge = self.builder.create_block(Some("mm_i_merge"));
        let i_param = self
            .builder
            .add_block_param(i_hdr, Some("mm_i"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(i_merge, Some("mm_if"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: i_hdr,
                args: vec![zero],
            },
            None,
        );

        // i loop header
        self.builder.set_current_block(i_hdr);
        let i_cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: i_cond,
                op: BinOp::CmpLt,
                lhs: i_param,
                rhs: m_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond: i_cond,
                then_block: i_body,
                then_args: vec![],
                else_block: i_merge,
                else_args: vec![i_param],
            },
            None,
        );

        // i loop body — inner loop: j in 0..n
        self.builder.set_current_block(i_body);
        let zero2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: zero2,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let j_hdr = self.builder.create_block(Some("mm_j_hdr"));
        let j_body = self.builder.create_block(Some("mm_j_body"));
        let j_merge = self.builder.create_block(Some("mm_j_merge"));
        let j_param = self
            .builder
            .add_block_param(j_hdr, Some("mm_j"), i64_ty.clone());
        let _ = self
            .builder
            .add_block_param(j_merge, Some("mm_jf"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: j_hdr,
                args: vec![zero2],
            },
            None,
        );

        // j loop header
        self.builder.set_current_block(j_hdr);
        let j_cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: j_cond,
                op: BinOp::CmpLt,
                lhs: j_param,
                rhs: n_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond: j_cond,
                then_block: j_body,
                then_args: vec![],
                else_block: j_merge,
                else_args: vec![j_param],
            },
            None,
        );

        // j loop body — innermost loop: kk in 0..k
        self.builder.set_current_block(j_body);
        let zero3 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: zero3,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let acc0 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstFloat {
                result: acc0,
                value: 0.0,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );

        let k_hdr = self.builder.create_block(Some("mm_k_hdr"));
        let k_body = self.builder.create_block(Some("mm_k_body"));
        let k_merge = self.builder.create_block(Some("mm_k_merge"));
        let k_param = self
            .builder
            .add_block_param(k_hdr, Some("mm_kk"), i64_ty.clone());
        let k_acc = self
            .builder
            .add_block_param(k_hdr, Some("mm_acc"), f64_ty.clone());
        let _ = self
            .builder
            .add_block_param(k_merge, Some("mm_kf"), i64_ty.clone());
        let sum_v = self
            .builder
            .add_block_param(k_merge, Some("mm_sum"), f64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: k_hdr,
                args: vec![zero3, acc0],
            },
            None,
        );

        // k loop header
        self.builder.set_current_block(k_hdr);
        let k_cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: k_cond,
                op: BinOp::CmpLt,
                lhs: k_param,
                rhs: k_v,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond: k_cond,
                then_block: k_body,
                then_args: vec![],
                else_block: k_merge,
                else_args: vec![k_param, k_acc],
            },
            None,
        );

        // k loop body: acc += a[i*k + kk] * b[kk*n + j]
        self.builder.set_current_block(k_body);
        let i_times_k = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: i_times_k,
                op: BinOp::Mul,
                lhs: i_param,
                rhs: k_v,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let a_idx = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: a_idx,
                op: BinOp::Add,
                lhs: i_times_k,
                rhs: k_param,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let a_elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: a_elem,
                list: a_v,
                index: a_idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let kk_times_n = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: kk_times_n,
                op: BinOp::Mul,
                lhs: k_param,
                rhs: n_v,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let b_idx = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: b_idx,
                op: BinOp::Add,
                lhs: kk_times_n,
                rhs: j_param,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let b_elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: b_elem,
                list: b_v,
                index: b_idx,
                elem_ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let prod = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: prod,
                op: BinOp::Mul,
                lhs: a_elem,
                rhs: b_elem,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let new_acc = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: new_acc,
                op: BinOp::Add,
                lhs: k_acc,
                rhs: prod,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_k = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_k,
                op: BinOp::Add,
                lhs: k_param,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: k_hdr,
                args: vec![next_k, new_acc],
            },
            None,
        );

        // k merge: push sum_v to C
        self.builder.set_current_block(k_merge);
        self.builder.push_instr(
            IrInstr::ListPush {
                list: c_v,
                value: sum_v,
            },
            None,
        );
        // advance j
        let one2 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one2,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_j = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_j,
                op: BinOp::Add,
                lhs: j_param,
                rhs: one2,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: j_hdr,
                args: vec![next_j],
            },
            None,
        );

        // j merge: advance i
        self.builder.set_current_block(j_merge);
        let one3 = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one3,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_i = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_i,
                op: BinOp::Add,
                lhs: i_param,
                rhs: one3,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: i_hdr,
                args: vec![next_i],
            },
            None,
        );

        // i merge
        self.builder.set_current_block(i_merge);
        let _ = span;
        Ok((c_v, list_ty))
    }
}
