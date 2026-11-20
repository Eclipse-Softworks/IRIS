//! CFG Dataflow Borrow Checker and Liveness Analysis (rustc MIR / NLL style).
//!
//! Performs backward dataflow analysis over the control-flow graph of `IrFunction`
//! basic blocks to compute exact value liveness sets (`live_in`, `live_out`, `live_before`,
//! `live_after`). This replaces lexical scope boundaries with true Non-Lexical Lifetimes (NLL),
//! allowing loans to terminate immediately after their last active use.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::diagnostics::{Diagnostic, DiagnosticLevel, DiagnosticSpan};
use crate::error::PassError;
use crate::ir::block::{BlockId, IrBlock};
use crate::ir::function::{IrFunction, PlaceEvent, PlaceEventKind, PlaceSlot, PlaceSpan};
use crate::ir::instr::IrInstr;
use crate::ir::module::IrModule;
use crate::ir::value::ValueId;
use crate::pass::opt::apply_replacements;
use crate::pass::Pass;

/// Computed liveness information for a single basic block in an IR function.
#[derive(Debug, Clone, Default)]
pub struct BlockLiveness {
    /// Values defined in this block (block params + instruction results).
    pub defs: HashSet<ValueId>,
    /// Values used in this block before any definition in this block.
    pub uses: HashSet<ValueId>,
    /// Values live on entry to this block (flowing into the first instruction).
    pub live_in: HashSet<ValueId>,
    /// Values live on exit from this block (flowing out of the terminator).
    pub live_out: HashSet<ValueId>,
}

/// Full dataflow liveness analysis for an `IrFunction`.
#[derive(Debug, Clone)]
pub struct FunctionLiveness {
    /// Liveness per basic block ID.
    pub blocks: HashMap<BlockId, BlockLiveness>,
    /// Predecessors per basic block ID in the CFG.
    pub preds: HashMap<BlockId, Vec<BlockId>>,
    /// Successors per basic block ID in the CFG.
    pub succs: HashMap<BlockId, Vec<BlockId>>,
    /// Map of `ValueId` to the point of its last active use: `(BlockId, instr_index)`.
    pub last_uses: HashMap<ValueId, (BlockId, usize)>,
}

impl FunctionLiveness {
    /// Analyzes an `IrFunction` and computes backward dataflow liveness across all blocks.
    pub fn compute(func: &IrFunction) -> Self {
        let mut preds: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        let mut succs: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        let mut blocks: HashMap<BlockId, BlockLiveness> = HashMap::new();

        // 1. Build CFG edges (predecessors and successors) and initial def/use sets
        for block in &func.blocks {
            let bid = block.id;
            preds.entry(bid).or_default();
            let block_succs = succs.entry(bid).or_default();

            if let Some(term) = block.terminator() {
                match term {
                    IrInstr::Br { target, .. } => {
                        block_succs.push(*target);
                    }
                    IrInstr::CondBr {
                        then_block,
                        else_block,
                        ..
                    } => {
                        block_succs.push(*then_block);
                        block_succs.push(*else_block);
                    }
                    IrInstr::SwitchVariant {
                        arms,
                        default_block,
                        ..
                    } => {
                        for (_, target) in arms {
                            block_succs.push(*target);
                        }
                        if let Some(def) = default_block {
                            block_succs.push(*def);
                        }
                    }
                    IrInstr::Return { .. } | IrInstr::Panic { .. } => {}
                    _ => {}
                }
            }

            // Record defs and uses
            let mut block_liveness = BlockLiveness::default();
            for p in &block.params {
                block_liveness.defs.insert(p.id);
            }

            for instr in &block.instrs {
                for op in instr.operands() {
                    if !block_liveness.defs.contains(&op) {
                        block_liveness.uses.insert(op);
                    }
                }
                if let Some(r) = instr.result() {
                    block_liveness.defs.insert(r);
                }
            }

            blocks.insert(bid, block_liveness);
        }

        // Fill in predecessor edges from successors
        for (&from, targets) in &succs {
            for &to in targets {
                preds.entry(to).or_default().push(from);
            }
        }

        // 2. Fixed-point backward dataflow iteration for LiveIn and LiveOut:
        //    LiveOut(B) = union_{S in Succ(B)} LiveIn(S)
        //    LiveIn(B)  = Use(B) union (LiveOut(B) \ Def(B))
        let mut worklist: VecDeque<BlockId> = func.blocks.iter().map(|b| b.id).collect();
        let mut in_worklist: HashSet<BlockId> = worklist.iter().copied().collect();

        while let Some(bid) = worklist.pop_front() {
            in_worklist.remove(&bid);

            let block_succ_ids = succs.get(&bid).cloned().unwrap_or_default();
            let mut new_live_out = HashSet::new();
            for s in block_succ_ids {
                if let Some(s_info) = blocks.get(&s) {
                    new_live_out.extend(s_info.live_in.iter().copied());
                }
            }

            let info = blocks.get_mut(&bid).unwrap();
            info.live_out = new_live_out;

            let mut new_live_in = info.uses.clone();
            for &val in &info.live_out {
                if !info.defs.contains(&val) {
                    new_live_in.insert(val);
                }
            }

            if new_live_in != info.live_in {
                info.live_in = new_live_in;
                // Predecessors must be re-evaluated
                if let Some(p_list) = preds.get(&bid) {
                    for &p in p_list {
                        if in_worklist.insert(p) {
                            worklist.push_back(p);
                        }
                    }
                }
            }
        }

        // 3. Compute exact last use points for all values across instructions
        let mut last_uses = HashMap::new();
        for block in &func.blocks {
            let bid = block.id;
            let mut current_live = blocks
                .get(&bid)
                .map(|b| b.live_out.clone())
                .unwrap_or_default();

            for (idx, instr) in block.instrs.iter().enumerate().rev() {
                for op in instr.operands() {
                    if !current_live.contains(&op) {
                        // This instruction is the last use of `op` along this path
                        last_uses.entry(op).or_insert((bid, idx));
                        current_live.insert(op);
                    }
                }
                if let Some(res) = instr.result() {
                    current_live.remove(&res);
                }
            }
        }

        Self {
            blocks,
            preds,
            succs,
            last_uses,
        }
    }

    /// Returns `true` if `val` is live at exit from block `bid`.
    pub fn is_live_out(&self, bid: BlockId, val: ValueId) -> bool {
        self.blocks
            .get(&bid)
            .map(|b| b.live_out.contains(&val))
            .unwrap_or(false)
    }

    /// Returns `true` if `val` is live at entry to block `bid`.
    pub fn is_live_in(&self, bid: BlockId, val: ValueId) -> bool {
        self.blocks
            .get(&bid)
            .map(|b| b.live_in.contains(&val))
            .unwrap_or(false)
    }

    /// Returns the location of the last active use of `val`, if known.
    pub fn last_use_of(&self, val: ValueId) -> Option<(BlockId, usize)> {
        self.last_uses.get(&val).copied()
    }
}

/// CFG-based borrow checker pass.
///
/// Runs dataflow liveness analysis and verifies that no active borrows conflict
/// with mutations or moves across basic block boundaries.
pub struct CfgBorrowCheckerPass;

impl Pass for CfgBorrowCheckerPass {
    fn name(&self) -> &'static str {
        "cfg_borrow_checker"
    }

    fn run(&mut self, module: &mut IrModule) -> Result<(), PassError> {
        for func in &module.functions {
            let liveness = FunctionLiveness::compute(func);
            Self::check_use_before_def(func, &liveness)?;
            Self::check_nll_borrows_and_moves(func, &liveness)?;
        }
        for func in &mut module.functions {
            Self::strip_borrow_move_markers(func);
        }
        Ok(())
    }
}

/// Root storage identity for borrow and move tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum PlaceRoot {
    /// Lowered variable binding slot from `PlaceMeta`.
    Slot(PlaceSlot),
    /// Direct SSA value (used for hand-constructed IR without `PlaceMeta`).
    Value(ValueId),
}

/// A root storage location plus field/tuple projection path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Place {
    root: PlaceRoot,
    projections: Vec<usize>,
}

impl Place {
    fn whole(root: PlaceRoot) -> Self {
        Self {
            root,
            projections: Vec::new(),
        }
    }

    /// Two places overlap if they share a root and either projection path is a
    /// prefix of the other (so disjoint struct/tuple fields do not conflict).
    fn overlaps(&self, other: &Self) -> bool {
        if self.root != other.root {
            return false;
        }
        let shared = self.projections.len().min(other.projections.len());
        self.projections[..shared] == other.projections[..shared]
    }
}

/// Shared vs mutable borrow classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BorrowKind {
    Shared,
    Mutable,
}

/// Concrete borrow site recorded in a function.
#[derive(Debug, Clone)]
struct Loan {
    id: usize,
    holder: ValueId,
    place: Place,
    kind: BorrowKind,
    span: Option<PlaceSpan>,
}

/// Initialization/move state of a `PlaceRoot` at a program point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoveStatus {
    Moved(PlaceSpan),
    MaybeMoved(PlaceSpan),
}

impl MoveStatus {
    fn span(self) -> PlaceSpan {
        match self {
            MoveStatus::Moved(s) | MoveStatus::MaybeMoved(s) => s,
        }
    }
}

/// Forward dataflow state tracked across CFG edges and instructions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct NllState {
    /// Moved-out places. Keys are full projection paths, so moving `p.a` does
    /// not mark `p.b` as moved. A key with no projections means the whole root.
    moved: HashMap<Place, MoveStatus>,
    active_loans: BTreeSet<usize>,
}

impl NllState {
    /// Status of a place if any moved-out place overlaps it. Overlap is what
    /// makes a partial move invalidate a read of the whole record, or a move
    /// of a whole record invalidate a read of one of its fields.
    fn moved_status(&self, place: &Place) -> Option<MoveStatus> {
        self.moved
            .iter()
            .find(|(moved, _)| moved.overlaps(place))
            .map(|(_, status)| *status)
    }

    /// Restores a place after reassignment. Writing a whole binding clears
    /// every moved field of it; writing `p.a` clears only that field and its
    /// descendants.
    fn clear_moved(&mut self, place: &Place) {
        self.moved.retain(|moved, _| {
            moved.root != place.root
                || !(place.projections.is_empty()
                    || moved.projections.starts_with(&place.projections))
        });
    }

    fn join_from(&mut self, incoming: &Self) -> bool {
        let mut changed = false;
        for &loan_id in &incoming.active_loans {
            if self.active_loans.insert(loan_id) {
                changed = true;
            }
        }

        // Merge moved places from both predecessors. Overlapping places are
        // represented by the more general one (shorter projection path), and
        // a place moved on only one path becomes MaybeMoved.
        let mut combined: Vec<Place> = self.moved.keys().cloned().collect();
        for place in incoming.moved.keys() {
            if !combined.iter().any(|p| p.overlaps(place)) {
                combined.push(place.clone());
            }
        }
        combined.sort_by(|a, b| {
            a.root
                .cmp(&b.root)
                .then_with(|| a.projections.len().cmp(&b.projections.len()))
                .then_with(|| a.projections.cmp(&b.projections))
        });
        let next: HashMap<Place, MoveStatus> = combined
            .into_iter()
            .filter_map(|place| {
                let a = self.moved_status(&place);
                let b = incoming.moved_status(&place);
                let merged = match (a, b) {
                    (Some(MoveStatus::Moved(x)), Some(MoveStatus::Moved(y))) => {
                        Some(MoveStatus::Moved(earlier_span(x, y)))
                    }
                    (Some(a), Some(b)) => {
                        Some(MoveStatus::MaybeMoved(earlier_span(a.span(), b.span())))
                    }
                    (Some(a), None) | (None, Some(a)) => Some(MoveStatus::MaybeMoved(a.span())),
                    (None, None) => None,
                };
                merged.map(|status| (place, status))
            })
            .collect();
        if next != self.moved {
            self.moved = next;
            changed = true;
        }
        changed
    }
}

/// Per-block instruction-boundary liveness computed backwards from `live_out`.
struct BlockLivenessPoints {
    live_at_entry: HashSet<ValueId>,
    live_before_instr: Vec<HashSet<ValueId>>,
    live_at_end: HashSet<ValueId>,
}

/// Precomputed CFG, liveness, loan provenance, and block index for one function.
struct NllAnalysis<'a> {
    func: &'a IrFunction,
    liveness: &'a FunctionLiveness,
    has_place_meta: bool,
    block_map: HashMap<BlockId, &'a IrBlock>,
    rpo_order: Vec<BlockId>,
    rpo_pos: HashMap<BlockId, usize>,
    preds: HashMap<BlockId, Vec<BlockId>>,
    liveness_pts: HashMap<BlockId, BlockLivenessPoints>,
    def_map: HashMap<ValueId, (BlockId, usize)>,
    loans: Vec<Loan>,
    loan_by_holder: HashMap<ValueId, usize>,
    loans_of: HashMap<ValueId, BTreeSet<usize>>,
}

impl CfgBorrowCheckerPass {
    /// Verifies that every operand used in a block is defined or live-in.
    fn check_use_before_def(
        func: &IrFunction,
        liveness: &FunctionLiveness,
    ) -> Result<(), PassError> {
        for block in &func.blocks {
            let mut active_defs: HashSet<ValueId> = block.params.iter().map(|p| p.id).collect();
            if let Some(b_info) = liveness.blocks.get(&block.id) {
                active_defs.extend(b_info.live_in.iter().copied());
            }

            for (ii, instr) in block.instrs.iter().enumerate() {
                for op in instr.operands() {
                    if !active_defs.contains(&op) {
                        return Err(Self::use_before_def(func, block.id, ii, instr, op));
                    }
                }
                if let Some(r) = instr.result() {
                    active_defs.insert(r);
                }
            }
        }
        Ok(())
    }

    /// Runs full IR-level NLL borrow, move, mutation, and cross-arm checking.
    fn check_nll_borrows_and_moves(
        func: &IrFunction,
        liveness: &FunctionLiveness,
    ) -> Result<(), PassError> {
        if func.blocks.is_empty() {
            return Ok(());
        }
        let has_borrows_or_moves = func.blocks.iter().any(|b| {
            b.instrs
                .iter()
                .any(|i| matches!(i, IrInstr::Borrow { .. } | IrInstr::Move { .. }))
        });
        if !has_borrows_or_moves {
            return Ok(());
        }

        let analysis = Self::build_analysis(func, liveness);
        Self::check_cross_arm_block_params(&analysis)?;
        let converged_in = Self::solve_forward_dataflow(&analysis);
        Self::verify_blocks(&analysis, &converged_in)
    }

    /// Builds CFG topology, per-instruction liveness, loans, and provenance.
    fn build_analysis<'a>(func: &'a IrFunction, liveness: &'a FunctionLiveness) -> NllAnalysis<'a> {
        let block_map: HashMap<BlockId, &'a IrBlock> =
            func.blocks.iter().map(|b| (b.id, b)).collect();
        let (rpo_order, rpo_pos, preds) = compute_cfg_rpo(func, &block_map);

        let mut liveness_pts = HashMap::new();
        let mut def_map = HashMap::new();
        for &bid in &rpo_order {
            let block = block_map[&bid];
            for (ii, instr) in block.instrs.iter().enumerate() {
                if let Some(res) = instr.result() {
                    def_map.insert(res, (bid, ii));
                }
            }
            liveness_pts.insert(bid, compute_block_liveness_points(block, liveness));
        }

        let mut loans = Vec::new();
        let mut loan_by_holder = HashMap::new();
        for &bid in &rpo_order {
            let block = block_map[&bid];
            for (ii, instr) in block.instrs.iter().enumerate() {
                if let IrInstr::Borrow {
                    result,
                    place,
                    is_mut,
                } = instr
                {
                    let (resolved_place, span) =
                        resolve_borrow_place(func, &def_map, bid, ii, *result, *place);
                    let id = loans.len();
                    let kind = if *is_mut {
                        BorrowKind::Mutable
                    } else {
                        BorrowKind::Shared
                    };
                    loans.push(Loan {
                        id,
                        holder: *result,
                        place: resolved_place,
                        kind,
                        span,
                    });
                    loan_by_holder.insert(*result, id);
                }
            }
        }

        let loans_of = compute_loan_provenance(&rpo_order, &block_map, &loans);
        let has_place_meta =
            !func.place_meta.events.is_empty() || !func.place_meta.slot_names.is_empty();

        NllAnalysis {
            func,
            liveness,
            has_place_meta,
            block_map,
            rpo_order,
            rpo_pos,
            preds,
            liveness_pts,
            def_map,
            loans,
            loan_by_holder,
            loans_of,
        }
    }

    /// Rejects incompatible shared vs mutable loans flowing across sibling
    /// forward CFG arms into the same `BlockParam` (e.g. `if c { &x } else { &mut x }`).
    fn check_cross_arm_block_params(analysis: &NllAnalysis<'_>) -> Result<(), PassError> {
        for &succ_id in &analysis.rpo_order {
            let succ_block = analysis.block_map[&succ_id];
            if succ_block.params.is_empty() {
                continue;
            }
            let succ_pos = analysis.rpo_pos[&succ_id];
            let Some(pred_ids) = analysis.preds.get(&succ_id) else {
                continue;
            };

            let mut forward_edges: Vec<&[ValueId]> = Vec::new();
            for &pred_id in pred_ids {
                if analysis
                    .rpo_pos
                    .get(&pred_id)
                    .copied()
                    .unwrap_or(usize::MAX)
                    >= succ_pos
                {
                    continue;
                }
                let pred_block = analysis.block_map[&pred_id];
                for (edge_target, edge_args) in outgoing_edges(pred_block) {
                    if edge_target == succ_id {
                        forward_edges.push(edge_args);
                    }
                }
            }
            if forward_edges.len() < 2 {
                continue;
            }

            for param_idx in 0..succ_block.params.len() {
                for e1 in 0..forward_edges.len() {
                    let Some(&arg1) = forward_edges[e1].get(param_idx) else {
                        continue;
                    };
                    let Some(set1) = analysis.loans_of.get(&arg1) else {
                        continue;
                    };
                    for edge2 in forward_edges.iter().skip(e1 + 1) {
                        let Some(&arg2) = edge2.get(param_idx) else {
                            continue;
                        };
                        let Some(set2) = analysis.loans_of.get(&arg2) else {
                            continue;
                        };
                        for &l1 in set1 {
                            for &l2 in set2 {
                                let loan1 = &analysis.loans[l1];
                                let loan2 = &analysis.loans[l2];
                                if loan1.id != loan2.id
                                    && loan1.kind != loan2.kind
                                    && loan1.place.overlaps(&loan2.place)
                                {
                                    return Err(diag_incompatible_branch_borrows(
                                        analysis.func,
                                        loan1,
                                        loan2,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Computes converged entry `NllState` for every reachable basic block.
    fn solve_forward_dataflow(analysis: &NllAnalysis<'_>) -> HashMap<BlockId, NllState> {
        let mut state_in: HashMap<BlockId, NllState> = HashMap::new();
        let entry = analysis.rpo_order[0];
        state_in.insert(entry, NllState::default());

        let mut worklist: VecDeque<BlockId> = analysis.rpo_order.iter().copied().collect();
        let mut in_queue: HashSet<BlockId> = analysis.rpo_order.iter().copied().collect();

        while let Some(bid) = worklist.pop_front() {
            in_queue.remove(&bid);
            let Some(mut state) = state_in.get(&bid).cloned() else {
                continue;
            };
            let block = analysis.block_map[&bid];
            simulate_block_transfer(analysis, block, &mut state);

            for (succ_id, edge_args) in outgoing_edges(block) {
                if !analysis.block_map.contains_key(&succ_id) {
                    continue;
                }
                let mut edge_state = state.clone();
                prune_loans_across_edge(analysis, succ_id, edge_args, &mut edge_state.active_loans);

                let changed = match state_in.get_mut(&succ_id) {
                    Some(existing) => existing.join_from(&edge_state),
                    None => {
                        state_in.insert(succ_id, edge_state);
                        true
                    }
                };
                if changed && in_queue.insert(succ_id) {
                    worklist.push_back(succ_id);
                }
            }
        }
        state_in
    }

    /// Walks each reachable block using converged entry state and emits the
    /// first borrow/move/mutation diagnostic encountered.
    fn verify_blocks(
        analysis: &NllAnalysis<'_>,
        converged_in: &HashMap<BlockId, NllState>,
    ) -> Result<(), PassError> {
        for &bid in &analysis.rpo_order {
            let Some(mut state) = converged_in.get(&bid).cloned() else {
                continue;
            };
            let block = analysis.block_map[&bid];
            check_block_instructions(analysis, block, &mut state)?;
        }
        Ok(())
    }

    /// Replaces `IrInstr::Borrow` and `IrInstr::Move` results with their root
    /// places and strips the marker instructions once checking succeeds.
    fn strip_borrow_move_markers(func: &mut IrFunction) {
        let mut direct_reps: HashMap<ValueId, ValueId> = HashMap::new();
        for block in &func.blocks {
            for instr in &block.instrs {
                if let IrInstr::Borrow { result, place, .. } | IrInstr::Move { result, place } =
                    instr
                {
                    direct_reps.insert(*result, *place);
                }
            }
        }
        if !direct_reps.is_empty() {
            // Resolve chains like Move(Borrow(x)) down to x.
            let mut resolved_reps: HashMap<ValueId, ValueId> =
                HashMap::with_capacity(direct_reps.len());
            for &k in direct_reps.keys() {
                let mut cur = k;
                let mut steps = 0usize;
                while let Some(&next) = direct_reps.get(&cur) {
                    if next == cur || steps > direct_reps.len() {
                        break;
                    }
                    cur = next;
                    steps += 1;
                }
                resolved_reps.insert(k, cur);
            }

            for block in &mut func.blocks {
                block
                    .instrs
                    .retain(|i| !matches!(i, IrInstr::Borrow { .. } | IrInstr::Move { .. }));
                for instr in &mut block.instrs {
                    apply_replacements(instr, &resolved_reps);
                }
            }
        }

        // Mixed-branch compatibility checking (`if c { &x } else { &mut y }`) can
        // leave `MutRef(T)` branch arguments targeting a non-`MutRef` block
        // parameter so `check_cross_arm_block_params` sees both loans. After
        // borrow checking passes, materialize a `CellLoad` before the terminator.
        let block_param_tys: HashMap<BlockId, Vec<crate::ir::types::IrType>> = func
            .blocks
            .iter()
            .map(|b| (b.id, b.params.iter().map(|p| p.ty.clone()).collect()))
            .collect();
        let num_blocks = func.blocks.len();
        for bi in 0..num_blocks {
            let block_id = func.blocks[bi].id;
            let Some(term_idx) = func.blocks[bi].instrs.len().checked_sub(1) else {
                continue;
            };
            let mut term = match func.blocks[bi].instrs.get(term_idx) {
                Some(t @ (IrInstr::Br { .. } | IrInstr::CondBr { .. })) => t.clone(),
                _ => continue,
            };

            let mut loads: Vec<(ValueId, crate::ir::types::IrType, IrInstr)> = Vec::new();
            let mut loaded_cache: HashMap<ValueId, ValueId> = HashMap::new();
            let mut coerce_edges = |edges: &mut [(BlockId, &mut Vec<ValueId>)]| {
                for (target, args) in edges.iter_mut() {
                    let Some(param_tys) = block_param_tys.get(target) else {
                        continue;
                    };
                    for (arg, param_ty) in args.iter_mut().zip(param_tys.iter()) {
                        if matches!(param_ty, crate::ir::types::IrType::MutRef(_)) {
                            continue;
                        }
                        let Some(crate::ir::types::IrType::MutRef(inner)) =
                            func.value_types.get(arg).cloned()
                        else {
                            continue;
                        };
                        let loaded = if let Some(&existing) = loaded_cache.get(arg) {
                            existing
                        } else {
                            let loaded_id = func.fresh_value();
                            loads.push((
                                loaded_id,
                                (*inner).clone(),
                                IrInstr::CellLoad {
                                    result: loaded_id,
                                    cell: *arg,
                                    result_ty: (*inner).clone(),
                                },
                            ));
                            loaded_cache.insert(*arg, loaded_id);
                            loaded_id
                        };
                        *arg = loaded;
                    }
                }
            };

            match &mut term {
                IrInstr::Br { target, args } => {
                    coerce_edges(&mut [(*target, args)]);
                }
                IrInstr::CondBr {
                    then_block,
                    then_args,
                    else_block,
                    else_args,
                    ..
                } => {
                    coerce_edges(&mut [(*then_block, then_args), (*else_block, else_args)]);
                }
                _ => {}
            }

            if loads.is_empty() {
                continue;
            }

            let mut instrs_to_insert = Vec::with_capacity(loads.len());
            for (offset, (loaded_id, inner_ty, instr)) in loads.into_iter().enumerate() {
                let tid = func.intern_type(inner_ty.clone());
                func.value_types.insert(loaded_id, inner_ty);
                func.value_type_ids.insert(loaded_id, tid);
                func.value_defs.insert(
                    loaded_id,
                    crate::ir::value::ValueDef::InstrResult {
                        block: block_id,
                        instr: crate::ir::instr::InstrId((term_idx + offset) as u32),
                    },
                );
                instrs_to_insert.push(instr);
            }
            let block = &mut func.blocks[bi];
            block.instrs[term_idx] = term;
            block.instrs.splice(term_idx..term_idx, instrs_to_insert);
        }
    }

    /// A `use before def` at the instruction that trips it, pointed at the
    /// instruction's own span when the span table has one (the borrow
    /// checker's answer needs a location to be actionable).
    fn use_before_def(
        func: &IrFunction,
        block: BlockId,
        ii: usize,
        instr: &IrInstr,
        op: ValueId,
    ) -> PassError {
        let byte = func
            .get_instr_span(block.0, ii)
            .or_else(|| {
                instr
                    .operands()
                    .iter()
                    .find_map(|v| func.span_table.get_for_val(*v))
            })
            .or_else(|| func.debug_span(block.0, ii));
        let mut diag = Diagnostic::error(format!(
            "in function '{}': variable '{}' is used before it has been assigned a value",
            func.name, op
        ));
        if let Some(b) = byte {
            diag = diag.with_primary_span(b, b + 1, None);
        }
        PassError::rich("E0202", diag)
    }
}

type CfgRpoResult = (
    Vec<BlockId>,
    HashMap<BlockId, usize>,
    HashMap<BlockId, Vec<BlockId>>,
);

/// Computes reachable CFG reverse postorder and predecessor lists.
fn compute_cfg_rpo(func: &IrFunction, block_map: &HashMap<BlockId, &IrBlock>) -> CfgRpoResult {
    let mut visited = HashSet::new();
    let mut postorder = Vec::new();
    let mut preds: HashMap<BlockId, Vec<BlockId>> = HashMap::new();

    fn dfs(
        bid: BlockId,
        block_map: &HashMap<BlockId, &IrBlock>,
        visited: &mut HashSet<BlockId>,
        postorder: &mut Vec<BlockId>,
        preds: &mut HashMap<BlockId, Vec<BlockId>>,
    ) {
        if !visited.insert(bid) {
            return;
        }
        if let Some(block) = block_map.get(&bid) {
            for (succ, _) in outgoing_edges(block) {
                if block_map.contains_key(&succ) {
                    preds.entry(succ).or_default().push(bid);
                    dfs(succ, block_map, visited, postorder, preds);
                }
            }
        }
        postorder.push(bid);
    }

    dfs(
        func.blocks[0].id,
        block_map,
        &mut visited,
        &mut postorder,
        &mut preds,
    );
    postorder.reverse();
    let rpo_pos = postorder
        .iter()
        .enumerate()
        .map(|(idx, &bid)| (bid, idx))
        .collect();
    (postorder, rpo_pos, preds)
}

/// Computes per-instruction `live_before` sets backwards from `live_out`.
fn compute_block_liveness_points(
    block: &IrBlock,
    liveness: &FunctionLiveness,
) -> BlockLivenessPoints {
    let live_at_end: HashSet<ValueId> = liveness
        .blocks
        .get(&block.id)
        .map(|b| b.live_out.clone())
        .unwrap_or_default();
    let mut live = live_at_end.clone();
    let mut live_before_instr = vec![HashSet::new(); block.instrs.len()];
    for ii in (0..block.instrs.len()).rev() {
        if let Some(res) = block.instrs[ii].result() {
            live.remove(&res);
        }
        for op in block.instrs[ii].operands() {
            live.insert(op);
        }
        live_before_instr[ii] = live.clone();
    }
    BlockLivenessPoints {
        live_at_entry: live,
        live_before_instr,
        live_at_end,
    }
}

/// Propagates `LoanId` provenance through `Move` instructions and CFG block parameters.
fn compute_loan_provenance(
    rpo_order: &[BlockId],
    block_map: &HashMap<BlockId, &IrBlock>,
    loans: &[Loan],
) -> HashMap<ValueId, BTreeSet<usize>> {
    let mut loans_of: HashMap<ValueId, BTreeSet<usize>> = HashMap::new();
    for loan in loans {
        loans_of.entry(loan.holder).or_default().insert(loan.id);
    }

    let mut changed = true;
    while changed {
        changed = false;
        for &bid in rpo_order {
            let block = block_map[&bid];
            for instr in &block.instrs {
                if let IrInstr::Move { result, place } = instr {
                    if let Some(src_loans) = loans_of.get(place).cloned() {
                        let dst = loans_of.entry(*result).or_default();
                        for lid in src_loans {
                            if dst.insert(lid) {
                                changed = true;
                            }
                        }
                    }
                }
            }
            for (succ_id, edge_args) in outgoing_edges(block) {
                let Some(succ_block) = block_map.get(&succ_id) else {
                    continue;
                };
                for (param, arg) in succ_block.params.iter().zip(edge_args.iter()) {
                    if let Some(arg_loans) = loans_of.get(arg).cloned() {
                        let dst = loans_of.entry(param.id).or_default();
                        for lid in arg_loans {
                            if dst.insert(lid) {
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
    }
    loans_of
}

/// Returns outgoing `(target_block, edge_args)` pairs for a block's terminator.
fn outgoing_edges(block: &IrBlock) -> Vec<(BlockId, &[ValueId])> {
    let Some(term) = block.instrs.last() else {
        return Vec::new();
    };
    match term {
        IrInstr::Br { target, args } => vec![(*target, args.as_slice())],
        IrInstr::CondBr {
            then_block,
            then_args,
            else_block,
            else_args,
            ..
        } => vec![
            (*then_block, then_args.as_slice()),
            (*else_block, else_args.as_slice()),
        ],
        IrInstr::SwitchVariant {
            arms,
            default_block,
            ..
        } => {
            let mut edges: Vec<(BlockId, &[ValueId])> =
                arms.iter().map(|(_, bb)| (*bb, &[][..])).collect();
            if let Some(def) = default_block {
                edges.push((*def, &[][..]));
            }
            edges
        }
        _ => Vec::new(),
    }
}

/// Keeps only active loans whose holder (or a derived alias) is live in `live_vals`.
fn prune_loans_by_live(
    analysis: &NllAnalysis<'_>,
    live_vals: &HashSet<ValueId>,
    active_loans: &mut BTreeSet<usize>,
) {
    active_loans.retain(|&loan_id| {
        live_vals.iter().any(|val| {
            analysis
                .loans_of
                .get(val)
                .is_some_and(|set| set.contains(&loan_id))
        })
    });
}

/// Filters active loans across a CFG edge `pred -> succ(edge_args)` using
/// both `live_in[succ]` and any live `succ.params` fed by `edge_args`.
fn prune_loans_across_edge(
    analysis: &NllAnalysis<'_>,
    succ_id: BlockId,
    edge_args: &[ValueId],
    active_loans: &mut BTreeSet<usize>,
) {
    let mut live_on_edge: HashSet<ValueId> = analysis
        .liveness
        .blocks
        .get(&succ_id)
        .map(|b| b.live_in.clone())
        .unwrap_or_default();

    if let (Some(succ_block), Some(succ_pts)) = (
        analysis.block_map.get(&succ_id),
        analysis.liveness_pts.get(&succ_id),
    ) {
        for (param, arg) in succ_block.params.iter().zip(edge_args.iter()) {
            if succ_pts.live_at_entry.contains(&param.id) {
                live_on_edge.insert(*arg);
            }
        }
    }
    prune_loans_by_live(analysis, &live_on_edge, active_loans);
}

/// Advances `NllState` through one block without emitting errors (for fixed-point).
fn simulate_block_transfer(analysis: &NllAnalysis<'_>, block: &IrBlock, state: &mut NllState) {
    let pts = &analysis.liveness_pts[&block.id];
    for (ii, instr) in block.instrs.iter().enumerate() {
        prune_loans_by_live(
            analysis,
            &pts.live_before_instr[ii],
            &mut state.active_loans,
        );

        if let Some(events) = analysis.func.place_meta.events.get(&(block.id.0, ii)) {
            for event in events {
                if let PlaceEventKind::Write { slot, field_index } = &event.kind {
                    state.clear_moved(&Place {
                        root: PlaceRoot::Slot(*slot),
                        projections: field_index.iter().copied().collect(),
                    });
                }
            }
        }

        match instr {
            IrInstr::Borrow { result, .. } => {
                if let Some(&loan_id) = analysis.loan_by_holder.get(result) {
                    state.active_loans.insert(loan_id);
                }
            }
            IrInstr::Move { place, .. } => {
                let (target, span) = resolve_move_target(analysis, block.id, ii, *place);
                state.moved.insert(target, MoveStatus::Moved(span));
            }
            _ => {}
        }
    }

    prune_loans_by_live(analysis, &pts.live_at_end, &mut state.active_loans);
    if let Some(events) = analysis
        .func
        .place_meta
        .events
        .get(&(block.id.0, block.instrs.len()))
    {
        for event in events {
            if let PlaceEventKind::Write { slot, field_index } = &event.kind {
                state.clear_moved(&Place {
                    root: PlaceRoot::Slot(*slot),
                    projections: field_index.iter().copied().collect(),
                });
            }
        }
    }
}

/// Checks every instruction and boundary event in `block` against converged `NllState`.
fn check_block_instructions(
    analysis: &NllAnalysis<'_>,
    block: &IrBlock,
    state: &mut NllState,
) -> Result<(), PassError> {
    let pts = &analysis.liveness_pts[&block.id];
    for (ii, instr) in block.instrs.iter().enumerate() {
        prune_loans_by_live(
            analysis,
            &pts.live_before_instr[ii],
            &mut state.active_loans,
        );

        if let Some(events) = analysis.func.place_meta.events.get(&(block.id.0, ii)) {
            process_boundary_events(analysis, instr, events, state)?;
        }

        match instr {
            IrInstr::Borrow { result, .. } => {
                if let Some(&loan_id) = analysis.loan_by_holder.get(result) {
                    check_borrow_instr(analysis, loan_id, state)?;
                    state.active_loans.insert(loan_id);
                }
            }
            IrInstr::Move { place, .. } => {
                let (target, span) = resolve_move_target(analysis, block.id, ii, *place);
                check_move_instr(analysis, &target, span, state)?;
                state.moved.insert(target, MoveStatus::Moved(span));
            }
            _ => {
                if let Some(target_val) = mutating_container_operand(instr) {
                    check_container_mutation(analysis, block.id, ii, target_val, state)?;
                }
                if !analysis.has_place_meta {
                    check_raw_operand_moves(analysis, block.id, ii, instr, state)?;
                }
            }
        }
    }

    prune_loans_by_live(analysis, &pts.live_at_end, &mut state.active_loans);
    if let Some(events) = analysis
        .func
        .place_meta
        .events
        .get(&(block.id.0, block.instrs.len()))
    {
        for event in events {
            check_single_event(analysis, event, None, state)?;
        }
    }
    Ok(())
}

/// Applies all `PlaceEvent`s recorded immediately before instruction `ii`.
fn process_boundary_events(
    analysis: &NllAnalysis<'_>,
    instr: &IrInstr,
    events: &[PlaceEvent],
    state: &mut NllState,
) -> Result<(), PassError> {
    let owned_slot = match instr {
        IrInstr::Move { .. } => events.iter().find_map(|e| match &e.kind {
            PlaceEventKind::Move { slot, .. } => Some(*slot),
            _ => None,
        }),
        IrInstr::Borrow { .. } => events.iter().find_map(|e| match &e.kind {
            PlaceEventKind::Borrow { slot, .. } => Some(*slot),
            _ => None,
        }),
        _ => None,
    };
    for event in events {
        check_single_event(analysis, event, owned_slot, state)?;
    }
    Ok(())
}

fn check_single_event(
    analysis: &NllAnalysis<'_>,
    event: &PlaceEvent,
    skip_read_slot: Option<PlaceSlot>,
    state: &mut NllState,
) -> Result<(), PassError> {
    match &event.kind {
        PlaceEventKind::Read { slot, projections } => {
            if skip_read_slot == Some(*slot) {
                return Ok(());
            }
            let root = PlaceRoot::Slot(*slot);
            let read_place = Place {
                root,
                projections: projections.clone(),
            };
            if let Some(status) = state.moved_status(&read_place) {
                return Err(diag_use_after_move(
                    analysis.func,
                    root,
                    status.span(),
                    event.span,
                ));
            }
            for &loan_id in &state.active_loans {
                let loan = &analysis.loans[loan_id];
                if loan.kind == BorrowKind::Mutable && read_place.overlaps(&loan.place) {
                    return Err(diag_use_while_mut_borrowed(
                        analysis.func,
                        root,
                        loan,
                        event.span,
                    ));
                }
            }
        }
        PlaceEventKind::Write { slot, field_index } => {
            let root = PlaceRoot::Slot(*slot);
            let write_place = Place {
                root,
                projections: field_index.iter().copied().collect(),
            };
            for &loan_id in &state.active_loans {
                let loan = &analysis.loans[loan_id];
                if write_place.overlaps(&loan.place) {
                    return Err(diag_mutate_while_borrowed(
                        analysis.func,
                        root,
                        loan,
                        Some(event.span),
                    ));
                }
            }
            // Reassigning the whole binding is always allowed and restores
            // initialization. Writing one field of a moved binding is not.
            if field_index.is_some() {
                if let Some(status) = state.moved_status(&write_place) {
                    return Err(diag_use_after_move(
                        analysis.func,
                        root,
                        status.span(),
                        event.span,
                    ));
                }
            }
            state.clear_moved(&write_place);
        }
        PlaceEventKind::Borrow { .. } | PlaceEventKind::Move { .. } => {}
    }
    Ok(())
}

fn check_borrow_instr(
    analysis: &NllAnalysis<'_>,
    loan_id: usize,
    state: &NllState,
) -> Result<(), PassError> {
    let loan = &analysis.loans[loan_id];
    if let Some(status) = state.moved_status(&loan.place) {
        return Err(diag_borrow_after_move(
            analysis.func,
            loan.place.root,
            status.span(),
            loan.span,
        ));
    }
    for &active_id in &state.active_loans {
        let existing = &analysis.loans[active_id];
        if !loan.place.overlaps(&existing.place) {
            continue;
        }
        if loan.kind == BorrowKind::Mutable {
            return Err(diag_mut_borrow_conflict(analysis.func, existing, loan));
        }
        if existing.kind == BorrowKind::Mutable {
            return Err(diag_borrow_while_mut_borrowed(
                analysis.func,
                existing,
                loan,
            ));
        }
    }
    Ok(())
}

fn check_move_instr(
    analysis: &NllAnalysis<'_>,
    target: &Place,
    move_span: PlaceSpan,
    state: &NllState,
) -> Result<(), PassError> {
    if let Some(status) = state.moved_status(target) {
        return Err(diag_move_after_move(
            analysis.func,
            target.root,
            status.span(),
            move_span,
        ));
    }
    for &active_id in &state.active_loans {
        let existing = &analysis.loans[active_id];
        if existing.place.overlaps(target) {
            return Err(diag_move_while_borrowed(
                analysis.func,
                target.root,
                existing,
                Some(move_span),
            ));
        }
    }
    Ok(())
}

fn check_container_mutation(
    analysis: &NllAnalysis<'_>,
    bid: BlockId,
    ii: usize,
    target_val: ValueId,
    state: &NllState,
) -> Result<(), PassError> {
    let target_place = resolve_place_from_val(analysis.func, &analysis.def_map, target_val);
    let holder_loans = analysis.loans_of.get(&target_val);
    for &active_id in &state.active_loans {
        let existing = &analysis.loans[active_id];
        // Mutating directly through an `&mut` reference holder is valid.
        if existing.kind == BorrowKind::Mutable
            && holder_loans.is_some_and(|set| set.contains(&active_id))
        {
            continue;
        }
        if target_place.overlaps(&existing.place) {
            let mut_span = value_or_instr_span(analysis.func, bid, ii, target_val);
            return Err(diag_mutate_while_borrowed(
                analysis.func,
                target_place.root,
                existing,
                mut_span,
            ));
        }
    }
    Ok(())
}

fn check_raw_operand_moves(
    analysis: &NllAnalysis<'_>,
    bid: BlockId,
    ii: usize,
    instr: &IrInstr,
    state: &NllState,
) -> Result<(), PassError> {
    for op in instr.operands() {
        let root = PlaceRoot::Value(op);
        if let Some(status) = state.moved_status(&Place::whole(root)) {
            let use_span = value_or_instr_span(analysis.func, bid, ii, op).unwrap_or(status.span());
            return Err(diag_use_after_move(
                analysis.func,
                root,
                status.span(),
                use_span,
            ));
        }
    }
    Ok(())
}

/// Returns the container/target `ValueId` mutated in place by container/store instructions.
fn mutating_container_operand(instr: &IrInstr) -> Option<ValueId> {
    match instr {
        IrInstr::ListPush { list, .. }
        | IrInstr::ListSet { list, .. }
        | IrInstr::ListPop { list, .. }
        | IrInstr::ListSort { list } => Some(*list),
        IrInstr::ArrayStore { array, .. } => Some(*array),
        IrInstr::MapSet { map, .. } | IrInstr::MapRemove { map, .. } => Some(*map),
        IrInstr::Store { tensor, .. } => Some(*tensor),
        IrInstr::AtomicStore { atomic, .. } | IrInstr::AtomicAdd { atomic, .. } => Some(*atomic),
        IrInstr::CellStore { cell, .. } => Some(*cell),
        _ => None,
    }
}

fn resolve_root(func: &IrFunction, v: ValueId) -> PlaceRoot {
    func.place_meta
        .value_slots
        .get(&v)
        .copied()
        .map(PlaceRoot::Slot)
        .unwrap_or(PlaceRoot::Value(v))
}

fn resolve_place_from_val(
    func: &IrFunction,
    def_map: &HashMap<ValueId, (BlockId, usize)>,
    mut cur: ValueId,
) -> Place {
    let mut projections = Vec::new();
    let mut visited = HashSet::new();
    while visited.insert(cur) {
        let Some(&(bid, ii)) = def_map.get(&cur) else {
            break;
        };
        let Some(block) = func.block(bid) else {
            break;
        };
        let Some(instr) = block.instrs.get(ii) else {
            break;
        };
        match instr {
            IrInstr::GetField {
                base, field_index, ..
            } => {
                projections.push(*field_index);
                cur = *base;
            }
            IrInstr::GetElement { base, index, .. } => {
                projections.push(*index);
                cur = *base;
            }
            _ => break,
        }
    }
    projections.reverse();
    Place {
        root: resolve_root(func, cur),
        projections,
    }
}

fn resolve_borrow_place(
    func: &IrFunction,
    def_map: &HashMap<ValueId, (BlockId, usize)>,
    bid: BlockId,
    ii: usize,
    result: ValueId,
    place: ValueId,
) -> (Place, Option<PlaceSpan>) {
    if let Some(events) = func.place_meta.events.get(&(bid.0, ii)) {
        for event in events {
            if let PlaceEventKind::Borrow { slot, projections } = &event.kind {
                return (
                    Place {
                        root: PlaceRoot::Slot(*slot),
                        projections: projections.clone(),
                    },
                    Some(event.span),
                );
            }
        }
    }
    (
        resolve_place_from_val(func, def_map, place),
        value_or_instr_span(func, bid, ii, result),
    )
}

fn resolve_move_target(
    analysis: &NllAnalysis<'_>,
    bid: BlockId,
    ii: usize,
    place: ValueId,
) -> (Place, PlaceSpan) {
    if let Some(events) = analysis.func.place_meta.events.get(&(bid.0, ii)) {
        for event in events {
            if let PlaceEventKind::Move { slot, projections } = &event.kind {
                return (
                    Place {
                        root: PlaceRoot::Slot(*slot),
                        projections: projections.clone(),
                    },
                    event.span,
                );
            }
        }
    }
    let root = resolve_place_from_val(analysis.func, &analysis.def_map, place).root;
    let span = value_or_instr_span(analysis.func, bid, ii, place)
        .unwrap_or(PlaceSpan { start: 0, end: 0 });
    (Place::whole(root), span)
}

fn value_or_instr_span(
    func: &IrFunction,
    bid: BlockId,
    ii: usize,
    val: ValueId,
) -> Option<PlaceSpan> {
    if let Some(&span) = func.place_meta.value_spans.get(&val) {
        return Some(span);
    }
    func.get_instr_span(bid.0, ii)
        .or_else(|| func.debug_span(bid.0, ii))
        .map(|b| PlaceSpan {
            start: b,
            end: b + 1,
        })
}

fn earlier_span(a: PlaceSpan, b: PlaceSpan) -> PlaceSpan {
    if a.start <= b.start {
        a
    } else {
        b
    }
}

fn place_name(func: &IrFunction, root: PlaceRoot) -> String {
    match root {
        PlaceRoot::Slot(slot) => func.place_meta.slot_names.get(&slot).cloned(),
        PlaceRoot::Value(val) => func.value_names.get(&val).cloned(),
    }
    .unwrap_or_else(|| "value".to_string())
}

fn build_e0382_diag(
    message: String,
    note: String,
    primary_span: Option<PlaceSpan>,
    primary_label: &str,
    secondary_span: Option<PlaceSpan>,
    secondary_label: &str,
) -> PassError {
    let mut spans = Vec::new();
    if let Some(sec) = secondary_span {
        if sec.end > sec.start
            && primary_span.map_or(true, |pri| pri.start != sec.start || pri.end != sec.end)
        {
            spans.push(DiagnosticSpan {
                start: sec.start,
                end: sec.end,
                label: Some(secondary_label.to_string()),
                is_primary: false,
            });
        }
    }
    if let Some(pri) = primary_span {
        if pri.end > pri.start {
            spans.push(DiagnosticSpan {
                start: pri.start,
                end: pri.end,
                label: Some(primary_label.to_string()),
                is_primary: true,
            });
        }
    }
    let diag = Diagnostic {
        code: Some("E0382".into()),
        level: DiagnosticLevel::Error,
        message,
        spans,
        notes: vec![note],
        helps: vec![],
        suggestions: vec![],
        fixes: vec![],
    };
    PassError::rich("E0382", diag)
}

fn diag_incompatible_branch_borrows(func: &IrFunction, l1: &Loan, l2: &Loan) -> PassError {
    let var = place_name(func, l1.place.root);
    let (shared, mutable) = if l1.kind == BorrowKind::Mutable {
        (l2, l1)
    } else {
        (l1, l2)
    };
    build_e0382_diag(
        format!(
            "cannot borrow `{}` as mutable because it is already borrowed as immutable across control-flow arms",
            var
        ),
        format!("value `{}` cannot unify mutable and immutable borrows", var),
        mutable.span,
        "mutable borrow occurs here",
        shared.span,
        "immutable borrow occurs here",
    )
}

fn diag_mut_borrow_conflict(func: &IrFunction, existing: &Loan, new_loan: &Loan) -> PassError {
    let var = place_name(func, existing.place.root);
    let (msg, sec_label) = match existing.kind {
        BorrowKind::Shared => (
            format!(
                "cannot borrow `{}` as mutable because it is already borrowed as immutable",
                var
            ),
            "immutable borrow occurs here",
        ),
        BorrowKind::Mutable => (
            format!(
                "cannot borrow `{}` as mutable because it is already borrowed as mutable",
                var
            ),
            "first mutable borrow occurs here",
        ),
    };
    build_e0382_diag(
        msg,
        format!("value `{}` cannot be used while borrowed or moved", var),
        new_loan.span,
        "mutable borrow occurs here",
        existing.span,
        sec_label,
    )
}

fn diag_borrow_while_mut_borrowed(
    func: &IrFunction,
    existing: &Loan,
    new_loan: &Loan,
) -> PassError {
    let var = place_name(func, existing.place.root);
    build_e0382_diag(
        format!(
            "cannot borrow `{}` as immutable because it is already borrowed as mutable",
            var
        ),
        format!("value `{}` cannot be used while borrowed or moved", var),
        new_loan.span,
        "immutable borrow occurs here",
        existing.span,
        "mutable borrow occurs here",
    )
}

fn diag_mutate_while_borrowed(
    func: &IrFunction,
    root: PlaceRoot,
    existing: &Loan,
    mutate_span: Option<PlaceSpan>,
) -> PassError {
    let var = place_name(func, root);
    build_e0382_diag(
        format!("cannot mutate `{}` because it is currently borrowed", var),
        format!("value `{}` cannot be used while borrowed or moved", var),
        mutate_span,
        "conflicting mutation occurs here",
        existing.span,
        "first borrow occurs here",
    )
}

fn diag_use_while_mut_borrowed(
    func: &IrFunction,
    root: PlaceRoot,
    existing: &Loan,
    use_span: PlaceSpan,
) -> PassError {
    let var = place_name(func, root);
    build_e0382_diag(
        format!(
            "cannot borrow `{}` as immutable because it is already borrowed as mutable",
            var
        ),
        format!(
            "value `{}` cannot be read directly while mutably borrowed",
            var
        ),
        Some(use_span),
        "conflicting read occurs here",
        existing.span,
        "mutable borrow occurs here",
    )
}

fn diag_move_while_borrowed(
    func: &IrFunction,
    root: PlaceRoot,
    existing: &Loan,
    move_span: Option<PlaceSpan>,
) -> PassError {
    let var = place_name(func, root);
    build_e0382_diag(
        format!("cannot move `{}` because it is currently borrowed", var),
        format!("value `{}` cannot be used while borrowed or moved", var),
        move_span,
        "move occurs here",
        existing.span,
        "first borrow occurs here",
    )
}

fn diag_use_after_move(
    func: &IrFunction,
    root: PlaceRoot,
    move_span: PlaceSpan,
    use_span: PlaceSpan,
) -> PassError {
    let var = place_name(func, root);
    build_e0382_diag(
        format!("use of moved value `{}`", var),
        format!("value `{}` cannot be used while borrowed or moved", var),
        Some(use_span),
        "use of moved value here",
        Some(move_span),
        "value moved here",
    )
}

fn diag_borrow_after_move(
    func: &IrFunction,
    root: PlaceRoot,
    move_span: PlaceSpan,
    borrow_span: Option<PlaceSpan>,
) -> PassError {
    let var = place_name(func, root);
    build_e0382_diag(
        format!("cannot borrow `{}` because it has been moved", var),
        format!("value `{}` cannot be used while borrowed or moved", var),
        borrow_span,
        "borrow occurs here",
        Some(move_span),
        "value moved here",
    )
}

fn diag_move_after_move(
    func: &IrFunction,
    root: PlaceRoot,
    first_move: PlaceSpan,
    second_move: PlaceSpan,
) -> PassError {
    let var = place_name(func, root);
    build_e0382_diag(
        format!("cannot move `{}` because it has already been moved", var),
        format!("value `{}` cannot be used while borrowed or moved", var),
        Some(second_move),
        "second move occurs here",
        Some(first_move),
        "first move occurs here",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::function::Param;
    use crate::ir::instr::{BinOp, IrInstr};
    use crate::ir::module::IrFunctionBuilder;
    use crate::ir::types::{DType, IrType};

    #[test]
    fn test_cfg_liveness_straight_line() {
        let mut builder = IrFunctionBuilder::new(
            "test_liveness",
            vec![Param {
                name: "x".into(),
                ty: IrType::Scalar(DType::I64),
            }],
            IrType::Scalar(DType::I64),
        );

        let b0 = builder.create_block(Some("entry"));
        builder.set_current_block(b0);
        let p_x = builder.add_block_param(b0, Some("x"), IrType::Scalar(DType::I64));

        let v1 = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: v1,
                value: 42,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );

        let v2 = builder.fresh_value();
        builder.push_instr(
            IrInstr::BinOp {
                result: v2,
                op: BinOp::Add,
                lhs: p_x,
                rhs: v1,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );

        builder.push_instr(IrInstr::Return { values: vec![v2] }, None);
        let func = builder.build();

        let liveness = FunctionLiveness::compute(&func);
        let b0_info = liveness.blocks.get(&b0).expect("b0 exists");

        // p_x is a param, so defined in block.
        assert!(b0_info.defs.contains(&p_x));
        assert!(b0_info.defs.contains(&v1));
        assert!(b0_info.defs.contains(&v2));

        // Return exits the function with v2, so nothing is live_out after return.
        assert!(b0_info.live_out.is_empty());

        // Last use of p_x and v1 is at the BinOp instruction (index 1)
        assert_eq!(liveness.last_use_of(p_x), Some((b0, 1)));
        assert_eq!(liveness.last_use_of(v1), Some((b0, 1)));
    }

    #[test]
    fn test_cfg_liveness_branching_non_lexical() {
        // BB0: condbr -> BB1, BB2
        // BB1: use x, br BB3
        // BB2: no use of x, br BB3
        // BB3: return 0
        let mut builder = IrFunctionBuilder::new(
            "test_branch_liveness",
            vec![
                Param {
                    name: "c".into(),
                    ty: IrType::Scalar(DType::Bool),
                },
                Param {
                    name: "x".into(),
                    ty: IrType::Scalar(DType::I64),
                },
            ],
            IrType::Scalar(DType::I64),
        );

        let b0 = builder.create_block(Some("b0"));
        let b1 = builder.create_block(Some("b1"));
        let b2 = builder.create_block(Some("b2"));
        let b3 = builder.create_block(Some("b3"));

        builder.set_current_block(b0);
        let c = builder.add_block_param(b0, Some("c"), IrType::Scalar(DType::Bool));
        let x = builder.add_block_param(b0, Some("x"), IrType::Scalar(DType::I64));
        builder.push_instr(
            IrInstr::CondBr {
                cond: c,
                then_block: b1,
                then_args: vec![],
                else_block: b2,
                else_args: vec![],
            },
            None,
        );

        // BB1: uses x
        builder.set_current_block(b1);
        let v_add = builder.fresh_value();
        builder.push_instr(
            IrInstr::BinOp {
                result: v_add,
                op: BinOp::Add,
                lhs: x,
                rhs: x,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );
        builder.push_instr(
            IrInstr::Br {
                target: b3,
                args: vec![],
            },
            None,
        );

        // BB2: does NOT use x
        builder.set_current_block(b2);
        builder.push_instr(
            IrInstr::Br {
                target: b3,
                args: vec![],
            },
            None,
        );

        // BB3: return 0
        builder.set_current_block(b3);
        let v_ret = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: v_ret,
                value: 0,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );
        builder.push_instr(
            IrInstr::Return {
                values: vec![v_ret],
            },
            None,
        );

        let func = builder.build();
        let liveness = FunctionLiveness::compute(&func);

        // x is live_in to BB1 because it is used there
        assert!(liveness.is_live_in(b1, x));

        // x is NOT live_in to BB2 because it is neither used in BB2 nor live_out of BB2
        assert!(!liveness.is_live_in(b2, x));

        // x is NOT live_in to BB3
        assert!(!liveness.is_live_in(b3, x));
    }

    fn run_cfg_checker(func: IrFunction) -> Result<IrModule, PassError> {
        let mut module = IrModule::new("test");
        module.add_function(func).expect("add function");
        let mut pass = CfgBorrowCheckerPass;
        pass.run(&mut module)?;
        Ok(module)
    }

    #[test]
    fn test_nll_early_loan_release_allows_mut_borrow() {
        let i64_ty = IrType::Scalar(DType::I64);
        let mut builder = IrFunctionBuilder::new("nll_ok", vec![], i64_ty.clone());
        let b0 = builder.create_block(Some("entry"));
        builder.set_current_block(b0);

        let slot_x = builder.alloc_place_slot("x", None);
        let x = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: x,
                value: 10,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        builder.map_value_slot_if_unset(x, slot_x);

        let r = builder.fresh_value();
        builder.record_place_event(
            PlaceEventKind::Borrow {
                slot: slot_x,
                projections: vec![],
            },
            10,
            12,
        );
        builder.push_instr(
            IrInstr::Borrow {
                result: r,
                place: x,
                is_mut: false,
            },
            Some(i64_ty.clone()),
        );

        let one = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: one,
                value: 1,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let used = builder.fresh_value();
        builder.push_instr(
            IrInstr::BinOp {
                result: used,
                op: BinOp::Add,
                lhs: r,
                rhs: one,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        // `r` is dead here: mutably borrowing and mutating `x` must succeed.
        builder.record_place_event(
            PlaceEventKind::Write {
                slot: slot_x,
                field_index: None,
            },
            30,
            36,
        );
        let x2 = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: x2,
                value: 20,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let rm = builder.fresh_value();
        builder.record_place_event(
            PlaceEventKind::Borrow {
                slot: slot_x,
                projections: vec![],
            },
            40,
            46,
        );
        builder.push_instr(
            IrInstr::Borrow {
                result: rm,
                place: x2,
                is_mut: true,
            },
            Some(i64_ty.clone()),
        );
        let sum = builder.fresh_value();
        builder.push_instr(
            IrInstr::BinOp {
                result: sum,
                op: BinOp::Add,
                lhs: used,
                rhs: rm,
                ty: i64_ty.clone(),
            },
            Some(i64_ty),
        );
        builder.push_instr(IrInstr::Return { values: vec![sum] }, None);

        let checked = run_cfg_checker(builder.build()).expect("NLL early expiry should pass");
        assert!(!checked.functions[0].blocks[0]
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::Borrow { .. } | IrInstr::Move { .. })));
    }

    #[test]
    fn test_nll_overlapping_shared_then_mut_borrow_rejected() {
        let i64_ty = IrType::Scalar(DType::I64);
        let mut builder = IrFunctionBuilder::new("nll_err", vec![], i64_ty.clone());
        let b0 = builder.create_block(Some("entry"));
        builder.set_current_block(b0);

        let slot_x = builder.alloc_place_slot("x", None);
        let x = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: x,
                value: 10,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        builder.map_value_slot_if_unset(x, slot_x);

        let r1 = builder.fresh_value();
        builder.record_place_event(
            PlaceEventKind::Borrow {
                slot: slot_x,
                projections: vec![],
            },
            5,
            7,
        );
        builder.push_instr(
            IrInstr::Borrow {
                result: r1,
                place: x,
                is_mut: false,
            },
            Some(i64_ty.clone()),
        );

        let rm = builder.fresh_value();
        builder.record_place_event(
            PlaceEventKind::Borrow {
                slot: slot_x,
                projections: vec![],
            },
            15,
            21,
        );
        builder.push_instr(
            IrInstr::Borrow {
                result: rm,
                place: x,
                is_mut: true,
            },
            Some(i64_ty.clone()),
        );

        let sum = builder.fresh_value();
        builder.push_instr(
            IrInstr::BinOp {
                result: sum,
                op: BinOp::Add,
                lhs: r1,
                rhs: rm,
                ty: i64_ty.clone(),
            },
            Some(i64_ty),
        );
        builder.push_instr(IrInstr::Return { values: vec![sum] }, None);

        let err = run_cfg_checker(builder.build()).expect_err("overlapping &mut while & is live");
        let msg = err.to_string();
        assert!(msg.contains("cannot borrow `x` as mutable"), "got: {msg}");
    }

    #[test]
    fn test_nll_cross_arm_incompatible_borrows_rejected() {
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let mut builder = IrFunctionBuilder::new(
            "cross_arm",
            vec![Param {
                name: "cond".into(),
                ty: bool_ty.clone(),
            }],
            i64_ty.clone(),
        );

        let entry = builder.create_block(Some("entry"));
        let then_bb = builder.create_block(Some("then"));
        let else_bb = builder.create_block(Some("else"));
        let merge_bb = builder.create_block(Some("merge"));
        let merged = builder.add_block_param(merge_bb, Some("y"), i64_ty.clone());

        builder.set_current_block(entry);
        let cond = builder.add_block_param(entry, Some("cond"), bool_ty);
        let slot_x = builder.alloc_place_slot("x", None);
        let x = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: x,
                value: 10,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        builder.map_value_slot_if_unset(x, slot_x);
        builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: then_bb,
                then_args: vec![],
                else_block: else_bb,
                else_args: vec![],
            },
            None,
        );

        builder.set_current_block(then_bb);
        let r_shared = builder.fresh_value();
        builder.record_place_event(
            PlaceEventKind::Borrow {
                slot: slot_x,
                projections: vec![],
            },
            20,
            22,
        );
        builder.push_instr(
            IrInstr::Borrow {
                result: r_shared,
                place: x,
                is_mut: false,
            },
            Some(i64_ty.clone()),
        );
        builder.push_instr(
            IrInstr::Br {
                target: merge_bb,
                args: vec![r_shared],
            },
            None,
        );

        builder.set_current_block(else_bb);
        let r_mut = builder.fresh_value();
        builder.record_place_event(
            PlaceEventKind::Borrow {
                slot: slot_x,
                projections: vec![],
            },
            30,
            36,
        );
        builder.push_instr(
            IrInstr::Borrow {
                result: r_mut,
                place: x,
                is_mut: true,
            },
            Some(i64_ty),
        );
        builder.push_instr(
            IrInstr::Br {
                target: merge_bb,
                args: vec![r_mut],
            },
            None,
        );

        builder.set_current_block(merge_bb);
        builder.push_instr(
            IrInstr::Return {
                values: vec![merged],
            },
            None,
        );

        let err = run_cfg_checker(builder.build()).expect_err("cross-arm borrow must fail");
        let msg = err.to_string();
        assert!(msg.contains("cannot borrow `x` as mutable"), "got: {msg}");
    }

    #[test]
    fn test_nll_conditional_move_and_reinit() {
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);
        let build_conditional_move = |reinit: bool| {
            let mut builder = IrFunctionBuilder::new(
                "cond_move",
                vec![Param {
                    name: "c".into(),
                    ty: bool_ty.clone(),
                }],
                i64_ty.clone(),
            );
            let entry = builder.create_block(Some("entry"));
            let then_bb = builder.create_block(Some("then"));
            let else_bb = builder.create_block(Some("else"));
            let merge_bb = builder.create_block(Some("merge"));

            builder.set_current_block(entry);
            let c = builder.add_block_param(entry, Some("c"), bool_ty.clone());
            let slot_x = builder.alloc_place_slot("x", None);
            let x = builder.fresh_value();
            builder.push_instr(
                IrInstr::ConstInt {
                    result: x,
                    value: 5,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            builder.map_value_slot_if_unset(x, slot_x);
            builder.push_instr(
                IrInstr::CondBr {
                    cond: c,
                    then_block: then_bb,
                    then_args: vec![],
                    else_block: else_bb,
                    else_args: vec![],
                },
                None,
            );

            builder.set_current_block(then_bb);
            let y = builder.fresh_value();
            builder.record_place_event(
                PlaceEventKind::Move {
                    slot: slot_x,
                    projections: Vec::new(),
                },
                10,
                16,
            );
            builder.push_instr(
                IrInstr::Move {
                    result: y,
                    place: x,
                },
                Some(i64_ty.clone()),
            );
            if reinit {
                builder.record_place_event(
                    PlaceEventKind::Write {
                        slot: slot_x,
                        field_index: None,
                    },
                    20,
                    25,
                );
            }
            builder.push_instr(
                IrInstr::Br {
                    target: merge_bb,
                    args: vec![],
                },
                None,
            );

            builder.set_current_block(else_bb);
            builder.push_instr(
                IrInstr::Br {
                    target: merge_bb,
                    args: vec![],
                },
                None,
            );

            builder.set_current_block(merge_bb);
            builder.record_place_event(
                PlaceEventKind::Read {
                    slot: slot_x,
                    projections: Vec::new(),
                },
                30,
                31,
            );
            builder.push_instr(IrInstr::Return { values: vec![x] }, None);
            builder.build()
        };

        let err = run_cfg_checker(build_conditional_move(false))
            .expect_err("reading maybe-moved variable after branch must fail");
        assert!(
            err.to_string().contains("use of moved value `x`"),
            "got: {}",
            err
        );

        run_cfg_checker(build_conditional_move(true))
            .expect("re-initializing moved variable before join should pass");
    }

    #[test]
    fn test_nll_disjoint_struct_fields_vs_whole_struct() {
        let i64_ty = IrType::Scalar(DType::I64);
        let pair_ty = IrType::Struct {
            name: "Pair".into(),
            fields: vec![("a".into(), i64_ty.clone()), ("b".into(), i64_ty.clone())],
        };

        let build_field_test = |conflict_whole: bool| {
            let mut builder = IrFunctionBuilder::new("fields", vec![], i64_ty.clone());
            let entry = builder.create_block(Some("entry"));
            builder.set_current_block(entry);

            let slot_p = builder.alloc_place_slot("p", None);
            let c1 = builder.fresh_value();
            builder.push_instr(
                IrInstr::ConstInt {
                    result: c1,
                    value: 1,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            let p = builder.fresh_value();
            builder.push_instr(
                IrInstr::MakeStruct {
                    result: p,
                    fields: vec![c1, c1],
                    result_ty: pair_ty.clone(),
                },
                Some(pair_ty.clone()),
            );
            builder.map_value_slot_if_unset(p, slot_p);

            let f0 = builder.fresh_value();
            builder.push_instr(
                IrInstr::GetField {
                    result: f0,
                    base: p,
                    field_index: 0,
                    result_ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            let r0 = builder.fresh_value();
            builder.record_place_event(
                PlaceEventKind::Borrow {
                    slot: slot_p,
                    projections: vec![0],
                },
                10,
                14,
            );
            builder.push_instr(
                IrInstr::Borrow {
                    result: r0,
                    place: f0,
                    is_mut: false,
                },
                Some(i64_ty.clone()),
            );

            let (second_place, projs) = if conflict_whole {
                (p, vec![])
            } else {
                let f1 = builder.fresh_value();
                builder.push_instr(
                    IrInstr::GetField {
                        result: f1,
                        base: p,
                        field_index: 1,
                        result_ty: i64_ty.clone(),
                    },
                    Some(i64_ty.clone()),
                );
                (f1, vec![1])
            };
            let rm = builder.fresh_value();
            builder.record_place_event(
                PlaceEventKind::Borrow {
                    slot: slot_p,
                    projections: projs,
                },
                20,
                28,
            );
            builder.push_instr(
                IrInstr::Borrow {
                    result: rm,
                    place: second_place,
                    is_mut: true,
                },
                Some(i64_ty.clone()),
            );

            let sum = builder.fresh_value();
            builder.push_instr(
                IrInstr::BinOp {
                    result: sum,
                    op: BinOp::Add,
                    lhs: r0,
                    rhs: f0,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            let sum2 = builder.fresh_value();
            builder.push_instr(
                IrInstr::BinOp {
                    result: sum2,
                    op: BinOp::Add,
                    lhs: sum,
                    rhs: rm,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            builder.push_instr(IrInstr::Return { values: vec![sum2] }, None);
            builder.build()
        };

        run_cfg_checker(build_field_test(false))
            .expect("disjoint fields &p.a and &mut p.b must be allowed");
        let err = run_cfg_checker(build_field_test(true))
            .expect_err("&p.a and &mut p overlap and must fail");
        assert!(err.to_string().contains("cannot borrow `p` as mutable"));
    }

    #[test]
    fn test_nll_partial_field_moves_are_independent() {
        let i64_ty = IrType::Scalar(DType::I64);
        let pair_ty = IrType::Struct {
            name: "Pair".into(),
            fields: vec![("a".into(), i64_ty.clone()), ("b".into(), i64_ty.clone())],
        };

        let build_moves = |move_whole: bool| {
            let mut builder = IrFunctionBuilder::new("field_moves", vec![], i64_ty.clone());
            let entry = builder.create_block(Some("entry"));
            builder.set_current_block(entry);

            let slot_p = builder.alloc_place_slot("p", None);
            let c = builder.fresh_value();
            builder.push_instr(
                IrInstr::ConstInt {
                    result: c,
                    value: 1,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            let p = builder.fresh_value();
            builder.push_instr(
                IrInstr::MakeStruct {
                    result: p,
                    fields: vec![c, c],
                    result_ty: pair_ty.clone(),
                },
                Some(pair_ty.clone()),
            );
            builder.map_value_slot_if_unset(p, slot_p);

            let f0 = builder.fresh_value();
            builder.push_instr(
                IrInstr::GetField {
                    result: f0,
                    base: p,
                    field_index: 0,
                    result_ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            let m0 = builder.fresh_value();
            builder.record_place_event(
                PlaceEventKind::Move {
                    slot: slot_p,
                    projections: vec![0],
                },
                10,
                14,
            );
            builder.push_instr(
                IrInstr::Move {
                    result: m0,
                    place: f0,
                },
                Some(i64_ty.clone()),
            );

            if move_whole {
                let mw = builder.fresh_value();
                builder.record_place_event(
                    PlaceEventKind::Move {
                        slot: slot_p,
                        projections: Vec::new(),
                    },
                    20,
                    26,
                );
                builder.push_instr(
                    IrInstr::Move {
                        result: mw,
                        place: p,
                    },
                    Some(pair_ty.clone()),
                );
                builder.push_instr(IrInstr::Return { values: vec![mw] }, None);
            } else {
                let f1 = builder.fresh_value();
                builder.push_instr(
                    IrInstr::GetField {
                        result: f1,
                        base: p,
                        field_index: 1,
                        result_ty: i64_ty.clone(),
                    },
                    Some(i64_ty.clone()),
                );
                let m1 = builder.fresh_value();
                builder.record_place_event(
                    PlaceEventKind::Move {
                        slot: slot_p,
                        projections: vec![1],
                    },
                    20,
                    24,
                );
                builder.push_instr(
                    IrInstr::Move {
                        result: m1,
                        place: f1,
                    },
                    Some(i64_ty.clone()),
                );
                builder.push_instr(IrInstr::Return { values: vec![m1] }, None);
            }
            builder.build()
        };

        run_cfg_checker(build_moves(false))
            .expect("moving disjoint fields p.a and p.b must be allowed");
        let err =
            run_cfg_checker(build_moves(true)).expect_err("moving p.a then the whole p must fail");
        assert!(err.to_string().contains("already been moved"), "got: {err}");
    }

    #[test]
    fn test_nll_loop_move_and_outer_borrow() {
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);

        let mut builder = IrFunctionBuilder::new(
            "loop_move",
            vec![Param {
                name: "cond".into(),
                ty: bool_ty.clone(),
            }],
            i64_ty.clone(),
        );
        let entry = builder.create_block(Some("entry"));
        let header = builder.create_block(Some("header"));
        let body = builder.create_block(Some("body"));
        let exit = builder.create_block(Some("exit"));

        builder.set_current_block(entry);
        let cond = builder.add_block_param(entry, Some("cond"), bool_ty);
        let slot_x = builder.alloc_place_slot("x", None);
        let x = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: x,
                value: 7,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        builder.map_value_slot_if_unset(x, slot_x);
        builder.push_instr(
            IrInstr::Br {
                target: header,
                args: vec![],
            },
            None,
        );

        builder.set_current_block(header);
        builder.push_instr(
            IrInstr::CondBr {
                cond,
                then_block: body,
                then_args: vec![],
                else_block: exit,
                else_args: vec![],
            },
            None,
        );

        builder.set_current_block(body);
        let moved = builder.fresh_value();
        builder.record_place_event(
            PlaceEventKind::Move {
                slot: slot_x,
                projections: Vec::new(),
            },
            15,
            21,
        );
        builder.push_instr(
            IrInstr::Move {
                result: moved,
                place: x,
            },
            Some(i64_ty),
        );
        builder.push_instr(
            IrInstr::Br {
                target: header,
                args: vec![],
            },
            None,
        );

        builder.set_current_block(exit);
        builder.push_instr(IrInstr::Return { values: vec![x] }, None);

        let err = run_cfg_checker(builder.build())
            .expect_err("loop moving `x` without re-init must reject back-edge second move");
        assert!(
            err.to_string().contains("already been moved"),
            "got: {}",
            err
        );
    }

    #[test]
    fn test_nll_loop_mutation_respects_outer_borrow_liveness() {
        let i64_ty = IrType::Scalar(DType::I64);
        let bool_ty = IrType::Scalar(DType::Bool);

        let build_loop_borrow = |use_outer_after_loop: bool| {
            let mut builder = IrFunctionBuilder::new(
                "loop_borrow",
                vec![Param {
                    name: "cond".into(),
                    ty: bool_ty.clone(),
                }],
                i64_ty.clone(),
            );
            let entry = builder.create_block(Some("entry"));
            let header = builder.create_block(Some("header"));
            let body = builder.create_block(Some("body"));
            let exit = builder.create_block(Some("exit"));

            builder.set_current_block(entry);
            let cond = builder.add_block_param(entry, Some("cond"), bool_ty.clone());
            let slot_x = builder.alloc_place_slot("x", None);
            let x = builder.fresh_value();
            builder.push_instr(
                IrInstr::ConstInt {
                    result: x,
                    value: 1,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            builder.map_value_slot_if_unset(x, slot_x);

            let r = builder.fresh_value();
            builder.record_place_event(
                PlaceEventKind::Borrow {
                    slot: slot_x,
                    projections: vec![],
                },
                10,
                12,
            );
            builder.push_instr(
                IrInstr::Borrow {
                    result: r,
                    place: x,
                    is_mut: false,
                },
                Some(i64_ty.clone()),
            );
            builder.push_instr(
                IrInstr::Br {
                    target: header,
                    args: vec![],
                },
                None,
            );

            builder.set_current_block(header);
            builder.push_instr(
                IrInstr::CondBr {
                    cond,
                    then_block: body,
                    then_args: vec![],
                    else_block: exit,
                    else_args: vec![],
                },
                None,
            );

            builder.set_current_block(body);
            builder.record_place_event(
                PlaceEventKind::Write {
                    slot: slot_x,
                    field_index: None,
                },
                25,
                30,
            );
            builder.push_instr(
                IrInstr::Br {
                    target: header,
                    args: vec![],
                },
                None,
            );

            builder.set_current_block(exit);
            let ret_val = if use_outer_after_loop { r } else { x };
            builder.push_instr(
                IrInstr::Return {
                    values: vec![ret_val],
                },
                None,
            );
            builder.build()
        };

        run_cfg_checker(build_loop_borrow(false))
            .expect("outer borrow dead before loop must allow loop mutation");
        let err = run_cfg_checker(build_loop_borrow(true))
            .expect_err("outer borrow used after loop must block loop mutation");
        assert!(
            err.to_string()
                .contains("cannot mutate `x` because it is currently borrowed"),
            "got: {}",
            err
        );
    }

    #[test]
    fn test_nll_end_to_end_source_compilation() {
        let valid_nll = r#"
def main() -> i64 {
    var x: i64 = 10
    val r: &i64 = &x
    val a: i64 = *r + 1
    x = 20
    val r2: &mut i64 = &mut x
    val b: i64 = *r2 + a
    return b
}
"#;
        crate::compile(valid_nll, "nll_e2e_ok", crate::EmitKind::Ir)
            .expect("NLL program with dead borrow before mutation should compile");

        let invalid_nll = r#"
def main() -> i64 {
    var x: i64 = 10
    val r: &i64 = &x
    x = 20
    return *r
}
"#;
        let err = crate::compile(invalid_nll, "nll_e2e_err", crate::EmitKind::Ir)
            .expect_err("live borrow across mutation must be rejected");
        assert!(
            err.diagnostic_code() == "E0382" && err.to_string().contains("borrowed"),
            "unexpected error: {err}"
        );

        let cross_arm = r#"
def main() -> i64 {
    val x: i64 = 10
    val c: i64 = 1 + 1
    val y = if c == 2 { &x } else { &mut x }
    return *y
}
"#;
        let err = crate::compile(cross_arm, "nll_cross_arm_err", crate::EmitKind::Ir)
            .expect_err("cross-arm incompatible borrows must be rejected");
        assert!(
            err.diagnostic_code() == "E0382" && err.to_string().contains("borrow"),
            "unexpected error: {err}"
        );
    }
}
