use std::collections::HashMap;

use crate::ir::block::{BlockId, IrBlock};
use crate::ir::instr::IrInstr;
use crate::ir::types::{IrType, IrTypeId, TypeArena};
use crate::ir::value::{ValueDef, ValueId};

/// Maps `(block_id, instr_index)` to the start byte offset of the source
/// statement that produced the instruction. Populated during lowering.
/// Used by the debugger for source-level breakpoints and step tracing.
#[derive(Debug, Default, Clone)]
pub struct SpanTable {
    /// Key: `(block_id.0, instr_index)`, value: byte offset into source text.
    pub(crate) entries: HashMap<(u32, usize), u32>,
    /// Key: `ValueId`, value: byte offset into source text for value-producing instructions.
    pub(crate) value_spans: HashMap<ValueId, u32>,
    /// Key: `(block_id.0, instr_index)`: the enclosing statement or tail
    /// expression for *every* instruction, not only a statement's first.
    /// Used for native debug line tables; the interpreter's statement-level
    /// stepping deliberately keeps using `entries`.
    pub(crate) line_spans: HashMap<(u32, usize), u32>,
}

type FunctionLineTables = (HashMap<String, u32>, HashMap<ValueId, u32>);

/// Line positions captured before optimisation, keyed so they can be found
/// again after passes have moved, inserted or rewritten instructions.
#[derive(Debug, Default)]
pub(crate) struct LineSnapshot {
    /// Function name -> (instruction text -> byte, result value -> byte).
    functions: HashMap<String, FunctionLineTables>,
}

impl LineSnapshot {
    pub(crate) fn capture(functions: &[IrFunction]) -> Self {
        let mut snapshot = Self::default();
        for function in functions {
            let (by_text, by_value) = snapshot.functions.entry(function.name.clone()).or_default();
            for block in function.blocks() {
                for (idx, instr) in block.instrs.iter().enumerate() {
                    let byte = function
                        .span_table
                        .get_line(block.id.0, idx)
                        .or_else(|| function.get_instr_span(block.id.0, idx));
                    let Some(byte) = byte else { continue };
                    by_text.entry(format!("{instr:?}")).or_insert(byte);
                    if let Some(key) = stable_key(instr) {
                        by_text.entry(key).or_insert(byte);
                    }
                    if let Some(result) = instr.result() {
                        by_value.entry(result).or_insert(byte);
                    }
                }
            }
        }
        snapshot
    }

    /// Rebuild each function's `line_spans` for its current instructions.
    /// An instruction keeps its position when its result value or its exact
    /// text survived the passes; otherwise it is left unlocated and the
    /// debug-info emitter carries the previous location forward.
    pub(crate) fn restore(&self, functions: &mut [IrFunction]) {
        for function in functions {
            let Some((by_text, by_value)) = self.functions.get(&function.name) else {
                continue;
            };
            let mut line_spans = HashMap::new();
            for block in function.blocks() {
                for (idx, instr) in block.instrs.iter().enumerate() {
                    let byte = instr
                        .result()
                        .and_then(|result| by_value.get(&result))
                        .or_else(|| by_text.get(&format!("{instr:?}")))
                        .or_else(|| stable_key(instr).and_then(|k| by_text.get(&k)));
                    if let Some(&byte) = byte {
                        line_spans.insert((block.id.0, idx), byte);
                    }
                }
            }
            function.span_table.line_spans = line_spans;
        }
    }
}

/// A key that names an instruction whatever passes did to its operands: a
/// `par for` is its body function, which nothing renames.
fn stable_key(instr: &IrInstr) -> Option<String> {
    match instr {
        IrInstr::ParFor { body_fn, .. } => Some(format!("par_for {body_fn}")),
        _ => None,
    }
}

impl SpanTable {
    /// Returns the source byte offset for the given instruction, if known.
    pub fn get(&self, block_id: u32, instr_idx: usize) -> Option<u32> {
        self.entries.get(&(block_id, instr_idx)).copied()
    }

    /// Returns the source byte offset for a specific result value, if known.
    pub fn get_for_val(&self, val: ValueId) -> Option<u32> {
        self.value_spans.get(&val).copied()
    }

    /// Returns the enclosing statement's byte offset for any instruction.
    pub fn get_line(&self, block_id: u32, instr_idx: usize) -> Option<u32> {
        self.line_spans.get(&(block_id, instr_idx)).copied()
    }

    /// Returns `true` if any spans have been recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.value_spans.is_empty()
    }

    /// Returns the number of recorded spans.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Source binding slot identifier (`val`/`var`/parameter/pattern binding) for
/// IR-level borrow and move checking.
///
/// Distinct bindings (including `val y = x` copies and shadowed bindings) get
/// distinct `PlaceSlot`s even when they share an SSA `ValueId`, while
/// reassignments and branch/loop-merged `var` bindings retain the same slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlaceSlot(pub u32);

/// Byte-offset span `(start, end)` stored in IR place metadata for multi-span
/// borrow diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceSpan {
    pub start: u32,
    pub end: u32,
}

/// Source-level place event attached at an instruction boundary `(block_id, instr_idx)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceEventKind {
    /// Reading a variable binding (checks that its slot has not been moved).
    Read(PlaceSlot),
    /// Reassigning or mutating a binding slot (`x = v`, `x.f = v`, `t.0 = v`).
    /// When updating a single struct/tuple field, `field_index` identifies it.
    Write {
        slot: PlaceSlot,
        field_index: Option<usize>,
    },
    /// Attaches the explicit binding slot and optional field projection path
    /// to an `IrInstr::Borrow` at `(block_id, instr_idx)`.
    Borrow {
        slot: PlaceSlot,
        projections: Vec<usize>,
    },
    /// Attaches the explicit binding slot to an `IrInstr::Move` at `(block_id, instr_idx)`.
    Move(PlaceSlot),
}

/// A recorded source-level place operation together with its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceEvent {
    pub kind: PlaceEventKind,
    pub span: PlaceSpan,
}

/// Per-function source-place metadata populated during lowering and consumed
/// by `CfgBorrowCheckerPass` for NLL loan, move, and mutation tracking.
#[derive(Debug, Default, Clone)]
pub struct PlaceMeta {
    /// Human-readable binding name for each `PlaceSlot`.
    pub slot_names: HashMap<PlaceSlot, String>,
    /// Primary binding slot that produced/holds a `ValueId`.
    pub value_slots: HashMap<ValueId, PlaceSlot>,
    /// Place events keyed by `(block_id.0, instr_idx)`. `Borrow`/`Move` events
    /// apply to the instruction at `instr_idx`; `Read`/`Write` events occur
    /// just before `instr_idx` (or before the terminator at `instrs.len() - 1`).
    pub events: HashMap<(u32, usize), Vec<PlaceEvent>>,
    /// Full `(start, end)` span per `ValueId` when recorded during lowering.
    pub value_spans: HashMap<ValueId, PlaceSpan>,
    /// Next slot ID to allocate.
    pub(crate) next_slot: u32,
}

/// Uniquely identifies a function within an `IrModule`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FunctionId(pub u32);

/// A named, typed parameter of a function.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: IrType,
}

/// An attribute annotation with arguments, e.g. `@adaptive(learning_rate=0.01)`.
#[derive(Debug, Clone)]
pub struct IrAttribute {
    pub name: String,
    pub args: Vec<IrInstr>, // Arguments as IR instructions (constants, etc.)
}

impl PartialEq<str> for IrAttribute {
    fn eq(&self, other: &str) -> bool {
        self.name == other
    }
}

impl PartialEq<String> for IrAttribute {
    fn eq(&self, other: &String) -> bool {
        self.name == *other
    }
}

impl PartialEq for IrAttribute {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

/// Uniquely identifies a lexical drop scope within an [`IrFunction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(pub u32);

/// A single Drop-bearing binding registered inside a [`DropScope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropBinding {
    pub value: ValueId,
    pub var_name: String,
    pub drop_fn: String,
    pub ty: IrType,
}

/// Explicit lexical drop scope tracked on an [`IrFunction`].
#[derive(Debug, Clone)]
pub struct DropScope {
    pub id: ScopeId,
    pub parent: Option<ScopeId>,
    pub bindings: Vec<DropBinding>,
}

/// A compiled function in SSA form.
///
/// Internal representation uses flat `Vec`s indexed by `BlockId`. The entry
/// block is always `blocks[0]`; its block params are the function arguments.
///
/// Post-construction, the function is immutable to callers outside this crate.
/// Passes receive `&mut IrModule` but only mutate through the `pub(crate)` fields.
#[derive(Debug, Clone)]
pub struct IrFunction {
    pub id: FunctionId,
    pub name: String,
    pub params: Vec<Param>,
    pub return_ty: IrType,
    /// Flat list of blocks. `BlockId(n)` indexes `blocks[n]`.
    pub(crate) blocks: Vec<IrBlock>,
    /// Maps `ValueId` → its definition site. Populated during construction.
    pub(crate) value_defs: HashMap<ValueId, ValueDef>,
    /// Maps `ValueId` → its type. Populated during lowering.
    pub(crate) value_types: HashMap<ValueId, IrType>,
    /// Interned type arena for deduplicating `IrType`s in this function.
    pub type_arena: TypeArena,
    /// Maps `ValueId` → interned [`IrTypeId`] in `type_arena`.
    pub(crate) value_type_ids: HashMap<ValueId, IrTypeId>,
    /// Counter for allocating fresh `ValueId`s.
    pub(crate) next_value: u32,
    /// Function attributes, e.g. `@adaptive(learning_rate=0.01)`.
    pub attrs: Vec<IrAttribute>,
    /// Source position table for the debugger: maps `(block_id, instr_idx)` to
    /// the byte offset of the statement that produced the instruction.
    pub span_table: SpanTable,
    /// Source variable names of values bound by `val`/`var` or assigned to a
    /// `var` (each assignment binds the name to a new SSA value). Block
    /// parameters carry their own names. Used for debug information.
    pub value_names: HashMap<ValueId, String>,
    /// Number of leading parameters that are lambda captures (0 for normal fns).
    /// Used by LLVM codegen to emit env-based capture extraction.
    pub capture_count: usize,
    /// Whether this function is `const` — callable at compile time.
    pub is_const: bool,
    /// Source-level place and span metadata for IR NLL borrow/move checking.
    pub place_meta: PlaceMeta,
    /// Explicit lexical drop scopes (`ScopeId(0)` is the function root scope).
    pub drop_scopes: Vec<DropScope>,
    /// Generic call-site type arguments `(block_idx, instr_idx) -> (template_name, type_args)`
    /// resolved lazily by `MonoPass`.
    pub(crate) mono_calls: HashMap<(usize, usize), (String, Vec<IrType>)>,
}

impl IrFunction {
    /// Returns the entry block (always `BlockId(0)`).
    pub fn entry_block(&self) -> &IrBlock {
        &self.blocks[0]
    }

    pub fn block(&self, id: BlockId) -> Option<&IrBlock> {
        self.blocks.get(id.0 as usize)
    }

    pub fn blocks(&self) -> &[IrBlock] {
        &self.blocks
    }

    /// Returns the source byte offset for an instruction in a block, consulting
    /// both the instruction index in the block and any result value it defines.
    pub fn get_instr_span(&self, block_id: u32, instr_idx: usize) -> Option<u32> {
        if let Some(byte) = self.span_table.get(block_id, instr_idx) {
            return Some(byte);
        }
        if let Some(block) = self.block(BlockId(block_id)) {
            if let Some(instr) = block.instrs.get(instr_idx) {
                if let Some(res) = instr.result() {
                    if let Some(byte) = self.span_table.get_for_val(res) {
                        return Some(byte);
                    }
                }
            }
        }
        None
    }

    /// Source position for native debug info. Prefers the per-instruction
    /// `line_spans` (re-attached after optimisation by the debug pipeline);
    /// falls back to statement spans only for modules that never had line
    /// spans, since those index-keyed entries can be stale after passes.
    pub fn debug_span(&self, block_id: u32, instr_idx: usize) -> Option<u32> {
        if self.span_table.line_spans.is_empty() {
            self.get_instr_span(block_id, instr_idx)
        } else {
            self.span_table.get_line(block_id, instr_idx)
        }
    }

    /// Where a runtime error at this instruction is reported, the same on
    /// both backends: the expression that produced its result when it has
    /// one, else the enclosing statement. Both are keyed to survive the
    /// optimisation passes, unlike the index-keyed statement entries.
    pub fn error_span(&self, block_id: u32, instr_idx: usize) -> Option<u32> {
        let instr = self.block(BlockId(block_id))?.instrs.get(instr_idx)?;
        instr
            .result()
            .and_then(|result| self.span_table.get_for_val(result))
            .or_else(|| self.span_table.get_line(block_id, instr_idx))
    }

    /// Returns the type of a value, if known.
    pub fn value_type(&self, v: ValueId) -> Option<&IrType> {
        self.value_types.get(&v)
    }

    /// Returns the definition site of a value.
    pub fn value_def(&self, v: ValueId) -> Option<&ValueDef> {
        self.value_defs.get(&v)
    }

    /// Returns the interned [`IrTypeId`] for `v`, if recorded.
    pub fn value_type_id(&self, v: ValueId) -> Option<IrTypeId> {
        self.value_type_ids.get(&v).copied()
    }

    /// Interns `ty` into this function's [`TypeArena`].
    pub fn intern_type(&mut self, ty: IrType) -> IrTypeId {
        self.type_arena.intern(ty)
    }

    /// Resolves an interned [`IrTypeId`] from this function's [`TypeArena`].
    pub fn resolve_type(&self, id: IrTypeId) -> Option<&IrType> {
        self.type_arena.get(id)
    }

    /// Allocates a fresh `ValueId`. Used by the builder only.
    pub(crate) fn fresh_value(&mut self) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        id
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::block::BlockId;
    use crate::ir::types::{DType, IrType};

    // -- SpanTable ------------------------------------------------------------

    #[test]
    fn span_table_empty() {
        let st = SpanTable::default();
        assert!(st.is_empty());
        assert_eq!(st.len(), 0);
    }

    #[test]
    fn span_table_insert_and_get() {
        let mut st = SpanTable::default();
        st.entries.insert((0, 0), 42);
        assert!(!st.is_empty());
        assert_eq!(st.len(), 1);
        assert_eq!(st.get(0, 0), Some(42));
    }

    #[test]
    fn span_table_get_missing() {
        let st = SpanTable::default();
        assert_eq!(st.get(0, 0), None);
    }

    #[test]
    fn span_table_multiple_entries() {
        let mut st = SpanTable::default();
        st.entries.insert((0, 0), 10);
        st.entries.insert((0, 1), 20);
        st.entries.insert((1, 0), 30);
        assert_eq!(st.len(), 3);
        assert_eq!(st.get(0, 0), Some(10));
        assert_eq!(st.get(0, 1), Some(20));
        assert_eq!(st.get(1, 0), Some(30));
    }

    // -- FunctionId -----------------------------------------------------------

    #[test]
    fn function_id_equality() {
        assert_eq!(FunctionId(0), FunctionId(0));
        assert_ne!(FunctionId(0), FunctionId(1));
    }

    #[test]
    fn function_id_ordering() {
        assert!(FunctionId(0) < FunctionId(1));
    }

    // -- IrFunction -----------------------------------------------------------

    fn make_test_func() -> IrFunction {
        use crate::ir::module::IrFunctionBuilder;
        let params = vec![Param {
            name: "x".into(),
            ty: IrType::Scalar(DType::I64),
        }];
        let mut builder = IrFunctionBuilder::new("test_fn", params, IrType::Scalar(DType::I64));
        let entry = builder.create_block(Some("entry"));
        builder.set_current_block(entry);
        let _param_v = builder.add_block_param(entry, Some("x"), IrType::Scalar(DType::I64));
        let const_v = builder.fresh_value();
        builder.push_instr(
            crate::ir::instr::IrInstr::ConstInt {
                result: const_v,
                value: 42,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );
        builder.push_instr(
            crate::ir::instr::IrInstr::Return {
                values: vec![const_v],
            },
            None,
        );
        builder.build()
    }

    #[test]
    fn function_entry_block() {
        let f = make_test_func();
        assert_eq!(f.entry_block().id, BlockId(0));
    }

    #[test]
    fn function_block_by_id() {
        let f = make_test_func();
        assert!(f.block(BlockId(0)).is_some());
        assert!(f.block(BlockId(99)).is_none());
    }

    #[test]
    fn function_blocks_count() {
        let f = make_test_func();
        assert_eq!(f.blocks().len(), 1);
    }

    #[test]
    fn function_value_type() {
        let f = make_test_func();
        // The block param and const should have types
        let has_typed = f
            .value_types
            .values()
            .any(|t| matches!(t, IrType::Scalar(DType::I64)));
        assert!(has_typed);
    }

    #[test]
    fn function_fresh_value_increments() {
        let mut f = make_test_func();
        let before = f.next_value;
        let v = f.fresh_value();
        assert_eq!(v.0, before);
        assert_eq!(f.next_value, before + 1);
    }

    #[test]
    fn function_capture_count_default() {
        let f = make_test_func();
        assert_eq!(f.capture_count, 0);
    }
}
