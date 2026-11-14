//! Source-level debug information for native IRIS binaries.
//!
//! The LLVM emitter writes a marker comment carrying the source byte offset
//! before each IR instruction that has one (see `EMIT_DEBUG_LOCATIONS` in
//! `llvm_ir.rs`). [`attach_debug_info`] then rewrites the finished module
//! text: every function gets a `DISubprogram`, every instruction a `!dbg`
//! `DILocation` (the most recent marker's position, or the function's own
//! line before the first one), and the module gets a `DICompileUnit`. Byte
//! offsets are resolved through a [`SourceMap`], so code from brought modules
//! points at its own file. LLVM lowers this to DWARF, so gdb, lldb and the
//! VS Code native debuggers can break on IRIS source lines, step, show
//! backtraces with IRIS function names, and attach to a running process.
//!
//! Variables: function parameters, `val`/`var` bindings and loop-carried
//! `var`s carry DWARF types for scalars, `str`, `record` structs (with field
//! names and offsets), `enum` variants, fixed scalar arrays, `option`, `result`,
//! `tuple`, `list`, `map`, or opaque `ptr` when a lower-level type is erased.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use crate::compiler::SourceMap;
use crate::ir::function::IrFunction;
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};

/// Marker written by the emitter, followed by a decimal source byte offset.
pub(crate) const DEBUG_LOC_MARKER: &str = ";@iris.loc ";

/// Marker written by the emitter after a named value is defined:
/// `name\tkind\tllvm_type\toperand[\tencoded_ir_type]`.
pub(crate) const DEBUG_VAR_MARKER: &str = ";@iris.var ";

/// Metadata ids are allocated from here up, far above anything the emitter
/// numbers itself (it uses small ids for loop and branch-weight metadata).
const FIRST_ID: u32 = 1_000_000;

struct Metadata {
    next: u32,
    nodes: Vec<String>,
}

impl Metadata {
    fn reserve(&mut self) -> u32 {
        let id = self.next;
        self.next += 1;
        self.nodes.push(String::new());
        id
    }

    fn define(&mut self, id: u32, body: String) {
        let idx = (id - FIRST_ID) as usize;
        self.nodes[idx] = format!("!{id} = {body}");
    }

    fn add(&mut self, body: String) -> u32 {
        let id = self.reserve();
        self.define(id, body);
        id
    }
}

/// Line starts of one file, for binary-search position lookups.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { starts }
    }

    fn line_col(&self, text: &str, offset: usize) -> (u32, u32) {
        let offset = offset.min(text.len());
        let line = self.starts.partition_point(|&start| start <= offset);
        let start = self.starts[line - 1];
        let col = text
            .get(start..offset)
            .map_or(offset - start, |prefix| prefix.chars().count());
        (line as u32, col as u32 + 1)
    }
}

/// A resolved source position: file index in the source map, line, column.
type Position = (usize, u32, u32);

struct Resolver<'a> {
    map: &'a SourceMap,
    indexes: Vec<Option<LineIndex>>,
}

impl<'a> Resolver<'a> {
    fn new(map: &'a SourceMap) -> Self {
        Self {
            map,
            indexes: map.files().iter().map(|_| None).collect(),
        }
    }

    fn resolve(&mut self, byte: u32) -> Option<Position> {
        let files = self.map.files();
        let file = files
            .partition_point(|file| file.base <= byte)
            .checked_sub(1)?;
        let source = &files[file];
        let offset = (byte - source.base) as usize;
        if offset > source.text.len() {
            return None;
        }
        let index = self.indexes[file].get_or_insert_with(|| LineIndex::new(&source.text));
        let (line, col) = index.line_col(&source.text, offset);
        Some((file, line, col))
    }
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\5C").replace('"', "\\22")
}

fn llvm_scalar_info(dtype: DType) -> (&'static str, &'static str, u32, u32, &'static str) {
    // (DWARF name, LLVM type, size bits, ABI align bits, DWARF encoding)
    match dtype {
        DType::I64 => ("i64", "i64", 64, 64, "DW_ATE_signed"),
        DType::U64 => ("u64", "i64", 64, 64, "DW_ATE_unsigned"),
        DType::USize => ("usize", "i64", 64, 64, "DW_ATE_unsigned"),
        DType::I32 => ("i32", "i32", 32, 32, "DW_ATE_signed"),
        DType::U32 => ("u32", "i32", 32, 32, "DW_ATE_unsigned"),
        DType::I8 => ("i8", "i8", 8, 8, "DW_ATE_signed"),
        DType::U8 => ("u8", "i8", 8, 8, "DW_ATE_unsigned"),
        DType::F64 => ("f64", "double", 64, 64, "DW_ATE_float"),
        DType::F32 => ("f32", "float", 32, 32, "DW_ATE_float"),
        DType::Bool => ("bool", "i1", 8, 8, "DW_ATE_boolean"),
    }
}

fn pointer_bits_from_ir(ir: &str) -> u32 {
    for line in ir.lines() {
        let trimmed = line.trim();
        if let Some(layout) = trimmed.strip_prefix("target datalayout") {
            for part in layout.split(['"', '-', ' ']) {
                let spec = part.strip_prefix("p:").or_else(|| part.strip_prefix("p0:"));
                if let Some(rest) = spec {
                    if let Some(bits_str) = rest.split(':').next() {
                        if let Ok(bits) = bits_str.parse::<u32>() {
                            if bits > 0 {
                                return bits;
                            }
                        }
                    }
                }
            }
        }
        if let Some(triple) = trimmed.strip_prefix("target triple") {
            if triple.contains("avr") {
                return 16;
            }
            if triple.contains("wasm32")
                || triple.contains("i386")
                || triple.contains("i686")
                || triple.contains("armv7")
                || triple.contains("riscv32")
            {
                return 32;
            }
        }
        if trimmed.starts_with("define ") {
            break;
        }
    }
    64
}

fn align_up(offset: u32, align: u32) -> u32 {
    let align = align.max(8);
    let rem = offset % align;
    if rem == 0 {
        offset
    } else {
        offset + (align - rem)
    }
}

fn llvm_field_layout(ty: &IrType, ptr_bits: u32) -> (u32, u32) {
    match ty {
        IrType::Scalar(dt) => {
            let (_, _, size, align, _) = llvm_scalar_info(*dt);
            (size, align)
        }
        IrType::ConstInt(_) => (64, 64),
        _ => (ptr_bits, ptr_bits),
    }
}

/// Encode an `IrType` as a single-line token carried in `DEBUG_VAR_MARKER`'s
/// optional 5th tab column so DWARF emission preserves record/enum/collection
/// structure.
pub(crate) fn encode_ir_type(ty: &IrType) -> Option<String> {
    let mut out = String::new();
    if encode_nested_ir_type(ty, &mut out, 0) {
        Some(out)
    } else {
        None
    }
}

fn push_len_prefixed(out: &mut String, s: &str) {
    let _ = write!(out, "{}:{s}", s.len());
}

fn scalar_tag(dt: DType) -> &'static str {
    llvm_scalar_info(dt).0
}

fn parse_scalar_tag(tag: &str) -> Option<DType> {
    Some(match tag {
        "f32" => DType::F32,
        "f64" => DType::F64,
        "i32" => DType::I32,
        "i64" => DType::I64,
        "bool" => DType::Bool,
        "u8" => DType::U8,
        "i8" => DType::I8,
        "u32" => DType::U32,
        "u64" => DType::U64,
        "usize" => DType::USize,
        _ => return None,
    })
}

fn encode_nested_ir_type(ty: &IrType, out: &mut String, depth: usize) -> bool {
    match ty {
        IrType::Scalar(dt) => out.push_str(scalar_tag(*dt)),
        IrType::Str => out.push_str("str"),
        IrType::Struct { name, fields } => {
            out.push('S');
            push_len_prefixed(out, name);
            out.push('(');
            if depth < 3 {
                for (idx, (fname, fty)) in fields.iter().enumerate() {
                    if idx > 0 {
                        out.push(',');
                    }
                    push_len_prefixed(out, fname);
                    out.push(':');
                    if !encode_nested_ir_type(fty, out, depth + 1) {
                        out.push('P');
                    }
                }
            }
            out.push(')');
        }
        IrType::Enum { name, variants } => {
            out.push('E');
            push_len_prefixed(out, name);
            out.push('(');
            for (idx, vname) in variants.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                push_len_prefixed(out, vname);
            }
            out.push(')');
        }
        IrType::List(elem) => {
            out.push_str("L(");
            if !encode_nested_ir_type(elem, out, depth + 1) {
                out.push('P');
            }
            out.push(')');
        }
        IrType::Map(k, v) => {
            out.push_str("M(");
            if !encode_nested_ir_type(k, out, depth + 1) {
                out.push('P');
            }
            out.push(',');
            if !encode_nested_ir_type(v, out, depth + 1) {
                out.push('P');
            }
            out.push(')');
        }
        IrType::Option(inner) => {
            out.push_str("O(");
            if !encode_nested_ir_type(inner, out, depth + 1) {
                out.push('P');
            }
            out.push(')');
        }
        IrType::ResultType(ok, err) => {
            out.push_str("R(");
            if !encode_nested_ir_type(ok, out, depth + 1) {
                out.push('P');
            }
            out.push(',');
            if !encode_nested_ir_type(err, out, depth + 1) {
                out.push('P');
            }
            out.push(')');
        }
        IrType::Tuple(elems) => {
            out.push_str("T(");
            for (idx, elem) in elems.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                if !encode_nested_ir_type(elem, out, depth + 1) {
                    out.push('P');
                }
            }
            out.push(')');
        }
        IrType::Array { elem, len } => {
            let _ = write!(out, "A{len}(");
            if !encode_nested_ir_type(elem, out, depth + 1) {
                out.push('P');
            }
            out.push(')');
        }
        _ => return false,
    }
    true
}

struct TypeParser<'a> {
    rest: &'a str,
}

impl<'a> TypeParser<'a> {
    fn eat(&mut self, pat: &str) -> bool {
        if let Some(after) = self.rest.strip_prefix(pat) {
            self.rest = after;
            true
        } else {
            false
        }
    }

    fn read_usize(&mut self) -> Option<usize> {
        let end = self
            .rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(self.rest.len());
        if end == 0 {
            return None;
        }
        let n = self.rest[..end].parse::<usize>().ok()?;
        self.rest = &self.rest[end..];
        Some(n)
    }

    fn read_len_prefixed(&mut self) -> Option<String> {
        let len = self.read_usize()?;
        if !self.eat(":") {
            return None;
        }
        let s = self.rest.get(..len)?;
        self.rest = &self.rest[len..];
        Some(s.to_owned())
    }

    fn parse_type(&mut self) -> Option<IrType> {
        if self.eat("P") {
            return Some(IrType::Infer);
        }
        if self.eat("S") {
            let name = self.read_len_prefixed()?;
            if !self.eat("(") {
                return None;
            }
            let mut fields = Vec::new();
            if !self.eat(")") {
                loop {
                    let fname = self.read_len_prefixed()?;
                    if !self.eat(":") {
                        return None;
                    }
                    let fty = self.parse_type()?;
                    fields.push((fname, fty));
                    if self.eat(")") {
                        break;
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            return Some(IrType::Struct { name, fields });
        }
        if self.eat("E") {
            let name = self.read_len_prefixed()?;
            if !self.eat("(") {
                return None;
            }
            let mut variants = Vec::new();
            if !self.eat(")") {
                loop {
                    variants.push(self.read_len_prefixed()?);
                    if self.eat(")") {
                        break;
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            return Some(IrType::Enum { name, variants });
        }
        if self.eat("L(") {
            let elem = self.parse_type()?;
            if !self.eat(")") {
                return None;
            }
            return Some(IrType::List(Box::new(elem)));
        }
        if self.eat("M(") {
            let k = self.parse_type()?;
            if !self.eat(",") {
                return None;
            }
            let v = self.parse_type()?;
            if !self.eat(")") {
                return None;
            }
            return Some(IrType::Map(Box::new(k), Box::new(v)));
        }
        if self.eat("O(") {
            let inner = self.parse_type()?;
            if !self.eat(")") {
                return None;
            }
            return Some(IrType::Option(Box::new(inner)));
        }
        if self.eat("R(") {
            let ok = self.parse_type()?;
            if !self.eat(",") {
                return None;
            }
            let err = self.parse_type()?;
            if !self.eat(")") {
                return None;
            }
            return Some(IrType::ResultType(Box::new(ok), Box::new(err)));
        }
        if self.eat("T(") {
            let mut elems = Vec::new();
            if !self.eat(")") {
                loop {
                    elems.push(self.parse_type()?);
                    if self.eat(")") {
                        break;
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            return Some(IrType::Tuple(elems));
        }
        if self.eat("A") {
            let len = self.read_usize()?;
            if !self.eat("(") {
                return None;
            }
            let elem = self.parse_type()?;
            if !self.eat(")") {
                return None;
            }
            return Some(IrType::Array {
                elem: Box::new(elem),
                len,
            });
        }
        let end = self.rest.find([',', ')', ':']).unwrap_or(self.rest.len());
        let token = &self.rest[..end];
        self.rest = &self.rest[end..];
        if token == "str" {
            Some(IrType::Str)
        } else {
            parse_scalar_tag(token).map(IrType::Scalar)
        }
    }
}

pub(crate) fn decode_ir_type(encoded: &str) -> Option<IrType> {
    let mut parser = TypeParser {
        rest: encoded.trim(),
    };
    let ty = parser.parse_type()?;
    parser.rest.is_empty().then_some(ty)
}

/// The IRIS function behind an LLVM function name. The binary wrapper
/// renames the entry function to `iris_main`.
fn source_function<'m>(
    module: &'m IrModule,
    functions: &HashMap<&str, &'m IrFunction>,
    llvm_name: &str,
) -> Option<&'m IrFunction> {
    let llvm_name = llvm_name
        .strip_prefix(crate::codegen::llvm_ir::USER_SYMBOL_PREFIX)
        .unwrap_or(llvm_name);
    functions.get(llvm_name).copied().or_else(|| {
        (llvm_name == "iris_main").then(|| {
            functions
                .get("main")
                .copied()
                .or_else(|| module.functions().iter().find(|f| f.params.is_empty()))
        })?
    })
}

struct StructBuilder {
    struct_id: u32,
    file_id: u32,
    offset_bits: u32,
    max_align_bits: u32,
    members: Vec<u32>,
}

impl StructBuilder {
    fn new(struct_id: u32, file_id: u32) -> Self {
        Self {
            struct_id,
            file_id,
            offset_bits: 0,
            max_align_bits: 8,
            members: Vec::new(),
        }
    }

    fn field(
        &mut self,
        md: &mut Metadata,
        name: Option<&str>,
        base_type: u32,
        size_bits: u32,
        align_bits: u32,
    ) {
        let align = align_bits.max(8);
        self.offset_bits = align_up(self.offset_bits, align);
        self.max_align_bits = self.max_align_bits.max(align);
        let name_attr = match name {
            Some(n) => format!("name: \"{}\", ", escape(n)),
            None => String::new(),
        };
        let member = md.add(format!(
            "!DIDerivedType(tag: DW_TAG_member, {name_attr}scope: !{}, file: !{}, baseType: !{base_type}, size: {size_bits}, offset: {})",
            self.struct_id, self.file_id, self.offset_bits
        ));
        self.members.push(member);
        self.offset_bits += size_bits;
    }

    fn finish(self, md: &mut Metadata, name: &str) {
        let total_size = align_up(self.offset_bits, self.max_align_bits);
        let elements_str = self
            .members
            .iter()
            .map(|id| format!("!{id}"))
            .collect::<Vec<_>>()
            .join(", ");
        let elements = md.add(format!("!{{{elements_str}}}"));
        md.define(
            self.struct_id,
            format!(
                "!DICompositeType(tag: DW_TAG_structure_type, name: \"{}\", file: !{}, size: {}, align: {}, elements: !{})",
                escape(name),
                self.file_id,
                total_size,
                self.max_align_bits,
                elements
            ),
        );
    }
}

struct Emitter<'a> {
    md: Metadata,
    resolver: Resolver<'a>,
    module: &'a IrModule,
    unit: u32,
    unit_file: u32,
    subroutine_type: u32,
    ptr_bits: u32,
    files: HashMap<usize, u32>,
    scopes: HashMap<(u32, usize), u32>,
    locations: HashMap<(u32, Position), u32>,
    types: HashMap<String, u32>,
}

impl Emitter<'_> {
    fn file(&mut self, index: usize) -> u32 {
        if let Some(&id) = self.files.get(&index) {
            return id;
        }
        let path = &self.resolver.map.files()[index].path;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "main.iris".into());
        let directory = path
            .parent()
            .map(|dir| dir.to_string_lossy().trim_start_matches(r"\\?\").to_owned())
            .unwrap_or_default();
        let id = self.md.add(format!(
            "!DIFile(filename: \"{}\", directory: \"{}\")",
            escape(&name),
            escape(&directory)
        ));
        self.files.insert(index, id);
        id
    }

    /// The scope for a position inside subprogram `sp` (declared in file
    /// `sp_file`): the subprogram itself, or a lexical block file when the
    /// position lies in a different file.
    fn scope(&mut self, sp: u32, sp_file: usize, file: usize) -> u32 {
        if file == sp_file {
            return sp;
        }
        if let Some(&id) = self.scopes.get(&(sp, file)) {
            return id;
        }
        let file_id = self.file(file);
        let id = self.md.add(format!(
            "!DILexicalBlockFile(scope: !{sp}, file: !{file_id}, discriminator: 0)"
        ));
        self.scopes.insert((sp, file), id);
        id
    }

    fn location(&mut self, sp: u32, sp_file: usize, position: Position) -> u32 {
        if let Some(&id) = self.locations.get(&(sp, position)) {
            return id;
        }
        let (file, line, col) = position;
        let scope = self.scope(sp, sp_file, file);
        let id = self.md.add(format!(
            "!DILocation(line: {line}, column: {col}, scope: !{scope})"
        ));
        self.locations.insert((sp, position), id);
        id
    }

    fn basic_type(&mut self, name: &'static str, bits: u32, encoding: &'static str) -> u32 {
        if let Some(&id) = self.types.get(name) {
            return id;
        }
        let id = self.md.add(format!(
            "!DIBasicType(name: \"{name}\", size: {bits}, encoding: {encoding})"
        ));
        self.types.insert(name.to_owned(), id);
        id
    }

    fn scalar_dwarf_type(&mut self, dtype: DType) -> u32 {
        let (name, _, bits, _, encoding) = llvm_scalar_info(dtype);
        self.basic_type(name, bits, encoding)
    }

    fn c_size_t_type(&mut self) -> u32 {
        let name = if self.ptr_bits == 64 {
            "usize"
        } else {
            "size_t"
        };
        self.basic_type(name, self.ptr_bits, "DW_ATE_unsigned")
    }

    fn str_type(&mut self) -> u32 {
        if let Some(&id) = self.types.get("str") {
            return id;
        }
        let char_ty = self.basic_type("char", 8, "DW_ATE_signed_char");
        let id = self.md.add(format!(
            "!DIDerivedType(tag: DW_TAG_pointer_type, name: \"str\", baseType: !{char_ty}, size: {})",
            self.ptr_bits
        ));
        self.types.insert("str".to_owned(), id);
        id
    }

    fn void_ptr_type(&mut self) -> u32 {
        if let Some(&id) = self.types.get("ptr") {
            return id;
        }
        let id = self.md.add(format!(
            "!DIDerivedType(tag: DW_TAG_pointer_type, name: \"ptr\", baseType: null, size: {})",
            self.ptr_bits
        ));
        self.types.insert("ptr".to_owned(), id);
        id
    }

    fn pointer_to(&mut self, pointee: u32) -> u32 {
        let key = format!("*!{pointee}");
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let id = self.md.add(format!(
            "!DIDerivedType(tag: DW_TAG_pointer_type, baseType: !{pointee}, size: {})",
            self.ptr_bits
        ));
        self.types.insert(key, id);
        id
    }

    /// The `IrisTag` C enum from `iris_runtime.h` so `IrisVal.tag` prints symbolically.
    fn iris_tag_enum_type(&mut self) -> u32 {
        if let Some(&id) = self.types.get("c:IrisTag") {
            return id;
        }
        let base = self.basic_type("IrisTagBase", 32, "DW_ATE_unsigned");
        const TAGS: &[&str] = &[
            "IRIS_TAG_I64",
            "IRIS_TAG_I32",
            "IRIS_TAG_F64",
            "IRIS_TAG_F32",
            "IRIS_TAG_BOOL",
            "IRIS_TAG_STR",
            "IRIS_TAG_LIST",
            "IRIS_TAG_MAP",
            "IRIS_TAG_OPTION",
            "IRIS_TAG_RESULT",
            "IRIS_TAG_CLOSURE",
            "IRIS_TAG_TUPLE",
            "IRIS_TAG_STRUCT",
            "IRIS_TAG_CHAN",
            "IRIS_TAG_ATOMIC",
            "IRIS_TAG_GRAD",
            "IRIS_TAG_SPARSE",
            "IRIS_TAG_UNIT",
            "IRIS_TAG_ENUM",
            "IRIS_TAG_MUTEX",
            "IRIS_TAG_TASK_GROUP",
            "IRIS_TAG_WEAK_REF",
            "IRIS_TAG_NATIVE_OBJECT",
            "IRIS_TAG_TASK",
        ];
        let elems: Vec<String> = TAGS
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let e = self
                    .md
                    .add(format!("!DIEnumerator(name: \"{name}\", value: {i})"));
                format!("!{e}")
            })
            .collect();
        let elem_list = self.md.add(format!("!{{{}}}", elems.join(", ")));
        let id = self.md.add(format!(
            "!DICompositeType(tag: DW_TAG_enumeration_type, name: \"IrisTag\", file: !{}, baseType: !{base}, size: 32, align: 32, elements: !{elem_list})",
            self.unit_file
        ));
        self.types.insert("c:IrisTag".to_owned(), id);
        id
    }

    fn val_payload_ptr_type(&mut self, elem: Option<&IrType>) -> u32 {
        match elem {
            Some(
                ty @ (IrType::Struct { .. }
                | IrType::List(_)
                | IrType::Map(_, _)
                | IrType::Option(_)
                | IrType::ResultType(_, _)
                | IrType::Array { .. }),
            ) => self
                .dwarf_type_from_ir(ty, "ptr")
                .unwrap_or_else(|| self.void_ptr_type()),
            Some(IrType::Enum { name, variants }) => self.iris_enum_ptr_type(name, variants),
            Some(IrType::Tuple(_)) => self.iris_list_ptr_type(None),
            _ => self.void_ptr_type(),
        }
    }

    fn type_label(ty: &IrType) -> String {
        encode_ir_type(ty).unwrap_or_else(|| "ptr".to_owned())
    }

    fn iris_val_ptr_type(&mut self, elem: Option<&IrType>) -> u32 {
        let suffix = elem.map(Self::type_label).unwrap_or_else(|| "*".to_owned());
        let key = format!("c:IrisVal*:{suffix}");
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let struct_id = self.md.reserve();
        let ptr_id = self.pointer_to(struct_id);
        self.types.insert(key, ptr_id);

        let tag_ty = self.iris_tag_enum_type();
        let i64_ty = self.scalar_dwarf_type(DType::I64);
        let i32_ty = self.scalar_dwarf_type(DType::I32);
        let f64_ty = self.scalar_dwarf_type(DType::F64);
        let f32_ty = self.scalar_dwarf_type(DType::F32);
        let u8_ty = self.scalar_dwarf_type(DType::U8);
        let str_ty = self.str_type();
        let ptr_field_ty = self.val_payload_ptr_type(elem);

        let union_id = self.md.reserve();
        let union_bits = 64u32.max(self.ptr_bits);
        let union_fields = [
            ("i64", i64_ty, 64),
            ("i32", i32_ty, 32),
            ("f64", f64_ty, 64),
            ("f32", f32_ty, 32),
            ("boolean", u8_ty, 8),
            ("str", str_ty, self.ptr_bits),
            ("ptr", ptr_field_ty, self.ptr_bits),
        ];
        let u_members: Vec<String> = union_fields
            .iter()
            .map(|(fname, fty, fbits)| {
                let m = self.md.add(format!(
                    "!DIDerivedType(tag: DW_TAG_member, name: \"{fname}\", scope: !{union_id}, file: !{}, baseType: !{fty}, size: {fbits})",
                    self.unit_file
                ));
                format!("!{m}")
            })
            .collect();
        let u_list = self.md.add(format!("!{{{}}}", u_members.join(", ")));
        self.md.define(
            union_id,
            format!(
                "!DICompositeType(tag: DW_TAG_union_type, file: !{}, size: {union_bits}, align: {union_bits}, elements: !{u_list})",
                self.unit_file
            ),
        );

        let mut sb = StructBuilder::new(struct_id, self.unit_file);
        sb.field(&mut self.md, Some("tag"), tag_ty, 32, 32);
        sb.field(&mut self.md, None, union_id, union_bits, union_bits);
        let struct_name = match elem {
            Some(ty) => format!("IrisVal<{}>", Self::display_type_name(ty)),
            None => "IrisVal".to_owned(),
        };
        sb.finish(&mut self.md, &struct_name);
        ptr_id
    }

    fn display_type_name(ty: &IrType) -> String {
        match ty {
            IrType::Scalar(dt) => dt.to_string(),
            IrType::Str => "str".to_owned(),
            IrType::Struct { name, .. } | IrType::Enum { name, .. } => name.clone(),
            IrType::List(inner) => format!("list<{}>", Self::display_type_name(inner)),
            IrType::Map(k, v) => format!(
                "map<{}, {}>",
                Self::display_type_name(k),
                Self::display_type_name(v)
            ),
            IrType::Option(inner) => format!("option<{}>", Self::display_type_name(inner)),
            IrType::ResultType(ok, err) => format!(
                "result<{}, {}>",
                Self::display_type_name(ok),
                Self::display_type_name(err)
            ),
            IrType::Tuple(_) => "tuple".to_owned(),
            IrType::Array { elem, len } => format!("[{}; {}]", Self::display_type_name(elem), len),
            _ => "ptr".to_owned(),
        }
    }

    fn iris_list_ptr_type(&mut self, elem: Option<&IrType>) -> u32 {
        let suffix = elem.map(Self::type_label).unwrap_or_else(|| "*".to_owned());
        let key = format!("c:IrisList*:{suffix}");
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let struct_id = self.md.reserve();
        let ptr_id = self.pointer_to(struct_id);
        self.types.insert(key, ptr_id);

        let val_ptr = self.iris_val_ptr_type(elem);
        let data_ty = self.pointer_to(val_ptr);
        let size_t_ty = self.c_size_t_type();
        let void_ptr = self.void_ptr_type();
        let pb = self.ptr_bits;

        let mut sb = StructBuilder::new(struct_id, self.unit_file);
        sb.field(&mut self.md, Some("data"), data_ty, pb, pb);
        sb.field(&mut self.md, Some("len"), size_t_ty, pb, pb);
        sb.field(&mut self.md, Some("cap"), size_t_ty, pb, pb);
        sb.field(&mut self.md, Some("chunks"), void_ptr, pb, pb);
        let name = match elem {
            Some(ty) => format!("IrisList<{}>", Self::display_type_name(ty)),
            None => "IrisList".to_owned(),
        };
        sb.finish(&mut self.md, &name);
        ptr_id
    }

    fn iris_map_ptr_type(&mut self, val_ty: Option<&IrType>) -> u32 {
        let suffix = val_ty
            .map(Self::type_label)
            .unwrap_or_else(|| "*".to_owned());
        let key = format!("c:IrisMap*:{suffix}");
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let map_id = self.md.reserve();
        let map_ptr_id = self.pointer_to(map_id);
        self.types.insert(key, map_ptr_id);

        let entry_id = self.md.reserve();
        let entry_ptr_id = self.pointer_to(entry_id);
        let pb = self.ptr_bits;
        let str_ty = self.str_type();
        let val_ptr_ty = self.iris_val_ptr_type(val_ty);
        let size_t_ty = self.c_size_t_type();

        let mut eb = StructBuilder::new(entry_id, self.unit_file);
        eb.field(&mut self.md, Some("key"), str_ty, pb, pb);
        eb.field(&mut self.md, Some("val"), val_ptr_ty, pb, pb);
        eb.field(&mut self.md, Some("next"), entry_ptr_id, pb, pb);
        let entry_name = match val_ty {
            Some(ty) => format!("IrisMapEntry<{}>", Self::display_type_name(ty)),
            None => "IrisMapEntry".to_owned(),
        };
        eb.finish(&mut self.md, &entry_name);

        let buckets_ty = self.pointer_to(entry_ptr_id);
        let mut mb = StructBuilder::new(map_id, self.unit_file);
        mb.field(&mut self.md, Some("buckets"), buckets_ty, pb, pb);
        mb.field(&mut self.md, Some("n_buckets"), size_t_ty, pb, pb);
        mb.field(&mut self.md, Some("len"), size_t_ty, pb, pb);
        let map_name = match val_ty {
            Some(ty) => format!("IrisMap<{}>", Self::display_type_name(ty)),
            None => "IrisMap".to_owned(),
        };
        mb.finish(&mut self.md, &map_name);
        map_ptr_id
    }

    fn iris_option_ptr_type(&mut self, inner: &IrType) -> u32 {
        let key = format!("c:IrisOption*:{}", Self::type_label(inner));
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let struct_id = self.md.reserve();
        let ptr_id = self.pointer_to(struct_id);
        self.types.insert(key, ptr_id);

        let u8_ty = self.scalar_dwarf_type(DType::U8);
        let val_ptr = self.iris_val_ptr_type(Some(inner));
        let pb = self.ptr_bits;
        let mut sb = StructBuilder::new(struct_id, self.unit_file);
        sb.field(&mut self.md, Some("has_value"), u8_ty, 8, 8);
        sb.field(&mut self.md, Some("value"), val_ptr, pb, pb);
        sb.finish(
            &mut self.md,
            &format!("IrisOption<{}>", Self::display_type_name(inner)),
        );
        ptr_id
    }

    fn iris_result_ptr_type(&mut self, ok: &IrType) -> u32 {
        let key = format!("c:IrisResult*:{}", Self::type_label(ok));
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let struct_id = self.md.reserve();
        let ptr_id = self.pointer_to(struct_id);
        self.types.insert(key, ptr_id);

        let u8_ty = self.scalar_dwarf_type(DType::U8);
        let val_ptr = self.iris_val_ptr_type(Some(ok));
        let pb = self.ptr_bits;
        let mut sb = StructBuilder::new(struct_id, self.unit_file);
        sb.field(&mut self.md, Some("is_ok"), u8_ty, 8, 8);
        sb.field(&mut self.md, Some("value"), val_ptr, pb, pb);
        sb.finish(
            &mut self.md,
            &format!("IrisResult<{}>", Self::display_type_name(ok)),
        );
        ptr_id
    }

    fn enum_tag_type(&mut self, name: &str, variants: &[String]) -> u32 {
        let key = format!("enum_tag:{name}");
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let resolved: &[String] = self
            .module
            .enum_defs
            .get(name)
            .map(|(vs, _)| vs.as_slice())
            .filter(|vs| !vs.is_empty())
            .unwrap_or(variants);
        let base = self.scalar_dwarf_type(DType::I64);
        let elems: Vec<String> = resolved
            .iter()
            .enumerate()
            .map(|(i, vname)| {
                let e = self.md.add(format!(
                    "!DIEnumerator(name: \"{}\", value: {i})",
                    escape(vname)
                ));
                format!("!{e}")
            })
            .collect();
        let elem_list = self.md.add(format!("!{{{}}}", elems.join(", ")));
        let id = self.md.add(format!(
            "!DICompositeType(tag: DW_TAG_enumeration_type, name: \"{}\", file: !{}, baseType: !{base}, size: 64, align: 64, elements: !{elem_list})",
            escape(name),
            self.unit_file
        ));
        self.types.insert(key, id);
        id
    }

    fn iris_enum_ptr_type(&mut self, name: &str, variants: &[String]) -> u32 {
        let key = format!("c:IrisEnum*:{name}");
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let struct_id = self.md.reserve();
        let ptr_id = self.pointer_to(struct_id);
        self.types.insert(key, ptr_id);

        let tag_ty = self.enum_tag_type(name, variants);
        let val_ptr = self.iris_val_ptr_type(None);
        let fields_ty = self.pointer_to(val_ptr);
        let size_t_ty = self.c_size_t_type();
        let pb = self.ptr_bits;

        let mut sb = StructBuilder::new(struct_id, self.unit_file);
        sb.field(&mut self.md, Some("tag"), tag_ty, 64, 64);
        sb.field(&mut self.md, Some("fields"), fields_ty, pb, pb);
        sb.field(&mut self.md, Some("len"), size_t_ty, pb, pb);
        sb.finish(&mut self.md, &format!("IrisEnum<{name}>"));
        ptr_id
    }

    fn struct_ptr_type(&mut self, name: &str, inline_fields: &[(String, IrType)]) -> u32 {
        let key = format!("struct:{name}");
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let struct_id = self.md.reserve();
        let ptr_id = self.pointer_to(struct_id);
        self.types.insert(key, ptr_id);

        let fields = self
            .module
            .struct_defs
            .get(name)
            .map(Vec::as_slice)
            .filter(|fs| !fs.is_empty())
            .unwrap_or(inline_fields);
        let mut sb = StructBuilder::new(struct_id, self.unit_file);
        for (fname, fty) in fields {
            let (f_size, f_align) = llvm_field_layout(fty, self.ptr_bits);
            let f_llvm = crate::codegen::llvm_ir::llvm_type_complete(fty)
                .unwrap_or_else(|_| "ptr".to_owned());
            let f_dwarf = self
                .dwarf_type_from_ir(fty, &f_llvm)
                .unwrap_or_else(|| self.void_ptr_type());
            sb.field(&mut self.md, Some(fname), f_dwarf, f_size, f_align);
        }
        sb.finish(&mut self.md, name);
        ptr_id
    }

    fn fixed_array_ptr_type(&mut self, elem: &IrType, len: usize) -> Option<u32> {
        let key = format!("arr:{len}:{}", Self::type_label(elem));
        if let Some(&id) = self.types.get(&key) {
            return Some(id);
        }
        let elem_llvm = crate::codegen::llvm_ir::llvm_type_complete(elem).ok()?;
        let elem_dwarf = self.dwarf_type_from_ir(elem, &elem_llvm)?;
        let (elem_bits, elem_align) = llvm_field_layout(elem, self.ptr_bits);
        let total_bits = elem_bits.saturating_mul(len as u32);
        let subrange = self.md.add(format!("!DISubrange(count: {len})"));
        let elems = self.md.add(format!("!{{!{subrange}}}"));
        let arr_id = self.md.add(format!(
            "!DICompositeType(tag: DW_TAG_array_type, baseType: !{elem_dwarf}, size: {total_bits}, align: {elem_align}, elements: !{elems})"
        ));
        let ptr_id = self.pointer_to(arr_id);
        self.types.insert(key, ptr_id);
        Some(ptr_id)
    }

    fn dwarf_type_from_ir(&mut self, ty: &IrType, llvm_ty: &str) -> Option<u32> {
        match ty {
            IrType::Scalar(dt) => Some(self.scalar_dwarf_type(*dt)),
            IrType::Str => Some(self.str_type()),
            IrType::Struct { name, fields } => Some(self.struct_ptr_type(name, fields)),
            IrType::Enum { name, variants } => {
                if llvm_ty == "i64" {
                    Some(self.enum_tag_type(name, variants))
                } else {
                    Some(self.iris_val_ptr_type(Some(ty)))
                }
            }
            IrType::List(elem) => Some(self.iris_list_ptr_type(Some(elem))),
            IrType::Map(_, val) => Some(self.iris_map_ptr_type(Some(val))),
            IrType::Option(inner) => Some(self.iris_option_ptr_type(inner)),
            IrType::ResultType(ok, _) => Some(self.iris_result_ptr_type(ok)),
            IrType::Tuple(_) => Some(self.iris_val_ptr_type(Some(ty))),
            IrType::Array { elem, len } => self.fixed_array_ptr_type(elem, *len),
            _ if llvm_ty == "ptr" => Some(self.void_ptr_type()),
            _ => None,
        }
    }

    fn param_debug_type(&mut self, ty: &IrType) -> Option<(u32, &'static str)> {
        if let IrType::Scalar(dt) = ty {
            let (_, llvm_ty, _, _, _) = llvm_scalar_info(*dt);
            return Some((self.scalar_dwarf_type(*dt), llvm_ty));
        }
        let dwarf_ty = self
            .dwarf_type_from_ir(ty, "ptr")
            .unwrap_or_else(|| self.void_ptr_type());
        Some((dwarf_ty, "ptr"))
    }

    /// Fallback DWARF type for legacy 4-column variable markers without an
    /// encoded `IrType`.
    fn variable_type(&mut self, kind: &str) -> Option<u32> {
        Some(match kind {
            "i64" => self.scalar_dwarf_type(DType::I64),
            "u64" => self.scalar_dwarf_type(DType::U64),
            "usize" => self.scalar_dwarf_type(DType::USize),
            "i32" => self.scalar_dwarf_type(DType::I32),
            "u32" => self.scalar_dwarf_type(DType::U32),
            "i8" => self.scalar_dwarf_type(DType::I8),
            "u8" => self.scalar_dwarf_type(DType::U8),
            "f64" => self.scalar_dwarf_type(DType::F64),
            "f32" => self.scalar_dwarf_type(DType::F32),
            "bool" => self.scalar_dwarf_type(DType::Bool),
            "str" => self.str_type(),
            "ptr" => self.void_ptr_type(),
            _ => return None,
        })
    }
}

/// Per-function state while rewriting its body.
struct FunctionScope {
    sp: u32,
    file: usize,
    entry: Position,
    entry_loc: u32,
    current: Option<Position>,
    /// Instructions emitted at the very top of the entry block (parameter and
    /// local spill slots + `llvm.dbg.declare`), placed after entry labels.
    entry_decls: Vec<String>,
    /// Entry block label / comment lines seen before the first instruction.
    entry_prefix: String,
    /// Rewritten instructions of the function body.
    body: String,
    seen_first_instr: bool,
    /// Spill slots already allocated for `(name, type_key)` locals in this
    /// function so reassignments update the same stack slot.
    local_slots: HashMap<(String, String), String>,
    next_slot: usize,
    /// SSA names defined so far in this function. A variable marker whose
    /// operand is not among them is dropped rather than risk an IR that
    /// fails verification.
    defined: HashSet<String>,
}

/// Rewrite `ir` (emitted with location markers) to carry DWARF metadata,
/// resolving byte offsets through `map`.
pub(crate) fn attach_debug_info(ir: &str, module: &IrModule, map: &SourceMap) -> String {
    let mut md = Metadata {
        next: FIRST_ID,
        nodes: Vec::new(),
    };
    let subroutine_type = md.add("!DISubroutineType(types: !{})".into());
    let mut emitter = Emitter {
        md,
        resolver: Resolver::new(map),
        module,
        unit: 0,
        unit_file: 0,
        subroutine_type,
        ptr_bits: pointer_bits_from_ir(ir),
        files: HashMap::new(),
        scopes: HashMap::new(),
        locations: HashMap::new(),
        types: HashMap::new(),
    };
    let unit_file = if map.files().is_empty() {
        emitter
            .md
            .add("!DIFile(filename: \"<unknown>\", directory: \"\")".into())
    } else {
        emitter.file(0)
    };
    emitter.unit_file = unit_file;
    emitter.unit = emitter.md.add(format!(
        "distinct !DICompileUnit(language: DW_LANG_C, file: !{unit_file}, producer: \"iris {}\", isOptimized: false, runtimeVersion: 0, emissionKind: FullDebug)",
        env!("CARGO_PKG_VERSION")
    ));

    let functions: HashMap<&str, &IrFunction> = module
        .functions()
        .iter()
        .map(|function| (function.name.as_str(), function))
        .collect();

    let mut out = String::with_capacity(ir.len() + ir.len() / 4);
    let mut scope: Option<FunctionScope> = None;
    let mut uses_dbg_declare = false;

    for line in ir.lines() {
        // Function header: `define <ret> @name(<params>)<attrs> {`
        if line.starts_with("define ") && line.trim_end().ends_with('{') {
            if let Some(prev) = scope.take() {
                finish_function(prev, &mut out);
            }
            let llvm_name = line
                .split_once('@')
                .and_then(|(_, rest)| rest.split_once('('))
                .map(|(name, _)| name.trim_matches('"'))
                .unwrap_or("?");
            let function = source_function(module, &functions, llvm_name);
            let new_scope = open_function(&mut emitter, line, llvm_name, function, &mut out);
            uses_dbg_declare |= !new_scope.entry_decls.is_empty();
            scope = Some(new_scope);
            continue;
        }

        if line == "}" {
            if let Some(finished) = scope.take() {
                finish_function(finished, &mut out);
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }

        let Some(function) = scope.as_mut() else {
            out.push_str(line);
            out.push('\n');
            continue;
        };

        let trimmed = line.trim_start();
        if let Some(byte) = trimmed.strip_prefix(DEBUG_LOC_MARKER) {
            if let Some(position) = byte
                .trim()
                .parse::<u32>()
                .ok()
                .and_then(|byte| emitter.resolver.resolve(byte))
            {
                function.current = Some(position);
            }
            continue;
        }

        if let Some(spec) = trimmed.strip_prefix(DEBUG_VAR_MARKER) {
            if describe_variable(&mut emitter, function, spec) {
                uses_dbg_declare = true;
            }
            continue;
        }

        // Labels, blank lines and comments carry no location.
        if !line.starts_with(' ') || trimmed.is_empty() || trimmed.starts_with(';') {
            let target = if function.seen_first_instr {
                &mut function.body
            } else {
                &mut function.entry_prefix
            };
            target.push_str(line);
            target.push('\n');
            continue;
        }
        function.seen_first_instr = true;
        if let Some((name, _)) = trimmed.split_once(" = ") {
            if name.starts_with('%') && !name.contains(' ') {
                function.defined.insert(name.to_owned());
            }
        }

        let position = function.current.unwrap_or(function.entry);
        let loc = emitter.location(function.sp, function.file, position);
        // Insert before any trailing comment, never inside it.
        let (code, comment) = match line.find("  ;") {
            Some(at) => line.split_at(at),
            None => (line, ""),
        };
        if code.contains("!dbg ") {
            function.body.push_str(line);
        } else {
            let _ = write!(function.body, "{}, !dbg !{loc}{comment}", code.trim_end());
        }
        function.body.push('\n');
    }
    if let Some(finished) = scope.take() {
        finish_function(finished, &mut out);
    }

    if uses_dbg_declare {
        out.push_str("\ndeclare void @llvm.dbg.declare(metadata, metadata, metadata)\n");
    }
    let version = emitter
        .md
        .add("!{i32 2, !\"Debug Info Version\", i32 3}".into());
    let dwarf = emitter.md.add("!{i32 7, !\"Dwarf Version\", i32 4}".into());
    let _ = writeln!(out, "\n!llvm.dbg.cu = !{{!{}}}", emitter.unit);
    let _ = writeln!(out, "!llvm.module.flags = !{{!{version}, !{dwarf}}}");
    for node in emitter.md.nodes {
        debug_assert!(
            !node.is_empty(),
            "reserved DWARF metadata node was left undefined"
        );
        if !node.is_empty() {
            out.push_str(&node);
            out.push('\n');
        }
    }
    out
}

fn finish_function(function: FunctionScope, out: &mut String) {
    out.push_str(&function.entry_prefix);
    for decl in function.entry_decls {
        out.push_str(&decl);
        out.push('\n');
    }
    out.push_str(&function.body);
}

/// Emit the subprogram for one function header and prepare its scope.
fn open_function(
    emitter: &mut Emitter<'_>,
    header: &str,
    llvm_name: &str,
    function: Option<&IrFunction>,
    out: &mut String,
) -> FunctionScope {
    // The function's position: its first located instruction.
    let entry = function
        .and_then(|function| {
            function.blocks().iter().find_map(|block| {
                (0..block.instrs.len()).find_map(|idx| function.debug_span(block.id.0, idx))
            })
        })
        .and_then(|byte| emitter.resolver.resolve(byte))
        .unwrap_or((0, 1, 1));
    let (file, line, _) = entry;
    let file_id = emitter.file(file);
    let name = function.map_or(llvm_name, |function| function.name.as_str());
    let sp = emitter.md.add(format!(
        "distinct !DISubprogram(name: \"{}\", linkageName: \"{}\", scope: !{file_id}, file: !{file_id}, line: {line}, type: !{}, scopeLine: {line}, spFlags: DISPFlagDefinition, unit: !{})",
        escape(name),
        escape(llvm_name),
        emitter.subroutine_type,
        emitter.unit
    ));
    let header = header.trim_end();
    let _ = writeln!(
        out,
        "{} !dbg !{sp} {{",
        header[..header.len() - 1].trim_end()
    );

    // Describe scalar, `str`, record, enum, array, and collection parameters at
    // entry so debuggers can inspect them. Lambda and par-for bodies use a
    // different native parameter list.
    let entry_loc = emitter.location(sp, file, entry);
    let mut entry_decls = Vec::new();
    if let Some(function) = function.filter(|function| {
        !function.name.starts_with("__lambda_")
            && !function.name.starts_with("__par_body_")
            && function.capture_count == 0
    }) {
        for (arg_no, param) in function.params.iter().enumerate() {
            let Some((ty, llvm_ty)) = emitter.param_debug_type(&param.ty) else {
                continue;
            };
            let var = emitter.md.add(format!(
                "!DILocalVariable(name: \"{}\", arg: {}, scope: !{sp}, file: !{file_id}, line: {line}, type: !{ty})",
                escape(&param.name),
                arg_no + 1
            ));
            // Spilled to a stack slot and described with dbg.declare, as
            // clang does at -O0: a dbg.value would leave the parameter
            // "<optimized out>" once its register is reused.
            let name = crate::codegen::llvm_ir::param_ref(&param.name);
            entry_decls.push(format!("  {name}.dbg = alloca {llvm_ty}"));
            entry_decls.push(format!("  store {llvm_ty} {name}, ptr {name}.dbg"));
            entry_decls.push(format!(
                "  call void @llvm.dbg.declare(metadata ptr {name}.dbg, metadata !{var}, metadata !DIExpression()), !dbg !{entry_loc}"
            ));
        }
    }
    // Parameters are defined on entry.
    let defined = header
        .split(['(', ')', ',', ' '])
        .filter(|token| token.starts_with('%'))
        .map(str::to_owned)
        .collect();
    FunctionScope {
        sp,
        file,
        entry,
        entry_loc,
        current: None,
        entry_decls,
        entry_prefix: String::new(),
        body: String::new(),
        seen_first_instr: false,
        local_slots: HashMap::new(),
        next_slot: 0,
        defined,
    }
}

/// Spill a variable marker into an entry-block stack slot described by
/// `llvm.dbg.declare` so `-O0` debuggers keep unused or cross-call locals
/// inspectable throughout the function.
fn describe_variable(emitter: &mut Emitter<'_>, function: &mut FunctionScope, spec: &str) -> bool {
    let mut fields = spec.split('\t');
    let (Some(name), Some(kind), Some(llvm_ty), Some(operand)) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next().map(str::trim),
    ) else {
        return false;
    };
    let encoded_ty = fields.next().map(str::trim);
    if operand.starts_with('%') && !function.defined.contains(operand) {
        return false;
    }
    let position = function.current.unwrap_or(function.entry);
    let type_key = match encoded_ty {
        Some(enc) => format!("{llvm_ty}:{enc}"),
        None => kind.to_owned(),
    };
    let slot_key = (name.to_owned(), type_key);
    let slot = if let Some(existing_slot) = function.local_slots.get(&slot_key) {
        existing_slot.clone()
    } else {
        let Some(ty) = encoded_ty
            .and_then(decode_ir_type)
            .and_then(|ir_ty| emitter.dwarf_type_from_ir(&ir_ty, llvm_ty))
            .or_else(|| emitter.variable_type(kind))
        else {
            return false;
        };
        let file_id = emitter.file(position.0);
        let var = emitter.md.add(format!(
            "!DILocalVariable(name: \"{}\", scope: !{}, file: !{file_id}, line: {}, type: !{ty})",
            escape(name),
            function.sp,
            position.1
        ));
        let slot = format!("%__iris_dbg_{}", function.next_slot);
        function.next_slot += 1;
        let entry_loc = function.entry_loc;
        function
            .entry_decls
            .push(format!("  {slot} = alloca {llvm_ty}"));
        function
            .entry_decls
            .push(format!("  store {llvm_ty} zeroinitializer, ptr {slot}"));
        function.entry_decls.push(format!(
            "  call void @llvm.dbg.declare(metadata ptr {slot}, metadata !{var}, metadata !DIExpression()), !dbg !{entry_loc}"
        ));
        function.local_slots.insert(slot_key, slot.clone());
        slot
    };
    let loc = emitter.location(function.sp, function.file, position);
    let _ = writeln!(
        function.body,
        "  store {llvm_ty} {operand}, ptr {slot}, !dbg !{loc}"
    );
    true
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ir::function::{FunctionId, Param, PlaceMeta, SpanTable};
    use crate::ir::types::TypeArena;

    #[test]
    fn emits_dwarf_ptr_type_for_non_scalar_param_and_local() {
        let src = "def inspect(xs: list<i64>) -> i64 {\n    val ys = xs;\n    0\n}\n";
        let map = SourceMap::single(PathBuf::from("test.iris"), src.to_string());
        let mut module = IrModule::new("test");
        let func = IrFunction {
            id: FunctionId(0),
            name: "inspect".to_string(),
            params: vec![Param {
                name: "xs".to_string(),
                ty: IrType::List(Box::new(IrType::Scalar(DType::I64))),
            }],
            return_ty: IrType::Scalar(DType::I64),
            blocks: Vec::new(),
            value_defs: HashMap::new(),
            value_types: HashMap::new(),
            type_arena: TypeArena::new(),
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
        let _ = module.add_function(func);

        let xs_param = crate::codegen::llvm_ir::param_ref("xs");
        let ir = format!(
            "define i64 @{}inspect(ptr {xs_param}) {{\nentry:\n;@iris.loc 40\n  %v0 = bitcast ptr {xs_param} to ptr\n;@iris.var ys\tptr\tptr\t%v0\n  ret i64 0\n}}\n",
            crate::codegen::llvm_ir::USER_SYMBOL_PREFIX,
        );
        let annotated = attach_debug_info(&ir, &module, &map);
        assert!(
            annotated.contains("!DIDerivedType(tag: DW_TAG_pointer_type, name: \"ptr\""),
            "expected ptr DIDerivedType in:\n{annotated}"
        );
        assert!(
            annotated.contains(&format!(
                "call void @llvm.dbg.declare(metadata ptr {xs_param}.dbg"
            )),
            "expected llvm.dbg.declare for xs param in:\n{annotated}"
        );
        assert!(
            annotated.contains("call void @llvm.dbg.declare(metadata ptr %__iris_dbg_0")
                && annotated.contains("store ptr %v0, ptr %__iris_dbg_0"),
            "expected llvm.dbg.declare spill for ys local in:\n{annotated}"
        );
    }

    #[test]
    fn encode_decode_ir_type_roundtrips_composites() {
        let types = vec![
            IrType::Scalar(DType::I64),
            IrType::Scalar(DType::Bool),
            IrType::Str,
            IrType::Struct {
                name: "Point".to_owned(),
                fields: vec![
                    ("x".to_owned(), IrType::Scalar(DType::F64)),
                    ("y".to_owned(), IrType::Scalar(DType::F64)),
                ],
            },
            IrType::Enum {
                name: "Color".to_owned(),
                variants: vec!["Red".to_owned(), "Green".to_owned(), "Blue".to_owned()],
            },
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
            IrType::Map(Box::new(IrType::Str), Box::new(IrType::Scalar(DType::I64))),
            IrType::Option(Box::new(IrType::Str)),
            IrType::ResultType(Box::new(IrType::Scalar(DType::I64)), Box::new(IrType::Str)),
            IrType::Tuple(vec![IrType::Scalar(DType::I64), IrType::Str]),
            IrType::Array {
                elem: Box::new(IrType::Scalar(DType::I32)),
                len: 4,
            },
        ];
        for ty in types {
            let encoded = encode_ir_type(&ty).expect("type should encode");
            let decoded = decode_ir_type(&encoded).expect("type should decode");
            assert_eq!(decoded, ty, "encoded={encoded}");
        }
        assert!(encode_ir_type(&IrType::TapeRef).is_none());
    }

    #[test]
    fn emits_dwarf_composite_types_for_records_collections_enums_and_arrays() {
        let src = "record Point { x: f64, y: f64 }\nrecord Node { value: i64, next: option<Node> }\ndef inspect(p: Point, xs: list<i64>, m: map<str, Point>, n: Node, s: Shape) -> i64 {\n    val col = 0;\n    val arr = 0;\n    0\n}\n";
        let map = SourceMap::single(PathBuf::from("composites.iris"), src.to_string());
        let mut module = IrModule::new("composites");
        let point_fields = vec![
            ("x".to_owned(), IrType::Scalar(DType::F64)),
            ("y".to_owned(), IrType::Scalar(DType::F64)),
        ];
        let point_ty = IrType::Struct {
            name: "Point".to_owned(),
            fields: point_fields.clone(),
        };
        let node_ref = IrType::Struct {
            name: "Node".to_owned(),
            fields: vec![],
        };
        module.struct_defs.insert("Point".to_owned(), point_fields);
        module.struct_defs.insert(
            "Node".to_owned(),
            vec![
                ("value".to_owned(), IrType::Scalar(DType::I64)),
                (
                    "next".to_owned(),
                    IrType::Option(Box::new(node_ref.clone())),
                ),
            ],
        );
        module.enum_defs.insert(
            "Color".to_owned(),
            (
                vec!["Red".to_owned(), "Green".to_owned(), "Blue".to_owned()],
                vec![vec![], vec![], vec![]],
            ),
        );
        let shape_ty = IrType::Enum {
            name: "Shape".to_owned(),
            variants: vec!["Dot".to_owned(), "Circle".to_owned()],
        };
        module.enum_defs.insert(
            "Shape".to_owned(),
            (
                vec!["Dot".to_owned(), "Circle".to_owned()],
                vec![vec![], vec![IrType::Scalar(DType::F64)]],
            ),
        );

        let func = IrFunction {
            id: FunctionId(0),
            name: "inspect".to_owned(),
            params: vec![
                Param {
                    name: "p".to_owned(),
                    ty: point_ty.clone(),
                },
                Param {
                    name: "xs".to_owned(),
                    ty: IrType::List(Box::new(IrType::Scalar(DType::I64))),
                },
                Param {
                    name: "m".to_owned(),
                    ty: IrType::Map(Box::new(IrType::Str), Box::new(point_ty.clone())),
                },
                Param {
                    name: "n".to_owned(),
                    ty: node_ref,
                },
                Param {
                    name: "s".to_owned(),
                    ty: shape_ty,
                },
            ],
            return_ty: IrType::Scalar(DType::I64),
            blocks: Vec::new(),
            value_defs: HashMap::new(),
            value_types: HashMap::new(),
            type_arena: TypeArena::new(),
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
        let _ = module.add_function(func);

        let color_enc = encode_ir_type(&IrType::Enum {
            name: "Color".to_owned(),
            variants: vec![],
        })
        .unwrap();
        let arr_enc = encode_ir_type(&IrType::Array {
            elem: Box::new(IrType::Scalar(DType::I32)),
            len: 3,
        })
        .unwrap();
        let res_enc = encode_ir_type(&IrType::ResultType(
            Box::new(IrType::Scalar(DType::I64)),
            Box::new(IrType::Str),
        ))
        .unwrap();

        let ir = format!(
            "define i64 @{}inspect(ptr {}, ptr {}, ptr {}, ptr {}, ptr {}) {{\nentry:\n;@iris.loc 90\n  %v0 = add i64 0, 1\n;@iris.var col\ti64\ti64\t%v0\t{color_enc}\n  %v1 = alloca [3 x i32]\n;@iris.var arr\tptr\tptr\t%v1\t{arr_enc}\n  %v2 = call ptr @iris_make_ok(ptr null)\n;@iris.var res\tptr\tptr\t%v2\t{res_enc}\n  ret i64 0\n}}\n",
            crate::codegen::llvm_ir::USER_SYMBOL_PREFIX,
            crate::codegen::llvm_ir::param_ref("p"),
            crate::codegen::llvm_ir::param_ref("xs"),
            crate::codegen::llvm_ir::param_ref("m"),
            crate::codegen::llvm_ir::param_ref("n"),
            crate::codegen::llvm_ir::param_ref("s"),
        );
        let annotated = attach_debug_info(&ir, &module, &map);
        for needle in [
            "!DICompositeType(tag: DW_TAG_structure_type, name: \"Point\"",
            "!DIDerivedType(tag: DW_TAG_member, name: \"x\"",
            "!DIDerivedType(tag: DW_TAG_member, name: \"y\"",
            "!DICompositeType(tag: DW_TAG_structure_type, name: \"Node\"",
            "name: \"IrisOption<Node>\"",
            "name: \"IrisList<i64>\"",
            "name: \"IrisMap<Point>\"",
            "name: \"IrisMapEntry<Point>\"",
            "name: \"IrisEnum<Shape>\"",
            "name: \"IrisResult<i64>\"",
            "!DICompositeType(tag: DW_TAG_enumeration_type, name: \"Color\"",
            "!DIEnumerator(name: \"Green\", value: 1)",
            "!DICompositeType(tag: DW_TAG_array_type",
            "!DISubrange(count: 3)",
        ] {
            assert!(
                annotated.contains(needle),
                "missing `{needle}` in:\n{annotated}"
            );
        }
    }
}
