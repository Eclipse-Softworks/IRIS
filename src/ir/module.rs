use std::collections::{HashMap, HashSet};

use crate::ir::block::{BlockId, IrBlock};
use crate::ir::function::{
    DropBinding, DropScope, FunctionId, IrFunction, Param, PlaceEvent, PlaceEventKind, PlaceMeta,
    PlaceSlot, PlaceSpan, ScopeId, SpanTable,
};
use crate::ir::instr::{InstrId, IrInstr};
use crate::ir::types::{IrType, IrTypeId, TraitMethodSig, TypeArena};
use crate::ir::value::{BlockParam, ValueDef, ValueId};
use crate::parser::ast::{AstExpr, AstStructDef};

/// The top-level IR container.
///
/// Invariants:
/// - Function names are unique within a module.
/// - `FunctionId(n)` always indexes `functions[n]`.
/// - Once a function is added via `add_function()`, it is immutable to external
///   callers. Passes may mutate through the `pub(crate)` fields.
///
/// An extern function declaration (C-linkage FFI).
#[derive(Debug, Clone)]
pub struct IrExternFn {
    pub name: String,
    pub param_types: Vec<IrType>,
    pub ret_ty: IrType,
    pub abi: Option<String>,      // e.g. Some("C") for C calling convention
    pub link_lib: Option<String>, // e.g. Some("m") for -lm
}

/// A polymorphic IR function template awaiting lazy post-IR monomorphisation.
#[derive(Debug, Clone)]
pub struct GenericIrFn {
    pub type_params: Vec<String>,
    pub hkt_params: Vec<String>,
    pub func: IrFunction,
}

/// A generic trait declaration (`trait Container[T]`, `trait Mappable[F[_]]`).
#[derive(Debug, Clone)]
pub struct GenericTraitDef {
    pub type_params: Vec<String>,
    pub hkt_params: Vec<String>,
    pub methods: Vec<TraitMethodSig>,
}

#[derive(Debug, Default, Clone)]
pub struct IrModule {
    pub name: String,
    pub(crate) functions: Vec<IrFunction>,
    pub(crate) function_index: HashMap<String, FunctionId>,
    /// Polymorphic IR function templates keyed by template name, instantiated
    /// lazily by [`crate::pass::mono::MonoPass`].
    pub(crate) generic_templates: HashMap<String, GenericIrFn>,
    /// Struct type definitions: name → ordered list of (field_name, field_type).
    pub(crate) struct_defs: HashMap<String, Vec<(String, IrType)>>,
    /// Enum type definitions: name → (variant_names, variant_field_types).
    /// `variant_field_types[i]` is the list of payload types for variant `i`.
    pub(crate) enum_defs: HashMap<String, (Vec<String>, Vec<Vec<IrType>>)>,
    /// Type alias definitions: alias name → concrete IrType.
    pub(crate) type_aliases: HashMap<String, IrType>,
    /// Extern function declarations: name → signature.
    pub extern_fns: Vec<IrExternFn>,
    /// Trait definitions: trait_name → list of method signatures (no `self`).
    /// Used to resolve `dyn Trait` IR types and to lay out vtables at codegen.
    pub(crate) trait_defs: HashMap<String, Vec<TraitMethodSig>>,
    /// Generic trait declarations: trait_name → type/HKT params and method signatures.
    pub(crate) generic_trait_defs: HashMap<String, GenericTraitDef>,
    /// Impl method mangled names: trait_name → (concrete_struct_name, method_name, mangled_fn).
    /// Populated when `impl Trait for Type` is processed so codegen can build
    /// the per-concrete-type vtable global.
    pub(crate) trait_impl_methods: HashMap<String, Vec<(String, String, String)>>,
    /// Default values for struct fields: struct_name → Vec<default_expr_or_none>.
    /// Indexed in parallel with `struct_defs[name]`.
    pub(crate) struct_defaults: HashMap<String, Vec<Option<AstExpr>>>,
    /// Names of generic struct templates that require monomorphization.
    pub(crate) generic_struct_names: HashSet<String>,
    /// AST definitions for generic structs so dependent array lengths can be
    /// re-evaluated under concrete const-generic substitutions.
    pub(crate) generic_struct_asts: HashMap<String, AstStructDef>,
    /// Evaluated top-level integer constants (`const K: usize = 4`), used when resolving
    /// array lengths and const-generic type arguments.
    pub(crate) const_ints: HashMap<String, i64>,
    /// Program state (`var` at the top level), in declaration order; a
    /// variable's slot is its index.
    pub(crate) state_vars: Vec<(String, IrType)>,
    /// The file this module was compiled from, with the byte offset of each
    /// line, so native code can report a runtime error at `file:line:col`.
    pub source: Option<std::sync::Arc<SourceLines>>,
    /// Module-level interned type arena for deduplicating `IrType`s.
    pub types: TypeArena,
}

/// The program's source files, for reporting a native runtime error at
/// `file:line:col`. Offsets are the compile's: each file at its own base
/// (see `compiler::SourceMap`), the main file first, at 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLines {
    pub files: Vec<SourceFileLines>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFileLines {
    /// As messages show it: relative to the working directory.
    pub name: String,
    pub base: u32,
    pub len: u32,
    /// Offsets within the file of each line's first byte.
    pub line_starts: Vec<u32>,
    /// Errors inside this file are reported in it. Not the standard
    /// library's: a failure there is reported at the line that called it.
    pub reported: bool,
}

impl SourceFileLines {
    pub fn new(name: impl Into<String>, base: u32, text: &str, reported: bool) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        SourceFileLines {
            name: name.into(),
            base,
            len: text.len() as u32,
            line_starts,
            reported,
        }
    }
}

impl SourceLines {
    /// One file, at base 0.
    pub fn new(name: impl Into<String>, text: &str) -> Self {
        SourceLines {
            files: vec![SourceFileLines::new(name, 0, text, true)],
        }
    }

    pub fn from_files(files: Vec<SourceFileLines>) -> Self {
        SourceLines { files }
    }

    /// The file holding `byte`, its index, line and column (1-based), if
    /// errors there are reported.
    pub fn locate(&self, byte: u32) -> Option<(usize, u32, u32)> {
        let index = self.files.iter().rposition(|f| f.base <= byte)?;
        let file = &self.files[index];
        let offset = byte - file.base;
        if offset > file.len || !file.reported {
            return None;
        }
        let line = match file.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        Some((index, line as u32 + 1, offset - file.line_starts[line] + 1))
    }

    /// The byte offset of 1-based `line` and `col` in the file named `name`
    /// -- the inverse of [`locate`](Self::locate).
    pub fn offset(&self, name: &str, line: u32, col: u32) -> Option<u32> {
        let file = self.files.iter().find(|f| f.name == name)?;
        let start = *file.line_starts.get(line.checked_sub(1)? as usize)?;
        Some(file.base + start + col.saturating_sub(1))
    }
}

/// The generated function that gives every program state variable its
/// initial value. Hosts call it once before calling into the program.
pub const STATE_INIT_FN: &str = "__iris_state_init";

impl IrModule {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            functions: Vec::new(),
            function_index: HashMap::new(),
            generic_templates: HashMap::new(),
            struct_defs: HashMap::new(),
            enum_defs: HashMap::new(),
            type_aliases: HashMap::new(),
            extern_fns: Vec::new(),
            trait_defs: HashMap::new(),
            generic_trait_defs: HashMap::new(),
            trait_impl_methods: HashMap::new(),
            struct_defaults: HashMap::new(),
            generic_struct_names: HashSet::new(),
            generic_struct_asts: HashMap::new(),
            const_ints: HashMap::new(),
            state_vars: Vec::new(),
            source: None,
            types: TypeArena::new(),
        }
    }

    /// Interns `ty` in the module's [`TypeArena`], returning a compact [`IrTypeId`].
    pub fn intern_type(&mut self, ty: IrType) -> IrTypeId {
        self.types.intern(ty)
    }

    /// Resolves an [`IrTypeId`] from the module's [`TypeArena`].
    pub fn resolve_type(&self, id: IrTypeId) -> Option<&IrType> {
        self.types.get(id)
    }

    /// Program state variables, in declaration order (slot = index).
    pub fn state_vars(&self) -> &[(String, IrType)] {
        &self.state_vars
    }

    /// The slot and type of program state variable `name`.
    pub fn state_slot(&self, name: &str) -> Option<(usize, &IrType)> {
        self.state_vars
            .iter()
            .position(|(var, _)| var == name)
            .map(|slot| (slot, &self.state_vars[slot].1))
    }

    /// Registers a struct definition. Returns `Err` if the name already exists.
    pub fn add_struct_def(
        &mut self,
        name: impl Into<String>,
        fields: Vec<(String, IrType)>,
    ) -> Result<(), String> {
        let name = name.into();
        if self.struct_defs.contains_key(&name) {
            return Err(format!("struct '{}' already defined", name));
        }
        self.struct_defs.insert(name, fields);
        Ok(())
    }

    /// Looks up a struct definition by name.
    pub fn struct_def(&self, name: &str) -> Option<&Vec<(String, IrType)>> {
        self.struct_defs.get(name)
    }

    /// Looks up default values for struct fields by name.
    pub fn struct_defaults(&self, name: &str) -> Option<&Vec<Option<AstExpr>>> {
        self.struct_defaults.get(name)
    }

    /// Registers an enum definition. Returns `Err` if the name already exists.
    pub fn add_enum_def(
        &mut self,
        name: impl Into<String>,
        variants: Vec<String>,
        variant_fields: Vec<Vec<IrType>>,
    ) -> Result<(), String> {
        let name = name.into();
        if self.enum_defs.contains_key(&name) {
            return Err(format!("enum '{}' already defined", name));
        }
        self.enum_defs.insert(name, (variants, variant_fields));
        Ok(())
    }

    /// Looks up an enum definition by name (returns variant names).
    pub fn enum_def(&self, name: &str) -> Option<&Vec<String>> {
        self.enum_defs.get(name).map(|(variants, _)| variants)
    }

    /// Looks up the payload field types for each variant of an enum.
    pub fn enum_variant_fields(&self, name: &str) -> Option<&Vec<Vec<IrType>>> {
        self.enum_defs.get(name).map(|(_, fields)| fields)
    }

    /// Registers a type alias. Returns `Err` if the name already exists.
    pub fn add_type_alias(&mut self, name: impl Into<String>, ty: IrType) -> Result<(), String> {
        let name = name.into();
        if self.type_aliases.contains_key(&name) {
            return Err(format!("type alias '{}' already defined", name));
        }
        self.type_aliases.insert(name, ty);
        Ok(())
    }

    /// Looks up a type alias by name.
    pub fn type_alias(&self, name: &str) -> Option<&IrType> {
        self.type_aliases.get(name)
    }

    /// Registers a trait definition (method signatures only — bodies live in impl fns).
    pub fn add_trait_def(&mut self, name: impl Into<String>, methods: Vec<TraitMethodSig>) {
        self.trait_defs.insert(name.into(), methods);
    }

    /// Returns the trait definition (ordered method signatures) for `name`.
    pub fn trait_def(&self, name: &str) -> Option<&Vec<TraitMethodSig>> {
        self.trait_defs.get(name)
    }

    /// All trait definitions. Used at codegen to lay out vtables.
    pub fn trait_defs(&self) -> &HashMap<String, Vec<TraitMethodSig>> {
        &self.trait_defs
    }

    /// Records an impl method entry: `(concrete_struct, method_name, mangled_fn)`.
    pub fn add_trait_impl_method(
        &mut self,
        trait_name: impl Into<String>,
        concrete: impl Into<String>,
        method: impl Into<String>,
        mangled: impl Into<String>,
    ) {
        self.trait_impl_methods
            .entry(trait_name.into())
            .or_default()
            .push((concrete.into(), method.into(), mangled.into()));
    }

    /// All impl-method entries vtable-table for codegen consumption.
    pub fn trait_impl_methods(&self) -> &HashMap<String, Vec<(String, String, String)>> {
        &self.trait_impl_methods
    }

    /// Returns the mangled destructor function name for a type if it implements `Drop`.
    pub fn find_drop_method(&self, type_name: &str) -> Option<String> {
        if let Some(methods) = self.trait_impl_methods.get("Drop") {
            for (concrete, method, mangled) in methods {
                if concrete == type_name && (method == "drop" || method == "destroy") {
                    return Some(mangled.clone());
                }
            }
        }
        let direct_name = format!("Drop__{}__drop", type_name);
        if self.function_index.contains_key(&direct_name) {
            return Some(direct_name);
        }
        let direct_name2 = format!("{}__drop", type_name);
        if self.function_index.contains_key(&direct_name2) {
            return Some(direct_name2);
        }
        None
    }

    pub fn function(&self, id: FunctionId) -> Option<&IrFunction> {
        self.functions.get(id.0 as usize)
    }

    pub fn function_by_name(&self, name: &str) -> Option<&IrFunction> {
        let id = self.function_index.get(name)?;
        self.functions.get(id.0 as usize)
    }

    pub fn functions(&self) -> &[IrFunction] {
        &self.functions
    }

    /// Seals and registers a function built by `IrFunctionBuilder`.
    /// Returns `Err` if the name is already taken.
    pub fn add_function(&mut self, mut func: IrFunction) -> Result<FunctionId, String> {
        if self.function_index.contains_key(&func.name) {
            return Err(format!("function '{}' already defined", func.name));
        }
        let id = FunctionId(self.functions.len() as u32);
        func.id = id;
        self.function_index.insert(func.name.clone(), id);
        self.functions.push(func);
        Ok(id)
    }

    /// Registers a polymorphic IR function template for lazy post-IR monomorphisation.
    pub fn add_generic_template(
        &mut self,
        name: impl Into<String>,
        type_params: Vec<String>,
        func: IrFunction,
    ) {
        self.generic_templates.insert(
            name.into(),
            GenericIrFn {
                type_params,
                hkt_params: Vec::new(),
                func,
            },
        );
    }

    /// Registers a polymorphic IR function template with HKT type-constructor parameters.
    pub fn add_generic_template_with_hkt(
        &mut self,
        name: impl Into<String>,
        type_params: Vec<String>,
        hkt_params: Vec<String>,
        func: IrFunction,
    ) {
        self.generic_templates.insert(
            name.into(),
            GenericIrFn {
                type_params,
                hkt_params,
                func,
            },
        );
    }

    /// Registers a generic trait declaration (`trait Container[T]`, `trait Mappable[F[_]]`).
    pub fn add_generic_trait_def(
        &mut self,
        name: impl Into<String>,
        type_params: Vec<String>,
        hkt_params: Vec<String>,
        methods: Vec<TraitMethodSig>,
    ) {
        self.generic_trait_defs.insert(
            name.into(),
            GenericTraitDef {
                type_params,
                hkt_params,
                methods,
            },
        );
    }

    /// Generic trait declarations keyed by trait name.
    pub fn generic_trait_defs(&self) -> &HashMap<String, GenericTraitDef> {
        &self.generic_trait_defs
    }
}

/// Builder for constructing an `IrFunction` incrementally.
///
/// Call order:
/// 1. `create_block()` — allocate one or more blocks
/// 2. `add_block_param()` — add typed params to each block
/// 3. `set_current_block()` — point the cursor at a block
/// 4. `push_instr()` — emit instructions into the current block
/// 5. `build()` — consume the builder and return the completed `IrFunction`
///
/// `build()` panics in debug builds if any block lacks a terminator.
pub struct IrFunctionBuilder {
    func: IrFunction,
    current_block: Option<BlockId>,
    /// Source byte offset of the current statement being lowered. Set by the
    /// lowerer before emitting instructions; recorded into `span_table`.
    current_span: Option<u32>,
    /// Sticky counterpart of `current_span`: stays set until the next
    /// statement, so every instruction gets a `line_spans` entry.
    line_span: Option<u32>,
    /// Made by `new_return_decided_by_body`: `build_returning` gives the
    /// return type, and `build` is not allowed.
    return_pending: bool,
}

impl IrFunctionBuilder {
    pub fn new(name: impl Into<String>, params: Vec<Param>, return_ty: IrType) -> Self {
        let mut type_arena = TypeArena::new();
        for p in &params {
            type_arena.intern_ref(&p.ty);
        }
        type_arena.intern_ref(&return_ty);
        let func = IrFunction {
            id: FunctionId(0), // reassigned by IrModule::add_function
            name: name.into(),
            params,
            return_ty,
            blocks: Vec::new(),
            value_defs: HashMap::new(),
            value_types: HashMap::new(),
            type_arena,
            value_type_ids: HashMap::new(),
            next_value: 0,
            attrs: Vec::new(),
            span_table: SpanTable::default(),
            value_names: HashMap::new(),
            capture_count: 0,
            is_const: false,
            place_meta: PlaceMeta::default(),
            drop_scopes: Vec::new(),
            mono_calls: HashMap::new(),
        };
        Self {
            func,
            current_block: None,
            current_span: None,
            line_span: None,
            return_pending: false,
        }
    }

    /// Allocates a new explicit [`DropScope`] with optional `parent`.
    pub fn alloc_drop_scope(&mut self, parent: Option<ScopeId>) -> ScopeId {
        let id = ScopeId(self.func.drop_scopes.len() as u32);
        self.func.drop_scopes.push(DropScope {
            id,
            parent,
            bindings: Vec::new(),
        });
        id
    }

    /// Registers a Drop-bearing binding inside `scope`.
    pub fn register_drop_binding(&mut self, scope: ScopeId, binding: DropBinding) {
        if let Some(s) = self.func.drop_scopes.get_mut(scope.0 as usize) {
            s.bindings.push(binding);
        }
    }

    /// A builder for a function whose body decides its return type (a
    /// lambda, an effect handler): lower the body, then `build_returning`
    /// with its type. IR types have no "not yet known" to start from.
    pub fn new_return_decided_by_body(name: impl Into<String>, params: Vec<Param>) -> Self {
        let mut b = Self::new(name, params, IrType::Tuple(Vec::new()));
        b.return_pending = true;
        b
    }

    /// `build`, giving the return type a `new_return_decided_by_body`
    /// builder left open.
    pub fn build_returning(mut self, return_ty: IrType) -> IrFunction {
        self.func.return_ty = return_ty;
        self.return_pending = false;
        self.build()
    }

    pub fn func_name(&self) -> &str {
        &self.func.name
    }

    /// Records where `value` came from, unless a more specific (inner)
    /// expression already did.
    pub fn note_value_span(&mut self, value: ValueId, byte: u32) {
        self.func
            .span_table
            .value_spans
            .entry(value)
            .or_insert(byte);
    }

    /// Records the source byte offset of the statement currently being lowered.
    ///
    /// Call this before `push_instr` so that the instruction is associated with
    /// the correct source position in `span_table`. The span is cleared after
    /// the first instruction it is attached to.
    pub fn set_span_byte(&mut self, byte: u32) {
        self.current_span = Some(byte);
        self.line_span = Some(byte);
    }

    /// Records that `value` holds the source variable `name` from here on.
    pub fn name_value(&mut self, value: ValueId, name: &str) {
        self.func.value_names.insert(value, name.to_owned());
    }

    /// Allocates a fresh source binding `PlaceSlot` for `name` and optionally
    /// associates `val` with it when `val` does not already carry another slot.
    pub fn alloc_place_slot(&mut self, name: &str, val: Option<ValueId>) -> PlaceSlot {
        let slot = PlaceSlot(self.func.place_meta.next_slot);
        self.func.place_meta.next_slot += 1;
        self.func
            .place_meta
            .slot_names
            .insert(slot, name.to_owned());
        if let Some(v) = val {
            self.func.place_meta.value_slots.entry(v).or_insert(slot);
        }
        slot
    }

    /// Associates `val` with `slot` if `val` has no slot yet (e.g. a newly created
    /// `BlockParam` or assignment RHS result).
    pub fn map_value_slot_if_unset(&mut self, val: ValueId, slot: PlaceSlot) {
        self.func.place_meta.value_slots.entry(val).or_insert(slot);
    }

    /// Records a full `(start, end)` span for `val` when not already present.
    pub fn note_value_full_span(&mut self, val: ValueId, start: u32, end: u32) {
        self.func
            .place_meta
            .value_spans
            .entry(val)
            .or_insert(PlaceSpan { start, end });
    }

    /// Records a source place event at the current insertion point `(block_id, next_instr_idx)`.
    pub fn record_place_event(&mut self, kind: PlaceEventKind, start: u32, end: u32) {
        let Some(block_id) = self.current_block else {
            return;
        };
        let instr_idx = self.func.blocks[block_id.0 as usize].instrs.len();
        self.func
            .place_meta
            .events
            .entry((block_id.0, instr_idx))
            .or_default()
            .push(PlaceEvent {
                kind,
                span: PlaceSpan { start, end },
            });
    }

    /// Sets only the sticky line position (see `SpanTable::line_spans`), for
    /// code such as a block's tail expression that is not a statement and so
    /// does not create an interpreter stepping point.
    pub fn set_line_span(&mut self, byte: u32) {
        self.line_span = Some(byte);
    }

    /// Creates a new block and returns its `BlockId`.
    pub fn create_block(&mut self, name: Option<&str>) -> BlockId {
        let id = BlockId(self.func.blocks.len() as u32);
        self.func
            .blocks
            .push(IrBlock::new(id, name.map(str::to_owned)));
        id
    }

    /// Adds a typed parameter to a block. Returns the `ValueId` of the new param.
    pub fn add_block_param(&mut self, block: BlockId, name: Option<&str>, ty: IrType) -> ValueId {
        let value_id = self.func.fresh_value();
        let param = BlockParam {
            id: value_id,
            ty: ty.clone(),
            name: name.map(str::to_owned),
        };
        self.func.blocks[block.0 as usize].params.push(param);
        self.func
            .value_defs
            .insert(value_id, ValueDef::BlockParam { block });
        let tid = self.func.type_arena.intern_ref(&ty);
        self.func.value_type_ids.insert(value_id, tid);
        self.func.value_types.insert(value_id, ty);
        value_id
    }

    /// Returns the current insertion block.
    pub fn current_block(&self) -> BlockId {
        self.current_block
            .expect("IrFunctionBuilder: no current block set")
    }

    /// Sets the current insertion block.
    pub fn set_current_block(&mut self, block: BlockId) {
        self.current_block = Some(block);
    }

    /// Records a generic call specialization `(template_name, type_args)` at
    /// `(current_block, next_instr_idx)` and appends the `IrInstr::Call`.
    pub fn push_generic_call(
        &mut self,
        template_name: impl Into<String>,
        type_args: Vec<IrType>,
        instr: IrInstr,
        result_ty: Option<IrType>,
    ) -> Option<ValueId> {
        let block_id = self.current_block();
        let instr_idx = self.func.blocks[block_id.0 as usize].instrs.len();
        self.func.mono_calls.insert(
            (block_id.0 as usize, instr_idx),
            (template_name.into(), type_args),
        );
        self.push_instr(instr, result_ty)
    }

    /// Appends an instruction to the current block.
    ///
    /// `result_ty` is the type of the instruction's result value, if any.
    /// It must be `Some` iff the instruction produces a result.
    ///
    /// Panics in debug builds if the current block is already sealed.
    pub fn push_instr(&mut self, instr: IrInstr, result_ty: Option<IrType>) -> Option<ValueId> {
        let block_id = self
            .current_block
            .expect("IrFunctionBuilder: no current block set before push_instr");

        let block = &self.func.blocks[block_id.0 as usize];
        debug_assert!(
            !block.is_sealed(),
            "push_instr called on already-sealed block {:?}",
            block_id
        );

        let result = instr.result();

        let instr_idx = self.func.blocks[block_id.0 as usize].instrs.len();

        if let (Some(result_id), Some(ty)) = (result, result_ty) {
            let instr_id = InstrId(instr_idx as u32);
            self.func.value_defs.insert(
                result_id,
                ValueDef::InstrResult {
                    block: block_id,
                    instr: instr_id,
                },
            );
            let tid = self.func.type_arena.intern_ref(&ty);
            self.func.value_type_ids.insert(result_id, tid);
            self.func.value_types.insert(result_id, ty);
        }

        if let Some(byte) = self.line_span {
            self.func
                .span_table
                .line_spans
                .insert((block_id.0, instr_idx), byte);
        }

        // Record the current span into the span table (first instruction per statement).
        if let Some(byte) = self.current_span.take() {
            self.func
                .span_table
                .entries
                .insert((block_id.0, instr_idx), byte);
            // A value's own position is its expression's, which the
            // lowerer notes (`note_value_span`); the statement's would win
            // here, and point every error at the statement's first word.
        }

        self.func.blocks[block_id.0 as usize].instrs.push(instr);
        result
    }

    /// Returns the first block param of the current block, if any.
    /// Useful when a block has no tail expression but has an incoming value
    /// (e.g. from `?` unwrapping).
    pub fn current_block_first_param(&self) -> Option<(ValueId, IrType)> {
        let block_id = self.current_block?;
        self.func.blocks[block_id.0 as usize]
            .params
            .first()
            .map(|p| (p.id, p.ty.clone()))
    }

    /// Returns true if the current block already ends with a terminator.
    /// Appends an argument to the `Br` terminating `block`, when it targets
    /// `target`. Returns whether it did.
    ///
    /// Needed because a branch's arguments are emitted before its sibling has
    /// been lowered, so whether a value carries an autodiff tape handle is not
    /// known until both arms exist. See known-issues #50.
    pub(crate) fn append_br_arg(&mut self, block: BlockId, target: BlockId, arg: ValueId) -> bool {
        let Some(b) = self.func.blocks.get_mut(block.0 as usize) else {
            return false;
        };
        match b.instrs.last_mut() {
            Some(IrInstr::Br { target: t, args }) if *t == target => {
                args.push(arg);
                true
            }
            _ => false,
        }
    }

    pub fn is_current_block_terminated(&self) -> bool {
        if let Some(block_id) = self.current_block {
            self.func.blocks[block_id.0 as usize].is_sealed()
        } else {
            false
        }
    }

    /// Allocates a fresh `ValueId` without attaching it to any instruction.
    /// Used by the lowerer when pre-allocating result values.
    pub fn fresh_value(&mut self) -> ValueId {
        self.func.fresh_value()
    }

    /// Emits a `ConstStr` instruction and returns the result `ValueId`.
    pub fn emit_const_str(&mut self, value: String) -> ValueId {
        let result = self.func.fresh_value();
        self.push_instr(
            crate::ir::instr::IrInstr::ConstStr { result, value },
            Some(crate::ir::types::IrType::Str),
        );
        result
    }

    /// Emits a `StrLen` instruction and returns the result `ValueId`.
    pub fn emit_str_len(&mut self, operand: ValueId) -> ValueId {
        let result = self.func.fresh_value();
        let ty = crate::ir::types::IrType::Scalar(crate::ir::types::DType::I64);
        self.push_instr(
            crate::ir::instr::IrInstr::StrLen { result, operand },
            Some(ty),
        );
        result
    }

    /// Emits a `StrConcat` instruction and returns the result `ValueId`.
    pub fn emit_str_concat(&mut self, lhs: ValueId, rhs: ValueId) -> ValueId {
        let result = self.func.fresh_value();
        self.push_instr(
            crate::ir::instr::IrInstr::StrConcat { result, lhs, rhs },
            Some(crate::ir::types::IrType::Str),
        );
        result
    }

    /// Emits a `Print` instruction (no result).
    pub fn emit_print(&mut self, operand: ValueId) {
        self.push_instr(crate::ir::instr::IrInstr::Print { operand }, None);
    }

    /// Terminates any unsealed blocks with `Return { values: [] }`.
    /// Call this before `build()` if early-return paths may leave orphan blocks.
    pub fn seal_unterminated_blocks(&mut self) {
        use crate::ir::instr::IrInstr;
        let block_ids: Vec<crate::ir::block::BlockId> =
            self.func.blocks.iter().map(|b| b.id).collect();
        for bid in block_ids {
            if !self.func.blocks[bid.0 as usize].is_sealed() {
                self.func.blocks[bid.0 as usize]
                    .instrs
                    .push(IrInstr::Return { values: vec![] });
            }
        }
    }

    /// Consumes the builder and returns the completed `IrFunction`.
    ///
    /// Panics in debug builds if any block is not sealed (lacks a terminator).
    pub fn build(self) -> IrFunction {
        assert!(
            !self.return_pending,
            "a builder whose body decides the return type is finished with build_returning"
        );
        #[cfg(debug_assertions)]
        for block in &self.func.blocks {
            assert!(
                block.is_sealed(),
                "build() called with unsealed block {:?} ('{:?}')",
                block.id,
                block.name
            );
        }
        self.func
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{DType, IrType};

    // -- IrModule basics ------------------------------------------------------

    #[test]
    fn module_new_empty() {
        let m = IrModule::new("test");
        assert_eq!(m.name, "test");
        assert!(m.functions().is_empty());
    }

    #[test]
    fn module_default() {
        let m = IrModule::default();
        assert_eq!(m.name, "");
        assert!(m.functions().is_empty());
    }

    // -- Struct definitions ---------------------------------------------------

    #[test]
    fn add_struct_def_ok() {
        let mut m = IrModule::new("test");
        let fields = vec![
            ("x".into(), IrType::Scalar(DType::F64)),
            ("y".into(), IrType::Scalar(DType::F64)),
        ];
        assert!(m.add_struct_def("Point", fields).is_ok());
    }

    #[test]
    fn add_struct_def_duplicate() {
        let mut m = IrModule::new("test");
        m.add_struct_def("Point", vec![]).unwrap();
        assert!(m.add_struct_def("Point", vec![]).is_err());
    }

    #[test]
    fn struct_def_lookup() {
        let mut m = IrModule::new("test");
        let fields = vec![("x".into(), IrType::Scalar(DType::I64))];
        m.add_struct_def("Foo", fields.clone()).unwrap();
        assert_eq!(m.struct_def("Foo").unwrap().len(), 1);
        assert!(m.struct_def("Bar").is_none());
    }

    // -- Enum definitions -----------------------------------------------------

    #[test]
    fn add_enum_def_ok() {
        let mut m = IrModule::new("test");
        let variants = vec!["Some".into(), "None".into()];
        let fields = vec![vec![IrType::Scalar(DType::I64)], vec![]];
        assert!(m.add_enum_def("Option", variants, fields).is_ok());
    }

    #[test]
    fn add_enum_def_duplicate() {
        let mut m = IrModule::new("test");
        m.add_enum_def("Color", vec!["Red".into()], vec![vec![]])
            .unwrap();
        assert!(m
            .add_enum_def("Color", vec!["Blue".into()], vec![vec![]])
            .is_err());
    }

    #[test]
    fn enum_def_lookup() {
        let mut m = IrModule::new("test");
        m.add_enum_def(
            "Color",
            vec!["Red".into(), "Blue".into()],
            vec![vec![], vec![]],
        )
        .unwrap();
        let variants = m.enum_def("Color").unwrap();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0], "Red");
    }

    #[test]
    fn enum_variant_fields_lookup() {
        let mut m = IrModule::new("test");
        m.add_enum_def(
            "Result",
            vec!["Ok".into(), "Err".into()],
            vec![vec![IrType::Scalar(DType::I64)], vec![IrType::Str]],
        )
        .unwrap();
        let fields = m.enum_variant_fields("Result").unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].len(), 1);
    }

    // -- Type aliases ---------------------------------------------------------

    #[test]
    fn add_type_alias_ok() {
        let mut m = IrModule::new("test");
        assert!(m
            .add_type_alias("MyInt", IrType::Scalar(DType::I64))
            .is_ok());
    }

    #[test]
    fn add_type_alias_duplicate() {
        let mut m = IrModule::new("test");
        m.add_type_alias("MyInt", IrType::Scalar(DType::I64))
            .unwrap();
        assert!(m
            .add_type_alias("MyInt", IrType::Scalar(DType::I32))
            .is_err());
    }

    #[test]
    fn type_alias_lookup() {
        let mut m = IrModule::new("test");
        m.add_type_alias("S", IrType::Str).unwrap();
        assert_eq!(*m.type_alias("S").unwrap(), IrType::Str);
        assert!(m.type_alias("T").is_none());
    }

    // -- Function management --------------------------------------------------

    fn make_simple_func(name: &str) -> IrFunction {
        let mut builder = IrFunctionBuilder::new(name, vec![], IrType::Scalar(DType::I64));
        let entry = builder.create_block(Some("entry"));
        builder.set_current_block(entry);
        builder.push_instr(crate::ir::instr::IrInstr::Return { values: vec![] }, None);
        builder.build()
    }

    #[test]
    fn add_function_ok() {
        let mut m = IrModule::new("test");
        let f = make_simple_func("main");
        let id = m.add_function(f).unwrap();
        assert_eq!(id.0, 0);
    }

    #[test]
    fn add_function_duplicate() {
        let mut m = IrModule::new("test");
        m.add_function(make_simple_func("main")).unwrap();
        assert!(m.add_function(make_simple_func("main")).is_err());
    }

    #[test]
    fn function_by_id() {
        let mut m = IrModule::new("test");
        let id = m.add_function(make_simple_func("foo")).unwrap();
        assert_eq!(m.function(id).unwrap().name, "foo");
    }

    #[test]
    fn function_by_name() {
        let mut m = IrModule::new("test");
        m.add_function(make_simple_func("bar")).unwrap();
        assert_eq!(m.function_by_name("bar").unwrap().name, "bar");
        assert!(m.function_by_name("baz").is_none());
    }

    #[test]
    fn functions_list() {
        let mut m = IrModule::new("test");
        m.add_function(make_simple_func("a")).unwrap();
        m.add_function(make_simple_func("b")).unwrap();
        assert_eq!(m.functions().len(), 2);
    }

    // -- IrFunctionBuilder ----------------------------------------------------

    #[test]
    fn builder_create_block() {
        let mut builder = IrFunctionBuilder::new("test", vec![], IrType::Scalar(DType::I64));
        let b0 = builder.create_block(Some("entry"));
        let b1 = builder.create_block(Some("body"));
        assert_eq!(b0.0, 0);
        assert_eq!(b1.0, 1);
    }

    #[test]
    fn builder_add_block_param() {
        let mut builder = IrFunctionBuilder::new("test", vec![], IrType::Scalar(DType::I64));
        let b0 = builder.create_block(Some("entry"));
        let v = builder.add_block_param(b0, Some("x"), IrType::Scalar(DType::I64));
        assert_eq!(v.0, 0);
    }

    #[test]
    fn builder_push_instr_and_build() {
        let mut builder = IrFunctionBuilder::new("test", vec![], IrType::Scalar(DType::I64));
        let b0 = builder.create_block(Some("entry"));
        builder.set_current_block(b0);
        builder.push_instr(crate::ir::instr::IrInstr::Return { values: vec![] }, None);
        let func = builder.build();
        assert_eq!(func.blocks().len(), 1);
        assert!(func.entry_block().is_sealed());
    }

    #[test]
    fn builder_is_current_block_terminated() {
        let mut builder = IrFunctionBuilder::new("test", vec![], IrType::Scalar(DType::I64));
        let b0 = builder.create_block(Some("entry"));
        builder.set_current_block(b0);
        assert!(!builder.is_current_block_terminated());
        builder.push_instr(crate::ir::instr::IrInstr::Return { values: vec![] }, None);
        assert!(builder.is_current_block_terminated());
    }

    #[test]
    fn builder_seal_unterminated_blocks() {
        let mut builder = IrFunctionBuilder::new("test", vec![], IrType::Scalar(DType::I64));
        builder.create_block(Some("entry"));
        builder.create_block(Some("orphan"));
        builder.seal_unterminated_blocks();
        let func = builder.build();
        assert!(func.blocks().iter().all(|b| b.is_sealed()));
    }

    #[test]
    fn builder_emit_const_str() {
        let mut builder = IrFunctionBuilder::new("test", vec![], IrType::Str);
        let b0 = builder.create_block(Some("entry"));
        builder.set_current_block(b0);
        let v = builder.emit_const_str("hello".into());
        assert_eq!(v.0, 0);
    }

    #[test]
    fn builder_fresh_value() {
        let mut builder = IrFunctionBuilder::new("test", vec![], IrType::Scalar(DType::I64));
        let v0 = builder.fresh_value();
        let v1 = builder.fresh_value();
        assert_ne!(v0, v1);
    }

    #[test]
    fn builder_span_table() {
        let mut builder = IrFunctionBuilder::new("test", vec![], IrType::Scalar(DType::I64));
        let b0 = builder.create_block(Some("entry"));
        builder.set_current_block(b0);
        builder.set_span_byte(42);
        builder.push_instr(crate::ir::instr::IrInstr::Return { values: vec![] }, None);
        let func = builder.build();
        assert_eq!(func.span_table.get(0, 0), Some(42));
    }
}
