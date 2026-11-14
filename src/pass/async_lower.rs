//! Async lowering: turns every `async def` poll function into a resumable
//! state machine.
//!
//! The lowerer compiles `async def f(params) -> T` into a constructor `f`
//! (create a task, store the arguments in its frame, schedule it) and a poll
//! function `__poll_f(task) -> T` holding the body, in which each `await` on an
//! unfinished task is an `async_await` placeholder. This pass, which runs last
//! so that every optimisation and the retain/release annotation have already
//! seen the straight-line body, rewrites each poll function so that:
//!
//! * every `return v` completes the task with `v` and returns 1 ("done");
//! * every value live across an `await` is stored in a raw slot of the task's
//!   frame right after it is defined and reloaded right before each use, so
//!   nothing depends on the native stack across a suspension (arguments are
//!   re-read from the slots the constructor filled, and constants are
//!   re-created at entry);
//! * each `await` splits its block: if the awaited task is already done,
//!   execution continues immediately; otherwise the task registers as a
//!   waiter and returns 0 ("pending"), and is re-polled when the awaited task
//!   completes;
//! * a dispatcher at entry jumps to the continuation for the saved state.
//!
//! Because this is an IR-to-IR transformation, the interpreter and the native
//! backend run the same state machine.

use std::collections::{HashMap, HashSet};

use crate::error::PassError;
use crate::ir::block::{BlockId, IrBlock};
use crate::ir::function::IrFunction;
use crate::ir::instr::{BinOp, InstrId, IrInstr};
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};
use crate::ir::value::{ValueDef, ValueId};
use crate::lower::ASYNC_POLL_PREFIX;
use crate::pass::Pass;

pub struct AsyncLowerPass;

impl Pass for AsyncLowerPass {
    fn name(&self) -> &'static str {
        "async-lower"
    }

    fn run(&mut self, module: &mut IrModule) -> Result<(), PassError> {
        // A call to one of these returns a task nobody else holds yet.
        let constructors: HashSet<String> = module
            .functions
            .iter()
            .filter_map(|f| f.name.strip_prefix(ASYNC_POLL_PREFIX).map(str::to_owned))
            .collect();
        for function in &mut module.functions {
            release_consumed_futures(function, &constructors);
            if function.name.starts_with(ASYNC_POLL_PREFIX) {
                lower_poll_function(function)?;
            } else if function
                .blocks
                .iter()
                .flat_map(|b| &b.instrs)
                .any(|i| is_builtin(i, "async_await"))
            {
                return Err(error(
                    function,
                    "a suspending `await` appeared outside an async function",
                ));
            }
        }
        Ok(())
    }
}

fn error(function: &IrFunction, detail: impl Into<String>) -> PassError {
    PassError::TypeError {
        func: function.name.clone(),
        detail: detail.into(),
    }
}

fn is_builtin(instr: &IrInstr, wanted: &str) -> bool {
    matches!(instr, IrInstr::BuiltinCall { name, .. } if name == wanted)
}

/// Release the program's handle on each task this function creates and then
/// uses only once, to await it or to hand it to `async_any`: after that use
/// the handle is dead. The native executor frees a task once the program and
/// the executor are both done with it (the interpreter's tasks are reference
/// counted already). A task whose handle goes anywhere else -- a list, a
/// call, a second await -- is never released, so it is never freed early.
fn release_consumed_futures(function: &mut IrFunction, constructors: &HashSet<String>) {
    let fresh: HashSet<ValueId> = function
        .blocks
        .iter()
        .flat_map(|b| &b.instrs)
        .filter_map(|instr| match instr {
            IrInstr::BuiltinCall { result, name, .. }
                if matches!(
                    name.as_str(),
                    "async_task_new"
                        | "async_sleep"
                        | "async_yield"
                        | "async_recv"
                        | "async_any"
                        | "async_readable"
                ) =>
            {
                Some(*result)
            }
            IrInstr::Call {
                result: Some(result),
                callee,
                ..
            } if constructors.contains(callee) => Some(*result),
            _ => None,
        })
        .collect();
    if fresh.is_empty() {
        return;
    }

    // Per fresh task: the instruction (by result) that consumes it, if exactly
    // one does and nothing else lets the handle escape.
    let mut consumer: HashMap<ValueId, Option<ValueId>> = HashMap::new();
    let mut escaped: HashSet<ValueId> = HashSet::new();
    for instr in function.blocks.iter().flat_map(|b| &b.instrs) {
        for operand in instr.operands() {
            if !fresh.contains(&operand) {
                continue;
            }
            match instr {
                // The (inlined) constructor filling in and scheduling it.
                IrInstr::BuiltinCall { name, args, .. }
                    if (name == "async_frame_set"
                        && args.first() == Some(&operand)
                        && args.get(2) != Some(&operand))
                        || name == "async_schedule" => {}
                IrInstr::BuiltinCall { result, name, .. }
                    if matches!(
                        name.as_str(),
                        "async_await" | "async_block_on" | "async_any"
                    ) =>
                {
                    match consumer.get(&operand) {
                        None => {
                            consumer.insert(operand, Some(*result));
                        }
                        // Consumed twice (even `async_any(t, t)`): keep it.
                        Some(_) => {
                            escaped.insert(operand);
                        }
                    }
                }
                _ => {
                    escaped.insert(operand);
                }
            }
        }
    }
    // Consumer result -> the task to release right after it.
    let release_after: HashMap<ValueId, Vec<ValueId>> = consumer
        .into_iter()
        .filter(|(task, _)| !escaped.contains(task))
        .filter_map(|(task, by)| by.map(|by| (by, task)))
        .fold(HashMap::new(), |mut map, (by, task)| {
            map.entry(by).or_insert_with(Vec::new).push(task);
            map
        });
    if release_after.is_empty() {
        return;
    }

    let mut inserted = false;
    for block_index in 0..function.blocks.len() {
        let block_id = function.blocks[block_index].id.0;
        let mut index = 0;
        while index < function.blocks[block_index].instrs.len() {
            let Some(tasks) = function.blocks[block_index].instrs[index]
                .result()
                .and_then(|r| release_after.get(&r).map(|tasks| (r, tasks.clone())))
            else {
                index += 1;
                continue;
            };
            let (consumer_result, mut tasks) = tasks;
            tasks.sort_by_key(|t| t.0);
            // After the GC retain of the result, if any: a heap result read
            // out of the task's result box must hold its own reference before
            // the box can be freed with the task.
            let mut at = index + 1;
            if matches!(
                function.blocks[block_index].instrs.get(at),
                Some(IrInstr::Retain { ptr }) if *ptr == consumer_result
            ) {
                at += 1;
            }
            for task in tasks {
                let result = ValueId(function.next_value);
                function.next_value += 1;
                function
                    .value_types
                    .insert(result, IrType::Scalar(DType::I64));
                function.blocks[block_index].instrs.insert(
                    at,
                    IrInstr::BuiltinCall {
                        result,
                        name: "async_release".to_owned(),
                        args: vec![task],
                        result_ty: IrType::Scalar(DType::I64),
                    },
                );
                shift_statement_spans(function, block_id, at);
                at += 1;
                inserted = true;
            }
            index = at;
        }
    }
    if inserted {
        rebuild_value_defs(function);
    }
}

/// Keep the interpreter's statement positions (keyed by instruction index)
/// pointing at the same instructions after one is inserted at `at`.
fn shift_statement_spans(function: &mut IrFunction, block: u32, at: usize) {
    let shift = |map: &mut HashMap<(u32, usize), u32>| {
        let moved: Vec<((u32, usize), u32)> = map
            .iter()
            .filter(|((b, i), _)| *b == block && *i >= at)
            .map(|(key, byte)| (*key, *byte))
            .collect();
        for (key, _) in &moved {
            map.remove(key);
        }
        for ((b, i), byte) in moved {
            map.insert((b, i + 1), byte);
        }
    };
    shift(&mut function.span_table.entries);
    shift(&mut function.span_table.line_spans);
}

fn successors(block: &IrBlock) -> Vec<BlockId> {
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
            let mut next: Vec<BlockId> = arms.iter().map(|(_, b)| *b).collect();
            next.extend(*default_block);
            next
        }
        _ => Vec::new(),
    }
}

/// Where a value that lives across an `await` is kept while the task is
/// suspended.
#[derive(Clone, Copy)]
enum Slot {
    /// A constructor argument: its frame slot is never overwritten before the
    /// task completes, so each use simply reads it again.
    Arg(i64),
    /// A raw spill slot, written right after the value is defined.
    Spill(i64),
}

/// Builds the new instructions and values of one poll function.
struct Rewriter<'f> {
    function: &'f mut IrFunction,
    task: ValueId,
    /// Integer constants used by the rewrite, materialised at entry (which
    /// dominates every block) once the rewrite is complete.
    constants: Vec<(ValueId, i64)>,
    constant_ids: HashMap<i64, ValueId>,
}

impl Rewriter<'_> {
    fn fresh(&mut self, ty: IrType) -> ValueId {
        let id = ValueId(self.function.next_value);
        self.function.next_value += 1;
        self.function.value_types.insert(id, ty);
        id
    }

    fn int(&mut self, value: i64) -> ValueId {
        if let Some(&id) = self.constant_ids.get(&value) {
            return id;
        }
        let id = self.fresh(IrType::Scalar(DType::I64));
        self.constants.push((id, value));
        self.constant_ids.insert(value, id);
        id
    }

    fn call(&mut self, name: &str, args: Vec<ValueId>, result_ty: IrType) -> (IrInstr, ValueId) {
        let result = self.fresh(result_ty.clone());
        (
            IrInstr::BuiltinCall {
                result,
                name: name.to_owned(),
                args,
                result_ty,
            },
            result,
        )
    }

    fn frame_get(&mut self, slot: i64, ty: IrType) -> (IrInstr, ValueId) {
        let task = self.task;
        let slot = self.int(slot);
        self.call("async_frame_get", vec![task, slot], ty)
    }

    /// Store `value` in a raw spill slot: the slot holds the value's bits, as
    /// a register would, without taking ownership of anything it points to.
    fn spill_store(&mut self, slot: i64, value: ValueId) -> IrInstr {
        let task = self.task;
        let slot = self.int(slot);
        self.call(
            "async_spill",
            vec![task, slot, value],
            IrType::Scalar(DType::I64),
        )
        .0
    }

    fn spill_load(&mut self, slot: i64, ty: IrType) -> (IrInstr, ValueId) {
        let task = self.task;
        let slot = self.int(slot);
        self.call("async_reload", vec![task, slot], ty)
    }

    /// Re-read a value kept across an await from wherever it lives.
    fn reload(&mut self, slot: Slot, ty: IrType) -> (IrInstr, ValueId) {
        match slot {
            Slot::Arg(slot) => self.frame_get(slot, ty),
            Slot::Spill(slot) => self.spill_load(slot, ty),
        }
    }

    fn push_block(&mut self, instrs: Vec<IrInstr>) -> BlockId {
        let id = BlockId(self.function.blocks.len() as u32);
        let mut block = IrBlock::new(id, None);
        block.instrs = instrs;
        self.function.blocks.push(block);
        id
    }
}

fn lower_poll_function(function: &mut IrFunction) -> Result<(), PassError> {
    if function.blocks.is_empty() {
        return Ok(());
    }
    if function
        .blocks
        .iter()
        .enumerate()
        .any(|(index, block)| block.id.0 as usize != index)
    {
        return Err(error(function, "internal error: block ids are not dense"));
    }
    let entry = function.blocks[0].id;
    let task = match function.blocks[0].params.as_slice() {
        [param] => param.id,
        _ => {
            return Err(error(
                function,
                "internal error: malformed async poll function",
            ))
        }
    };
    if function
        .blocks
        .iter()
        .any(|b| successors(b).contains(&entry))
    {
        return Err(error(
            function,
            "internal error: a branch re-enters an async function's entry block",
        ));
    }

    // Slots 0..n hold the constructor's arguments, read by the prologue the
    // lowerer emits; spills go after them.
    let constants: HashMap<ValueId, i64> = function
        .blocks
        .iter()
        .flat_map(|b| &b.instrs)
        .filter_map(|instr| match instr {
            IrInstr::ConstInt { result, value, .. } => Some((*result, *value)),
            _ => None,
        })
        .collect();
    let arg_slots: HashMap<ValueId, i64> = function
        .blocks
        .iter()
        .flat_map(|b| &b.instrs)
        .filter_map(|instr| match instr {
            IrInstr::BuiltinCall {
                result, name, args, ..
            } if name == "async_frame_get" => args
                .get(1)
                .and_then(|slot| constants.get(slot))
                .map(|&slot| (*result, slot)),
            _ => None,
        })
        .collect();
    let first_spill_slot = arg_slots.values().max().map_or(0, |max| max + 1);

    let mut rw = Rewriter {
        function,
        task,
        constants: Vec::new(),
        constant_ids: HashMap::new(),
    };

    // Constants are re-created at entry on every poll instead of being kept
    // in the frame.
    let hoisted = hoist_constants(rw.function);
    complete_on_return(&mut rw);
    let mut kept_in_registers: HashSet<ValueId> = hoisted
        .iter()
        .filter_map(IrInstr::result)
        .chain(rw.constant_ids.values().copied())
        .collect();
    kept_in_registers.insert(task);
    let spills = live_across_awaits(rw.function, &kept_in_registers)?;
    let mut next_slot = first_spill_slot;
    let mut slots: HashMap<ValueId, Slot> = HashMap::new();
    for value in spills {
        let slot = match arg_slots.get(&value) {
            Some(&arg) => Slot::Arg(arg),
            None => {
                next_slot += 1;
                Slot::Spill(next_slot - 1)
            }
        };
        slots.insert(value, slot);
    }
    spill(&mut rw, entry, &slots);
    let resume_points = split_at_awaits(&mut rw, next_slot);
    dispatch(&mut rw, entry, &resume_points, hoisted);

    let function = rw.function;
    function.return_ty = IrType::Scalar(DType::I64);
    rebuild_value_defs(function);
    Ok(())
}

/// Remove every constant from the body; `dispatch` re-creates them at entry,
/// which dominates every block and runs on every poll.
fn hoist_constants(function: &mut IrFunction) -> Vec<IrInstr> {
    let mut hoisted = Vec::new();
    for block in &mut function.blocks {
        let (constants, rest): (Vec<IrInstr>, Vec<IrInstr>) = std::mem::take(&mut block.instrs)
            .into_iter()
            .partition(|instr| {
                matches!(
                    instr,
                    IrInstr::ConstInt { .. }
                        | IrInstr::ConstFloat { .. }
                        | IrInstr::ConstBool { .. }
                        | IrInstr::ConstStr { .. }
                )
            });
        hoisted.extend(constants);
        block.instrs = rest;
    }
    hoisted
}

/// `return v` → `async_complete(task, v); return 1`.
fn complete_on_return(rw: &mut Rewriter) {
    for index in 0..rw.function.blocks.len() {
        let Some(IrInstr::Return { values }) = rw.function.blocks[index].instrs.last().cloned()
        else {
            continue;
        };
        let value = match values.first() {
            Some(&value) => value,
            None => rw.int(0),
        };
        let task = rw.task;
        let (complete, _) = rw.call(
            "async_complete",
            vec![task, value],
            IrType::Scalar(DType::I64),
        );
        let done = rw.int(1);
        let instrs = &mut rw.function.blocks[index].instrs;
        instrs.pop();
        instrs.push(complete);
        instrs.push(IrInstr::Return { values: vec![done] });
    }
}

/// Values live immediately after some `await` (other than those in
/// `kept_in_registers` and that await's own result), in a stable order.
fn live_across_awaits(
    function: &IrFunction,
    kept_in_registers: &HashSet<ValueId>,
) -> Result<Vec<ValueId>, PassError> {
    let block_count = function.blocks.len();
    let index_of: HashMap<BlockId, usize> = function
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i))
        .collect();
    let succ: Vec<Vec<usize>> = function
        .blocks
        .iter()
        .map(|b| {
            successors(b)
                .iter()
                .filter_map(|s| index_of.get(s).copied())
                .collect()
        })
        .collect();

    // Backward dataflow: live_in(B) = uses(B) ∪ (live_out(B) − defs(B)),
    // with block parameters defined at block entry.
    let mut live_in: Vec<HashSet<ValueId>> = vec![HashSet::new(); block_count];
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..block_count).rev() {
            let mut live: HashSet<ValueId> = succ[b]
                .iter()
                .flat_map(|&s| live_in[s].iter().copied())
                .collect();
            for instr in function.blocks[b].instrs.iter().rev() {
                if let Some(result) = instr.result() {
                    live.remove(&result);
                }
                live.extend(instr.operands());
            }
            for param in &function.blocks[b].params {
                live.remove(&param.id);
            }
            if live != live_in[b] {
                live_in[b] = live;
                changed = true;
            }
        }
    }

    let mut spills: HashSet<ValueId> = HashSet::new();
    for (b, block) in function.blocks.iter().enumerate() {
        let mut live: HashSet<ValueId> = succ[b]
            .iter()
            .flat_map(|&s| live_in[s].iter().copied())
            .collect();
        for instr in block.instrs.iter().rev() {
            if is_builtin(instr, "async_await") {
                let own = instr.result();
                spills.extend(
                    live.iter()
                        .copied()
                        .filter(|v| !kept_in_registers.contains(v) && Some(*v) != own),
                );
            }
            if let Some(result) = instr.result() {
                live.remove(&result);
            }
            live.extend(instr.operands());
        }
    }

    let mut ordered: Vec<ValueId> = spills.into_iter().collect();
    ordered.sort_by_key(|v| v.0);
    for value in &ordered {
        if matches!(
            function.value_types.get(value),
            Some(IrType::Array { .. } | IrType::Slice { .. } | IrType::MutRef(_))
        ) {
            return Err(error(
                function,
                "a fixed-size array cannot be used after an `await` in the same function: \
it lives on the stack, which does not survive suspension; use a list instead",
            ));
        }
    }
    Ok(ordered)
}

/// Store each spilled value to its slot right after its definition, and
/// reload it right before each use.
fn spill(rw: &mut Rewriter, entry: BlockId, slots: &HashMap<ValueId, Slot>) {
    if slots.is_empty() {
        return;
    }
    for index in 0..rw.function.blocks.len() {
        let old = std::mem::take(&mut rw.function.blocks[index].instrs);
        let mut new: Vec<IrInstr> = Vec::with_capacity(old.len() * 2);
        if rw.function.blocks[index].id != entry {
            let params: Vec<(ValueId, i64)> = rw.function.blocks[index]
                .params
                .iter()
                .filter_map(|p| match slots.get(&p.id) {
                    Some(&Slot::Spill(slot)) => Some((p.id, slot)),
                    _ => None,
                })
                .collect();
            for (param, slot) in params {
                new.push(rw.spill_store(slot, param));
            }
        }
        for mut instr in old {
            let mut replacements: HashMap<ValueId, ValueId> = HashMap::new();
            for operand in instr.operands() {
                if replacements.contains_key(&operand) {
                    continue;
                }
                if let Some(&slot) = slots.get(&operand) {
                    let ty = rw
                        .function
                        .value_types
                        .get(&operand)
                        .cloned()
                        .unwrap_or(IrType::Scalar(DType::I64));
                    let (load, loaded) = rw.reload(slot, ty);
                    new.push(load);
                    replacements.insert(operand, loaded);
                }
            }
            if !replacements.is_empty() {
                crate::pass::opt::apply_replacements(&mut instr, &replacements);
            }
            let result = instr.result();
            new.push(instr);
            if let Some((result, Slot::Spill(slot))) =
                result.and_then(|r| slots.get(&r).map(|&slot| (r, slot)))
            {
                let store = rw.spill_store(slot, result);
                new.push(store);
            }
        }
        rw.function.blocks[index].instrs = new;
    }
}

/// Split every block at each `await`; returns (state, continuation) pairs.
fn split_at_awaits(rw: &mut Rewriter, mut next_slot: i64) -> Vec<(i64, BlockId)> {
    let mut resume_points: Vec<(i64, BlockId)> = Vec::new();
    let mut work: Vec<usize> = (0..rw.function.blocks.len()).collect();
    while let Some(index) = work.pop() {
        let Some(at) = rw.function.blocks[index]
            .instrs
            .iter()
            .position(|i| is_builtin(i, "async_await"))
        else {
            continue;
        };
        let mut instrs = std::mem::take(&mut rw.function.blocks[index].instrs);
        let after = instrs.split_off(at + 1);
        let IrInstr::BuiltinCall {
            result,
            args,
            result_ty,
            ..
        } = instrs.pop().expect("the await itself")
        else {
            unreachable!("position() found an await builtin")
        };
        let future = args[0];
        let future_ty = rw
            .function
            .value_types
            .get(&future)
            .cloned()
            .unwrap_or(IrType::Task(Box::new(result_ty.clone())));
        let state = resume_points.len() as i64 + 1;
        // A future that lives across this await (to be released after it)
        // already has a spill slot: it was just reloaded from it. Otherwise
        // it gets a slot of its own here.
        let spilled_at = instrs.iter().rev().find_map(|instr| match instr {
            IrInstr::BuiltinCall {
                result, name, args, ..
            } if *result == future && name == "async_reload" => args
                .get(1)
                .and_then(|slot| rw.constants.iter().find(|(id, _)| id == slot))
                .map(|&(_, slot)| slot),
            _ => None,
        });
        let (slot, save) = match spilled_at {
            Some(slot) => (slot, None),
            None => {
                let slot = next_slot;
                next_slot += 1;
                (slot, Some(rw.spill_store(slot, future)))
            }
        };
        let task = rw.task;

        // Continuation: reload the future, take its result, carry on.
        let (reload, reloaded) = rw.spill_load(slot, future_ty);
        let mut continuation = vec![
            reload,
            IrInstr::BuiltinCall {
                result,
                name: "async_result".to_owned(),
                args: vec![reloaded],
                result_ty,
            },
        ];
        continuation.extend(after);
        let continue_block = rw.push_block(continuation);
        work.push(continue_block.0 as usize);

        // Suspend: wait on the future and report "pending".
        let (wait, _) = rw.call("async_wait", vec![task, future], IrType::Scalar(DType::I64));
        let pending = rw.int(0);
        let suspend_block = rw.push_block(vec![
            wait,
            IrInstr::Return {
                values: vec![pending],
            },
        ]);

        // Before the await: save the future and the state, then take the fast
        // path when the future is already done.
        let state_value = rw.int(state);
        let (set_state, _) = rw.call(
            "async_set_state",
            vec![task, state_value],
            IrType::Scalar(DType::I64),
        );
        let (is_done, done) = rw.call("async_is_done", vec![future], IrType::Scalar(DType::Bool));
        instrs.extend(save);
        instrs.push(set_state);
        instrs.push(is_done);
        instrs.push(IrInstr::CondBr {
            cond: done,
            then_block: continue_block,
            then_args: Vec::new(),
            else_block: suspend_block,
            else_args: Vec::new(),
        });
        rw.function.blocks[index].instrs = instrs;
        resume_points.push((state, continue_block));
    }
    resume_points.sort_by_key(|(state, _)| *state);
    resume_points
}

/// Move the entry block's code to a new block and make entry a dispatcher on
/// the task's saved state; materialise the body's and the rewrite's constants
/// there.
fn dispatch(
    rw: &mut Rewriter,
    entry: BlockId,
    resume_points: &[(i64, BlockId)],
    hoisted: Vec<IrInstr>,
) {
    let entry_index = entry.0 as usize;
    let mut head: Vec<IrInstr> = Vec::new();
    if !resume_points.is_empty() {
        let body = std::mem::take(&mut rw.function.blocks[entry_index].instrs);
        let start = rw.push_block(body);
        let task = rw.task;
        let (load_state, state) = rw.call("async_state", vec![task], IrType::Scalar(DType::I64));
        head.push(load_state);
        // entry: if state == 1 -> C1 else D2; D2: if state == 2 -> C2 else ...
        let mut current: Option<usize> = None;
        for (i, &(state_number, target)) in resume_points.iter().enumerate() {
            let expected = rw.int(state_number);
            let matches = rw.fresh(IrType::Scalar(DType::Bool));
            let compare = IrInstr::BinOp {
                result: matches,
                op: BinOp::CmpEq,
                lhs: state,
                rhs: expected,
                ty: IrType::Scalar(DType::I64),
            };
            let otherwise = if i + 1 == resume_points.len() {
                start
            } else {
                rw.push_block(Vec::new())
            };
            let branch = IrInstr::CondBr {
                cond: matches,
                then_block: target,
                then_args: Vec::new(),
                else_block: otherwise,
                else_args: Vec::new(),
            };
            match current {
                None => {
                    head.push(compare);
                    head.push(branch);
                }
                Some(block) => {
                    rw.function.blocks[block].instrs = vec![compare, branch];
                }
            }
            current = Some(otherwise.0 as usize);
        }
    }
    let mut constants = hoisted;
    constants.extend(
        rw.constants
            .iter()
            .map(|&(result, value)| IrInstr::ConstInt {
                result,
                value,
                ty: IrType::Scalar(DType::I64),
            }),
    );
    let block = &mut rw.function.blocks[entry_index];
    let rest = std::mem::take(&mut block.instrs);
    block.instrs = constants;
    if head.is_empty() {
        block.instrs.extend(rest);
    } else {
        block.instrs.extend(head);
    }
}

fn rebuild_value_defs(function: &mut IrFunction) {
    let mut defs: HashMap<ValueId, ValueDef> = HashMap::new();
    for block in &function.blocks {
        for param in &block.params {
            defs.insert(param.id, ValueDef::BlockParam { block: block.id });
        }
        for (index, instr) in block.instrs.iter().enumerate() {
            if let Some(result) = instr.result() {
                defs.insert(
                    result,
                    ValueDef::InstrResult {
                        block: block.id,
                        instr: InstrId(index as u32),
                    },
                );
            }
        }
    }
    function.value_defs = defs;
}
