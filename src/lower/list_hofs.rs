//! List higher-order functional operations (`map`, `filter`, `fold`, `any`, `all`) lowered to SSA loops.

use crate::error::LowerError;
use crate::ir::instr::{BinOp, IrInstr};
use crate::ir::types::{DType, IrType};
use crate::ir::value::ValueId;
use crate::parser::ast::AstExpr;
use crate::parser::lexer::Span;

use super::{builtin_arity_error, Lowerer};

impl<'m> Lowerer<'m> {
    // -----------------------------------------------------------------------
    // List functional operations: map, filter, fold, any, all
    // These are desugared to SSA loops at lowering time (no new IrInstr needed).
    // -----------------------------------------------------------------------

    fn emit_list_map_loop(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        closure_val: ValueId,
        closure_ty: &IrType,
    ) -> (ValueId, IrType) {
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let mapped_elem_ty = match closure_ty {
            IrType::Fn { ret, .. } => *ret.clone(),
            _ => elem_ty.clone(),
        };
        let len_val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_val,
                list: base_val,
            },
            Some(i64_ty.clone()),
        );
        let out_list = self.builder.fresh_value();
        let out_list_ty = IrType::List(Box::new(mapped_elem_ty.clone()));
        self.builder.push_instr(
            IrInstr::ListNew {
                result: out_list,
                elem_ty: mapped_elem_ty.clone(),
            },
            Some(out_list_ty.clone()),
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

        let header_bb = self.builder.create_block(Some("map_header"));
        let body_bb = self.builder.create_block(Some("map_body"));
        let merge_bb = self.builder.create_block(Some("map_merge"));

        let idx_param = self
            .builder
            .add_block_param(header_bb, Some("map_idx"), i64_ty.clone());
        let _idx_fin = self
            .builder
            .add_block_param(merge_bb, Some("map_idx_fin"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: vec![idx_init],
            },
            None,
        );

        self.builder.set_current_block(header_bb);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx_param,
                rhs: len_val,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body_bb,
                then_args: vec![],
                else_block: merge_bb,
                else_args: vec![idx_param],
            },
            None,
        );

        self.builder.set_current_block(body_bb);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: base_val,
                index: idx_param,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let mapped = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::CallClosure {
                result: Some(mapped),
                closure: closure_val,
                args: vec![elem],
                result_ty: mapped_elem_ty.clone(),
                pass_env: true,
            },
            Some(mapped_elem_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::ListPush {
                list: out_list,
                value: mapped,
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
        let next_idx = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_idx,
                op: BinOp::Add,
                lhs: idx_param,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: vec![next_idx],
            },
            None,
        );

        self.builder.set_current_block(merge_bb);
        (out_list, out_list_ty)
    }

    pub(super) fn lower_list_map(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list.map expects 1 argument (closure)",
                args.len(),
                span,
            ));
        }
        let (closure_val, closure_ty) = self.lower_expr(&args[0])?;
        let _ = span;
        Ok(self.emit_list_map_loop(base_val, elem_ty, closure_val, &closure_ty))
    }

    pub(super) fn lower_thir_list_map(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        args: &[crate::thir::ThirExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list.map expects 1 argument (closure)",
                args.len(),
                span,
            ));
        }
        let (closure_val, closure_ty) = self.lower_thir_expr_req(&args[0])?;
        let _ = span;
        Ok(self.emit_list_map_loop(base_val, elem_ty, closure_val, &closure_ty))
    }

    fn emit_list_filter_loop(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        closure_val: ValueId,
    ) -> (ValueId, IrType) {
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let len_val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_val,
                list: base_val,
            },
            Some(i64_ty.clone()),
        );
        let out_list = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListNew {
                result: out_list,
                elem_ty: elem_ty.clone(),
            },
            Some(IrType::List(Box::new(elem_ty.clone()))),
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

        let header_bb = self.builder.create_block(Some("filter_header"));
        let body_bb = self.builder.create_block(Some("filter_body"));
        let push_bb = self.builder.create_block(Some("filter_push"));
        let inc_bb = self.builder.create_block(Some("filter_inc"));
        let merge_bb = self.builder.create_block(Some("filter_merge"));

        let idx_param = self
            .builder
            .add_block_param(header_bb, Some("filter_idx"), i64_ty.clone());
        let _idx_fin =
            self.builder
                .add_block_param(merge_bb, Some("filter_idx_fin"), i64_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: vec![idx_init],
            },
            None,
        );

        self.builder.set_current_block(header_bb);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx_param,
                rhs: len_val,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body_bb,
                then_args: vec![],
                else_block: merge_bb,
                else_args: vec![idx_param],
            },
            None,
        );

        self.builder.set_current_block(body_bb);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: base_val,
                index: idx_param,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let keep = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::CallClosure {
                result: Some(keep),
                closure: closure_val,
                args: vec![elem],
                result_ty: bool_ty.clone(),
                pass_env: true,
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond: keep,
                then_block: push_bb,
                then_args: vec![],
                else_block: inc_bb,
                else_args: vec![],
            },
            None,
        );

        self.builder.set_current_block(push_bb);
        self.builder.push_instr(
            IrInstr::ListPush {
                list: out_list,
                value: elem,
            },
            None,
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: inc_bb,
                args: vec![],
            },
            None,
        );

        self.builder.set_current_block(inc_bb);
        let one = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let next_idx = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_idx,
                op: BinOp::Add,
                lhs: idx_param,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: vec![next_idx],
            },
            None,
        );

        self.builder.set_current_block(merge_bb);
        (out_list, IrType::List(Box::new(elem_ty)))
    }

    pub(super) fn lower_list_filter(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list.filter expects 1 argument (closure)",
                args.len(),
                span,
            ));
        }
        let (closure_val, _) = self.lower_expr(&args[0])?;
        let _ = span;
        Ok(self.emit_list_filter_loop(base_val, elem_ty, closure_val))
    }

    pub(super) fn lower_thir_list_filter(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        args: &[crate::thir::ThirExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list.filter expects 1 argument (closure)",
                args.len(),
                span,
            ));
        }
        let (closure_val, _) = self.lower_thir_expr_req(&args[0])?;
        let _ = span;
        Ok(self.emit_list_filter_loop(base_val, elem_ty, closure_val))
    }

    fn emit_list_fold_loop(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        init_val: ValueId,
        init_ty: IrType,
        closure_val: ValueId,
    ) -> (ValueId, IrType) {
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let len_val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_val,
                list: base_val,
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

        let header_bb = self.builder.create_block(Some("fold_header"));
        let body_bb = self.builder.create_block(Some("fold_body"));
        let merge_bb = self.builder.create_block(Some("fold_merge"));

        let idx_param = self
            .builder
            .add_block_param(header_bb, Some("fold_idx"), i64_ty.clone());
        let acc_param = self
            .builder
            .add_block_param(header_bb, Some("fold_acc"), init_ty.clone());
        let _idx_fin = self
            .builder
            .add_block_param(merge_bb, Some("fold_idx_fin"), i64_ty.clone());
        let acc_fin = self
            .builder
            .add_block_param(merge_bb, Some("fold_acc_fin"), init_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: vec![idx_init, init_val],
            },
            None,
        );

        self.builder.set_current_block(header_bb);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx_param,
                rhs: len_val,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body_bb,
                then_args: vec![],
                else_block: merge_bb,
                else_args: vec![idx_param, acc_param],
            },
            None,
        );

        self.builder.set_current_block(body_bb);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: base_val,
                index: idx_param,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let new_acc = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::CallClosure {
                result: Some(new_acc),
                closure: closure_val,
                args: vec![acc_param, elem],
                result_ty: init_ty.clone(),
                pass_env: true,
            },
            Some(init_ty.clone()),
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
        let next_idx = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_idx,
                op: BinOp::Add,
                lhs: idx_param,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: vec![next_idx, new_acc],
            },
            None,
        );

        self.builder.set_current_block(merge_bb);
        (acc_fin, init_ty)
    }

    pub(super) fn lower_list_fold(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 2 {
            return Err(builtin_arity_error(
                "list.fold expects 2 arguments (init, closure)",
                args.len(),
                span,
            ));
        }
        let (init_val, init_ty) = self.lower_expr(&args[0])?;
        let (closure_val, _) = self.lower_expr(&args[1])?;
        let _ = span;
        Ok(self.emit_list_fold_loop(base_val, elem_ty, init_val, init_ty, closure_val))
    }

    pub(super) fn lower_thir_list_fold(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        args: &[crate::thir::ThirExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 2 {
            return Err(builtin_arity_error(
                "list.fold expects 2 arguments (init, closure)",
                args.len(),
                span,
            ));
        }
        let (init_val, init_ty) = self.lower_thir_expr_req(&args[0])?;
        let (closure_val, _) = self.lower_thir_expr_req(&args[1])?;
        let _ = span;
        Ok(self.emit_list_fold_loop(base_val, elem_ty, init_val, init_ty, closure_val))
    }

    fn emit_list_any_loop(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        closure_val: ValueId,
    ) -> (ValueId, IrType) {
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let len_val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_val,
                list: base_val,
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
        let acc_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstBool {
                result: acc_init,
                value: false,
            },
            Some(bool_ty.clone()),
        );

        let header_bb = self.builder.create_block(Some("any_header"));
        let body_bb = self.builder.create_block(Some("any_body"));
        let merge_bb = self.builder.create_block(Some("any_merge"));

        let idx_param = self
            .builder
            .add_block_param(header_bb, Some("any_idx"), i64_ty.clone());
        let acc_param = self
            .builder
            .add_block_param(header_bb, Some("any_acc"), bool_ty.clone());
        let _idx_fin = self
            .builder
            .add_block_param(merge_bb, Some("any_idx_fin"), i64_ty.clone());
        let acc_fin = self
            .builder
            .add_block_param(merge_bb, Some("any_acc_fin"), bool_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: vec![idx_init, acc_init],
            },
            None,
        );

        self.builder.set_current_block(header_bb);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx_param,
                rhs: len_val,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body_bb,
                then_args: vec![],
                else_block: merge_bb,
                else_args: vec![idx_param, acc_param],
            },
            None,
        );

        self.builder.set_current_block(body_bb);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: base_val,
                index: idx_param,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::CallClosure {
                result: Some(val),
                closure: closure_val,
                args: vec![elem],
                result_ty: bool_ty.clone(),
                pass_env: true,
            },
            Some(bool_ty.clone()),
        );
        let new_acc = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: new_acc,
                op: BinOp::BitOr,
                lhs: acc_param,
                rhs: val,
                ty: bool_ty.clone(),
            },
            Some(bool_ty.clone()),
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
        let next_idx = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_idx,
                op: BinOp::Add,
                lhs: idx_param,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: vec![next_idx, new_acc],
            },
            None,
        );

        self.builder.set_current_block(merge_bb);
        (acc_fin, bool_ty)
    }

    pub(super) fn lower_list_any(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list.any expects 1 argument (closure)",
                args.len(),
                span,
            ));
        }
        let (closure_val, _) = self.lower_expr(&args[0])?;
        let _ = span;
        Ok(self.emit_list_any_loop(base_val, elem_ty, closure_val))
    }

    pub(super) fn lower_thir_list_any(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        args: &[crate::thir::ThirExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list.any expects 1 argument (closure)",
                args.len(),
                span,
            ));
        }
        let (closure_val, _) = self.lower_thir_expr_req(&args[0])?;
        let _ = span;
        Ok(self.emit_list_any_loop(base_val, elem_ty, closure_val))
    }

    fn emit_list_all_loop(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        closure_val: ValueId,
    ) -> (ValueId, IrType) {
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let len_val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListLen {
                result: len_val,
                list: base_val,
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
        let acc_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstBool {
                result: acc_init,
                value: true,
            },
            Some(bool_ty.clone()),
        );

        let header_bb = self.builder.create_block(Some("all_header"));
        let body_bb = self.builder.create_block(Some("all_body"));
        let merge_bb = self.builder.create_block(Some("all_merge"));

        let idx_param = self
            .builder
            .add_block_param(header_bb, Some("all_idx"), i64_ty.clone());
        let acc_param = self
            .builder
            .add_block_param(header_bb, Some("all_acc"), bool_ty.clone());
        let _idx_fin = self
            .builder
            .add_block_param(merge_bb, Some("all_idx_fin"), i64_ty.clone());
        let acc_fin = self
            .builder
            .add_block_param(merge_bb, Some("all_acc_fin"), bool_ty.clone());

        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: vec![idx_init, acc_init],
            },
            None,
        );

        self.builder.set_current_block(header_bb);
        let cond = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond,
                op: BinOp::CmpLt,
                lhs: idx_param,
                rhs: len_val,
                ty: i64_ty.clone(),
            },
            Some(bool_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body_bb,
                then_args: vec![],
                else_block: merge_bb,
                else_args: vec![idx_param, acc_param],
            },
            None,
        );

        self.builder.set_current_block(body_bb);
        let elem = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ListGet {
                result: elem,
                list: base_val,
                index: idx_param,
                elem_ty: elem_ty.clone(),
            },
            Some(elem_ty.clone()),
        );
        let val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::CallClosure {
                result: Some(val),
                closure: closure_val,
                args: vec![elem],
                result_ty: bool_ty.clone(),
                pass_env: true,
            },
            Some(bool_ty.clone()),
        );
        let new_acc = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: new_acc,
                op: BinOp::BitAnd,
                lhs: acc_param,
                rhs: val,
                ty: bool_ty.clone(),
            },
            Some(bool_ty.clone()),
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
        let next_idx = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: next_idx,
                op: BinOp::Add,
                lhs: idx_param,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: vec![next_idx, new_acc],
            },
            None,
        );

        self.builder.set_current_block(merge_bb);
        (acc_fin, bool_ty)
    }

    pub(super) fn lower_list_all(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list.all expects 1 argument (closure)",
                args.len(),
                span,
            ));
        }
        let (closure_val, _) = self.lower_expr(&args[0])?;
        let _ = span;
        Ok(self.emit_list_all_loop(base_val, elem_ty, closure_val))
    }

    pub(super) fn lower_thir_list_all(
        &mut self,
        base_val: ValueId,
        elem_ty: IrType,
        args: &[crate::thir::ThirExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.len() != 1 {
            return Err(builtin_arity_error(
                "list.all expects 1 argument (closure)",
                args.len(),
                span,
            ));
        }
        let (closure_val, _) = self.lower_thir_expr_req(&args[0])?;
        let _ = span;
        Ok(self.emit_list_all_loop(base_val, elem_ty, closure_val))
    }
}
