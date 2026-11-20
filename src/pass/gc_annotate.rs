/// Phase 83: GC Annotation Pass
///
/// Inserts `Retain` after each heap-allocated value (list, map, option, result,
/// channel, etc.) is created, and `Release` once its last use has passed.
///
/// Every heap-typed creation gets a `Retain`, including IRIS-defined calls
/// (`Call`, `CallClosure`, `DynCall`). The caller must own the reference it
/// releases at last use; without the matching `Retain` a returned value that
/// the callee did not attach a fresh reference to (for example a borrow of a
/// parameter) is released one time too many and its memory is recycled while
/// still in use.
///
/// ## Where the `Release` goes
///
/// A heap value created inside a loop body never dominated the function's
/// `Return`, so a "release everything at the return" rule leaked those values
/// forever — which is every per-frame GUI allocation. This pass instead groups
/// every heap-typed identifier that denotes the *same allocation* into one
/// equivalence group (block parameters merged with the branch arguments they
/// were passed), then computes a standard backward liveness over the groups.
/// Uses are the only thing that keeps a group alive; a group is released in a
/// block when it is used there but not live-out of the block (liveness into a
/// block is filtered by whether some definition of the group dominates it, so
/// dead loop-carried groups die at their definition site instead of staying
/// live around the back-edge forever).
///
/// Still never released: groups containing a function parameter (the caller
/// owns those), groups containing a string constant (not heap), and groups
/// that escape through a `Return` (ownership transfers to the caller).
///
/// In the interpreter, Retain/Release are no-ops.
/// In LLVM IR, they lower to `@iris_retain` / `@iris_release` calls.
use std::collections::{HashMap, HashSet};

use crate::error::PassError;
use crate::ir::block::{BlockId, IrBlock};
use crate::ir::instr::IrInstr;
use crate::ir::module::IrModule;
use crate::ir::types::IrType;
use crate::ir::value::ValueId;
use crate::pass::Pass;

pub struct GcAnnotatePass;

// ---------------------------------------------------------------------------
// Union-find over heap values that may denote the same allocation.
// ---------------------------------------------------------------------------

struct UnionFind {
    parent: Vec<usize>,
    fc_param: Vec<bool>,
    escaping: Vec<bool>,
}

impl UnionFind {
    fn new() -> Self {
        Self {
            parent: Vec::new(),
            fc_param: Vec::new(),
            escaping: Vec::new(),
        }
    }

    fn add(&mut self, fc_param: bool, escaping: bool) -> usize {
        let id = self.parent.len();
        self.parent.push(id);
        self.fc_param.push(fc_param);
        self.escaping.push(escaping);
        id
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return;
        }
        let (big, small) = if ra < rb { (ra, rb) } else { (rb, ra) };
        self.parent[small] = big;
        self.fc_param[big] |= self.fc_param[small];
        self.escaping[big] |= self.escaping[small];
        self.fc_param[small] = false;
        self.escaping[small] = false;
    }
}

impl Pass for GcAnnotatePass {
    fn name(&self) -> &'static str {
        "GcAnnotate"
    }

    fn run(&mut self, module: &mut IrModule) -> Result<(), PassError> {
        // ------------------------------------------------------------------
        // Inter-procedural escape analysis (fixpoint).
        // ------------------------------------------------------------------
        let mut callee_escape: HashMap<String, Vec<usize>> = HashMap::new();
        let mut escaping_per_func: Vec<HashSet<ValueId>> =
            vec![HashSet::new(); module.functions.len()];
        let mut changed = true;
        let mut iters = 0;
        let max_iters = module.functions.len().max(1) + 2;
        while changed && iters < max_iters {
            changed = false;
            iters += 1;
            for (fi, func) in module.functions.iter().enumerate() {
                let bps = build_block_param_sources(func);
                let mut escaped: HashSet<ValueId> = HashSet::new();
                for block in &func.blocks {
                    for instr in &block.instrs {
                        if let IrInstr::Return { values } = instr {
                            escaped.extend(collect_return_escape_values(
                                func,
                                values,
                                &bps,
                                &callee_escape,
                            ));
                        }
                    }
                }
                let entry_params: Vec<ValueId> = func
                    .blocks
                    .first()
                    .map(|b| b.params.iter().map(|p| p.id).collect())
                    .unwrap_or_default();
                let new_params: Vec<usize> = entry_params
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| escaped.contains(p))
                    .map(|(i, _)| i)
                    .collect();
                if callee_escape.get(&func.name) != Some(&new_params) {
                    callee_escape.insert(func.name.clone(), new_params);
                    changed = true;
                }
                if escaping_per_func[fi] != escaped {
                    escaping_per_func[fi] = escaped;
                    changed = true;
                }
            }
        }

        for (fi, func) in module.functions.iter_mut().enumerate() {
            let n = func.blocks.len();
            if n == 0 {
                continue;
            }

            let escaping = escaping_per_func[fi].clone();

            // ------------------------------------------------------------------
            // 1. Constant strings and function (entry block) parameters.
            // ------------------------------------------------------------------
            let mut const_strs: HashSet<ValueId> = HashSet::new();
            for block in &func.blocks {
                for instr in &block.instrs {
                    if let IrInstr::ConstStr { result, .. } = instr {
                        const_strs.insert(*result);
                    }
                }
            }

            let mut function_params: HashSet<ValueId> = HashSet::new();
            if let Some(entry) = func.blocks.first() {
                for param in &entry.params {
                    if is_heap_ty(&param.ty) {
                        function_params.insert(param.id);
                    }
                }
            }

            // ------------------------------------------------------------------
            // 2. Union heap identifiers that may denote one allocation.
            //    Seeds: heap instruction results and heap block params. A
            //    branch's argument and the parameter it flows into are merged.
            // ------------------------------------------------------------------
            let mut uf = UnionFind::new();
            let mut idx_of: HashMap<ValueId, usize> = HashMap::new();

            let heap_of = |value: ValueId| -> Option<IrType> {
                func.value_types.get(&value).cloned().filter(is_heap_ty)
            };

            for block in &func.blocks {
                for param in &block.params {
                    if !is_heap_ty(&param.ty) || const_strs.contains(&param.id) {
                        continue;
                    }
                    let fc = function_params.contains(&param.id);
                    let esc = escaping.contains(&param.id);
                    let idx = uf.add(fc, esc);
                    idx_of.insert(param.id, idx);
                }
                for instr in &block.instrs {
                    let Some(result) = instr.result() else {
                        continue;
                    };
                    if const_strs.contains(&result) {
                        continue;
                    }
                    if heap_of(result).is_none() {
                        continue;
                    }
                    let esc = escaping.contains(&result);
                    let idx = uf.add(false, esc);
                    idx_of.insert(result, idx);
                }
            }

            // Merge the two sides of every branch edge.
            let mut block_by_id: HashMap<BlockId, usize> = HashMap::new();
            for (i, block) in func.blocks.iter().enumerate() {
                block_by_id.insert(block.id, i);
            }
            let merge_edge = |uf: &mut UnionFind,
                              idx_of: &mut HashMap<ValueId, usize>,
                              from: ValueId,
                              target: BlockId,
                              arg_index: usize| {
                let Some(&ti) = block_by_id.get(&target) else {
                    return;
                };
                let Some(to_param) = func.blocks[ti].params.get(arg_index).map(|p| p.id) else {
                    return;
                };
                if let (Some(&a), Some(&b)) = (idx_of.get(&from), idx_of.get(&to_param)) {
                    uf.union(a, b);
                }
            };
            for block in &func.blocks {
                for instr in &block.instrs {
                    match instr {
                        IrInstr::Br { target, args } => {
                            for (i, arg) in args.iter().enumerate() {
                                merge_edge(&mut uf, &mut idx_of, *arg, *target, i);
                            }
                        }
                        IrInstr::CondBr {
                            then_block,
                            then_args,
                            else_block,
                            else_args,
                            ..
                        } => {
                            for (i, arg) in then_args.iter().enumerate() {
                                merge_edge(&mut uf, &mut idx_of, *arg, *then_block, i);
                            }
                            for (i, arg) in else_args.iter().enumerate() {
                                merge_edge(&mut uf, &mut idx_of, *arg, *else_block, i);
                            }
                        }
                        _ => {}
                    }
                }
            }

            // ------------------------------------------------------------------
            // 3. Per-block use sets over representatives.
            // ------------------------------------------------------------------
            let mut gen: Vec<HashSet<usize>> = vec![HashSet::new(); n];
            let mut def_in: Vec<Vec<ValueId>> = vec![Vec::new(); n];
            let mut used_in: Vec<Vec<ValueId>> = vec![Vec::new(); n];
            let mut rep_defs: HashMap<usize, Vec<usize>> = HashMap::new();

            for (bi, block) in func.blocks.iter().enumerate() {
                for param in &block.params {
                    if let Some(&idx) = idx_of.get(&param.id) {
                        let rep = uf.find(idx);
                        def_in[bi].push(param.id);
                        rep_defs.entry(rep).or_default().push(bi);
                    }
                }
                for instr in &block.instrs {
                    for operand in instr.operands() {
                        if let Some(&idx) = idx_of.get(&operand) {
                            let rep = uf.find(idx);
                            gen[bi].insert(rep);
                            used_in[bi].push(operand);
                        }
                    }
                    if let Some(result) = instr.result() {
                        if let Some(&idx) = idx_of.get(&result) {
                            let rep = uf.find(idx);
                            def_in[bi].push(result);
                            rep_defs.entry(rep).or_default().push(bi);
                        }
                    }
                }
            }

            // ------------------------------------------------------------------
            // 4. Dominators: a representative may only flow into a block that
            //    some definition of it dominates.
            // ------------------------------------------------------------------
            let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
            let mut succs_id: Vec<Vec<usize>> = vec![Vec::new(); n];
            for (i, block) in func.blocks.iter().enumerate() {
                for succ in block_successors(block) {
                    if let Some(&s) = block_by_id.get(&succ) {
                        succs_id[i].push(s);
                    }
                }
            }
            for (i, ss) in succs_id.iter().enumerate() {
                for &s in ss {
                    preds[s].push(i);
                }
            }

            // Loop unrolling leaves the old header/body blocks behind as dead
            // code. Treating those unreachable blocks as dominator roots makes
            // every real successor look undominated by the function entry, so
            // liveness stops at the loop and loop-invariant values get a
            // Release inside the loop (freeing a list that is still used).
            // Compute reachability from the entry and ignore dead predecessors.
            let mut reachable = vec![false; n];
            {
                let mut stack = vec![0usize];
                reachable[0] = true;
                while let Some(b) = stack.pop() {
                    for &s in &succs_id[b] {
                        if !reachable[s] {
                            reachable[s] = true;
                            stack.push(s);
                        }
                    }
                }
            }

            let mut dom: Vec<HashSet<usize>> = vec![HashSet::new(); n];
            dom[0].insert(0);
            for d in dom.iter_mut().skip(1) {
                *d = (0..n).collect();
            }
            loop {
                let mut changed = false;
                for b in 1..n {
                    if !reachable[b] {
                        continue;
                    }
                    let live_preds: Vec<usize> =
                        preds[b].iter().copied().filter(|&p| reachable[p]).collect();
                    let mut new_dom: HashSet<usize> = (0..n).collect();
                    if live_preds.is_empty() {
                        new_dom.clear();
                        new_dom.insert(b);
                    } else {
                        for p in live_preds {
                            new_dom = new_dom.intersection(&dom[p]).copied().collect();
                        }
                        new_dom.insert(b);
                    }
                    if new_dom != dom[b] {
                        dom[b] = new_dom;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
            let dominates =
                |def_block: usize, b: usize| reachable[b] && dom[b].contains(&def_block);

            // ------------------------------------------------------------------
            // 5. Backward liveness fixpoint over representatives.
            //    live_out(b) = ∪ live_in(succ(b))
            //    live_in(b)  = gen(b) ∪ { r ∈ live_out(b) : some def of r dominates b }
            // ------------------------------------------------------------------
            let succs: Vec<Vec<usize>> = func
                .blocks
                .iter()
                .map(|block| {
                    block_successors(block)
                        .into_iter()
                        .filter_map(|s| block_by_id.get(&s).copied())
                        .collect()
                })
                .collect();

            let mut live_in: Vec<HashSet<usize>> = vec![HashSet::new(); n];
            let mut live_out: Vec<HashSet<usize>> = vec![HashSet::new(); n];
            loop {
                let mut changed = false;
                for b in (0..n).rev() {
                    let mut out: HashSet<usize> = HashSet::new();
                    for &s in &succs[b] {
                        out.extend(live_in[s].iter().copied());
                    }
                    let mut inn = gen[b].clone();
                    for r in &out {
                        if inn.contains(r) {
                            continue;
                        }
                        let reachable = rep_defs
                            .get(r)
                            .is_some_and(|ds| ds.iter().any(|&d| dominates(d, b)));
                        if reachable {
                            inn.insert(*r);
                        }
                    }
                    if inn != live_in[b] {
                        live_in[b] = inn;
                        changed = true;
                    }
                    live_out[b] = out;
                }
                if !changed {
                    break;
                }
            }

            // ------------------------------------------------------------------
            // 6. Releases: reps used in a block but not live-out of it, plus reps
            //    created but never used anywhere (released at their definition
            //    site). Skip function-parameter owners and escaping reps.
            // ------------------------------------------------------------------
            let mut releases: Vec<Vec<(ValueId, IrType)>> = vec![Vec::new(); n];
            for b in 0..n {
                for rep in gen[b].iter().copied() {
                    if live_out[b].contains(&rep) {
                        continue;
                    }
                    if uf.fc_param[rep] || uf.escaping[rep] {
                        continue;
                    }
                    let mut pick: Option<ValueId> = None;
                    for v in def_in[b].iter().chain(used_in[b].iter()) {
                        if idx_of.get(v).map(|&i| uf.find(i)) == Some(rep) {
                            pick = Some(*v);
                            break;
                        }
                    }
                    if let Some(ptr) = pick {
                        let ty = func.value_types.get(&ptr).cloned().unwrap_or(IrType::Str);
                        releases[b].push((ptr, ty));
                    }
                }
            }
            let mut used_reps: HashSet<usize> = HashSet::new();
            for b in 0..n {
                for r in gen[b].iter().copied() {
                    used_reps.insert(r);
                }
            }
            for (&rep, defs) in &rep_defs {
                if used_reps.contains(&rep) {
                    continue;
                }
                if uf.fc_param[rep] || uf.escaping[rep] {
                    continue;
                }
                for &d in defs {
                    let mut pick: Option<ValueId> = None;
                    for v in def_in[d].iter().chain(used_in[d].iter()) {
                        if idx_of.get(v).map(|&i| uf.find(i)) == Some(rep) {
                            pick = Some(*v);
                            break;
                        }
                    }
                    if let Some(ptr) = pick {
                        let ty = func.value_types.get(&ptr).cloned().unwrap_or(IrType::Str);
                        releases[d].push((ptr, ty));
                    }
                }
            }

            // ------------------------------------------------------------------
            // 7. Per-block: insert Retain after creation (and when storing heap
            //    values into structs/cells whose storage does not Retain), and
            //    the Releases computed above immediately before the terminator.
            // ------------------------------------------------------------------
            for bidx in 0..func.blocks.len() {
                // Positions: heap-creating / storing instructions and the Return position.
                let mut inserts_after: Vec<(usize, IrInstr)> = Vec::new();

                for (i, instr) in func.blocks[bidx].instrs.iter().enumerate() {
                    if let Some(result) = instr.result() {
                        if let Some(ty) = func.value_types.get(&result) {
                            if is_heap_ty(ty) && !matches!(instr, IrInstr::ConstStr { .. }) {
                                inserts_after.push((i, IrInstr::Retain { ptr: result }));
                            }
                        }
                    }
                    match instr {
                        IrInstr::MakeStruct { fields, .. } => {
                            for field in fields {
                                if !const_strs.contains(field)
                                    && func.value_types.get(field).is_some_and(is_heap_ty)
                                {
                                    inserts_after.push((i, IrInstr::Retain { ptr: *field }));
                                }
                            }
                        }
                        IrInstr::CellNew { init, .. } => {
                            if !const_strs.contains(init)
                                && func.value_types.get(init).is_some_and(is_heap_ty)
                            {
                                inserts_after.push((i, IrInstr::Retain { ptr: *init }));
                            }
                        }
                        IrInstr::CellStore { value, .. } => {
                            if !const_strs.contains(value)
                                && func.value_types.get(value).is_some_and(is_heap_ty)
                            {
                                inserts_after.push((i, IrInstr::Retain { ptr: *value }));
                            }
                        }
                        _ => {}
                    }
                }

                let instrs = std::mem::take(&mut func.blocks[bidx].instrs);
                let terminator_at = instrs
                    .last()
                    .and_then(|i| i.is_terminator().then_some(instrs.len() - 1));

                let mut new_instrs: Vec<IrInstr> = Vec::with_capacity(instrs.len() + 4);
                for (i, instr) in instrs.into_iter().enumerate() {
                    if terminator_at == Some(i) {
                        for (ptr, ty) in releases[bidx].drain(..) {
                            new_instrs.push(IrInstr::Release { ptr, ty });
                        }
                    }
                    new_instrs.push(instr);
                    for (idx, retain) in &inserts_after {
                        if *idx == i {
                            new_instrs.push(retain.clone());
                        }
                    }
                }
                if terminator_at.is_none() {
                    for (ptr, ty) in releases[bidx].drain(..) {
                        new_instrs.push(IrInstr::Release { ptr, ty });
                    }
                }

                func.blocks[bidx].instrs = new_instrs;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Escape / alias helpers
// ---------------------------------------------------------------------------

fn build_block_param_sources(
    func: &crate::ir::function::IrFunction,
) -> HashMap<ValueId, Vec<ValueId>> {
    let mut sources: HashMap<ValueId, Vec<ValueId>> = HashMap::new();

    for block in &func.blocks {
        for instr in &block.instrs {
            match instr {
                IrInstr::Br { target, args } => {
                    extend_block_param_sources(func, &mut sources, *target, args);
                }
                IrInstr::CondBr {
                    then_block,
                    then_args,
                    else_block,
                    else_args,
                    ..
                } => {
                    extend_block_param_sources(func, &mut sources, *then_block, then_args);
                    extend_block_param_sources(func, &mut sources, *else_block, else_args);
                }
                _ => {}
            }
        }
    }

    sources
}

fn extend_block_param_sources(
    func: &crate::ir::function::IrFunction,
    sources: &mut HashMap<ValueId, Vec<ValueId>>,
    target: BlockId,
    args: &[ValueId],
) {
    let Some(block) = func.block(target) else {
        return;
    };

    for (param, arg) in block.params.iter().zip(args.iter()) {
        let entry = sources.entry(param.id).or_default();
        if !entry.contains(arg) {
            entry.push(*arg);
        }
    }
}

fn collect_return_escape_values(
    func: &crate::ir::function::IrFunction,
    values: &[ValueId],
    block_param_sources: &HashMap<ValueId, Vec<ValueId>>,
    callee_escape: &HashMap<String, Vec<usize>>,
) -> HashSet<ValueId> {
    let mut escaped_heap_vals: HashSet<ValueId> = HashSet::new();
    let mut seen: HashSet<ValueId> = HashSet::new();
    let mut worklist: Vec<ValueId> = values.to_vec();

    while let Some(value) = worklist.pop() {
        if !seen.insert(value) {
            continue;
        }

        if func.value_types.get(&value).is_some_and(is_heap_ty) {
            escaped_heap_vals.insert(value);
        }

        worklist.extend(escape_sources_for_value(
            func,
            value,
            block_param_sources,
            callee_escape,
        ));
    }

    escaped_heap_vals
}

fn escape_sources_for_value(
    func: &crate::ir::function::IrFunction,
    value: ValueId,
    block_param_sources: &HashMap<ValueId, Vec<ValueId>>,
    callee_escape: &HashMap<String, Vec<usize>>,
) -> Vec<ValueId> {
    if let Some(sources) = block_param_sources.get(&value) {
        return sources.clone();
    }

    let Some(instr) = defining_instr(func, value) else {
        return Vec::new();
    };

    match instr {
        IrInstr::MakeStruct { fields, .. } => fields.clone(),
        IrInstr::MakeTuple { elements, .. } => elements.clone(),
        IrInstr::MakeVariant { fields, .. } => fields.clone(),
        IrInstr::MakeSome { value, .. }
        | IrInstr::MakeOk { value, .. }
        | IrInstr::MakeErr { value, .. } => vec![*value],
        IrInstr::MakeClosure { captures, .. } => captures.clone(),
        IrInstr::GetField { base, .. } | IrInstr::GetElement { base, .. } => vec![*base],
        IrInstr::MakeTraitObject { value, .. } => vec![*value],
        IrInstr::DynCall { obj, args, .. } => {
            let mut ops = vec![*obj];
            ops.extend_from_slice(args);
            ops
        }
        IrInstr::Call { callee, args, .. } => {
            if let Some(idx) = callee_escape.get(callee) {
                idx.iter().filter_map(|&i| args.get(i).copied()).collect()
            } else {
                args.clone()
            }
        }
        IrInstr::OptionUnwrap { operand, .. }
        | IrInstr::ResultUnwrap { operand, .. }
        | IrInstr::ResultUnwrapErr { operand, .. } => vec![*operand],
        _ => Vec::new(),
    }
}

fn defining_instr(func: &crate::ir::function::IrFunction, value: ValueId) -> Option<&IrInstr> {
    for block in &func.blocks {
        if block.params.iter().any(|param| param.id == value) {
            return None;
        }

        if let Some(instr) = block
            .instrs
            .iter()
            .find(|instr| instr.result().is_some_and(|result| result == value))
        {
            return Some(instr);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// CFG helpers
// ---------------------------------------------------------------------------

/// Returns the direct successors of a block (targets of its terminator).
fn block_successors(block: &IrBlock) -> Vec<BlockId> {
    match block.terminator() {
        Some(IrInstr::Br { target, .. }) => vec![*target],
        Some(IrInstr::CondBr {
            then_block,
            else_block,
            ..
        }) => vec![*then_block, *else_block],
        Some(IrInstr::SwitchVariant {
            arms,
            default_block,
            ..
        }) => {
            let mut succs: Vec<BlockId> = arms.iter().map(|(_, b)| *b).collect();
            if let Some(d) = default_block {
                succs.push(*d);
            }
            succs
        }
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// Heap-type predicate
// ---------------------------------------------------------------------------

/// Returns `true` for IR types that are reference-counted heap allocations.
fn is_heap_ty(ty: &IrType) -> bool {
    matches!(
        ty,
        IrType::Str
            | IrType::List(_)
            | IrType::Map(_, _)
            | IrType::Option(_)
            | IrType::ResultType(_, _)
            | IrType::Chan(_)
            | IrType::Atomic(_)
            | IrType::Mutex(_)
            | IrType::Grad(_)
            | IrType::Sparse(_)
            | IrType::TaskGroup
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::compile;
    use crate::EmitKind;

    #[test]
    fn branching_heap_alloc_compiles() {
        let src = r#"
            def pick(flag: bool) -> i64 {
                if flag {
                    val xs: list<i64> = list()
                    val _ = list_push(xs, 42)
                    list_get(xs, 0)
                } else {
                    0
                }
            }
        "#;
        let result = compile(src, "test", EmitKind::Ir);
        assert!(
            result.is_ok(),
            "branching heap alloc should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn linear_heap_alloc_retain_release_present() {
        let src = r#"
            def build() -> i64 {
                val xs: list<i64> = list()
                val _ = list_push(xs, 1)
                val _ = list_push(xs, 2)
                list_len(xs)
            }
        "#;
        let ir = compile(src, "test", EmitKind::Ir).expect("should compile");
        assert!(ir.contains("retain"), "IR should contain retain: {}", ir);
        assert!(ir.contains("release"), "IR should contain release: {}", ir);
    }

    #[test]
    fn call_results_receive_a_matching_retain() {
        // A heap value returned from an IRIS-defined function must be retained
        // by the caller: the caller releases it at last use, and without this
        // Retain a borrowed return would be freed while its owner still needs it.
        let src = r#"
            def build(n: i64) -> list<i64> {
                val xs: list<i64> = list()
                var i = 0
                while i < n {
                    push(xs, i)
                    i = i + 1
                }
                xs
            }
            def main(n: i64) -> i64 {
                val xs = build(n)
                list_len(xs)
            }
        "#;
        let ir = compile(src, "test", EmitKind::Ir).expect("should compile");
        let lines: Vec<&str> = ir.lines().collect();
        let balanced = lines.windows(2).any(|w| {
            let call = w[0].trim();
            call.starts_with('%')
                && call.contains("= call @build")
                && w[1].trim().starts_with("retain ")
        });
        assert!(
            balanced,
            "the call result must be retained so its last-use release is balanced: {}",
            ir
        );
    }

    #[test]
    fn no_heap_no_gc_annotations() {
        let src = r#"
            def add(a: i64, b: i64) -> i64 { a + b }
        "#;
        let ir = compile(src, "test", EmitKind::Ir).expect("should compile");
        assert!(
            !ir.contains("retain"),
            "scalar fn should not have retain: {}",
            ir
        );
        assert!(
            !ir.contains("release"),
            "scalar fn should not have release: {}",
            ir
        );
    }

    #[test]
    fn returned_heap_alias_is_not_released() {
        let src = r#"
            def keep(flag: bool) -> list<i64> {
                val xs: list<i64> = list()
                val _ = list_push(xs, 1)
                val alias = if flag { xs } else { xs }
                alias
            }
        "#;
        let ir = compile(src, "test", EmitKind::Ir).expect("should compile");
        assert!(
            !ir.contains("release %"),
            "returned heap alias should not be released before return:\n{}",
            ir
        );
    }

    #[test]
    fn returned_struct_heap_field_is_not_released() {
        let src = r#"
            record Boxed { xs: list<i64>, }
            def build() -> Boxed {
                val xs: list<i64> = list()
                val _ = list_push(xs, 1)
                Boxed { xs: xs }
            }
        "#;
        let ir = compile(src, "test", EmitKind::Ir).expect("should compile");
        assert!(
            !ir.contains("release %"),
            "returned struct field should keep heap members alive:\n{}",
            ir
        );
    }

    #[test]
    fn discarded_heap_alloc_is_released_in_branch() {
        let src = r#"
            def drop_it(flag: bool) -> i64 {
                if flag {
                    val xs: list<i64> = list()
                    val _ = list_push(xs, 1)
                    val _n = list_len(xs)
                    1
                } else {
                    2
                }
            }
        "#;
        let ir = compile(src, "test", EmitKind::Ir).expect("should compile");
        assert!(
            ir.contains("release"),
            "a discarded branch-local list should be released: {}",
            ir
        );
    }

    #[test]
    fn borrowed_list_element_survives_caller() {
        // An IRIS function returning a list_get result must not free the
        // element while the list still owns it.
        let src = r#"
            def first(xs: list<i64>) -> i64 {
                list_get(xs, 0)
            }
            def main() -> i64 {
                val xs = list()
                val _ = list_push(xs, 42)
                val n = first(xs)
                list_len(xs) + n
            }
        "#;
        let ir = compile(src, "test", EmitKind::Ir).expect("should compile");
        assert!(ir.contains("retain"), "IR should contain retain: {}", ir);
    }
}
