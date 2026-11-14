//! Automated C/C++ header binding generator (`iris bindgen`).
//!
//! Uses `clang`/`clang++` (`-E -dD` and `-Xclang -ast-dump=json`) when available,
//! with a built-in C/C++ preprocessor and parser fallback when `--no-clang` is set
//! or clang is absent. Supports `#define` constant expression evaluation, `#if`/`#ifdef`
//! conditionals, multi-line macros, pure function-like macros, `enum` / `enum class`,
//! `struct` / `typedef struct`, `namespace`, `class` methods/constructors/destructors,
//! function overloads, concrete `template<...>` instantiations (`using`), function
//! pointer `typedef`s, and optional `extern "C"` `.cpp` wrapper shim emission.

mod clang_ast;
mod cpp_fallback;
mod preproc;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Configuration for generating IRIS FFI bindings from a C/C++ header.
#[derive(Debug, Clone, Default)]
pub struct BindgenOptions {
    /// Optional shared/static library name emitted via `@link(lib = "...")`.
    pub link_lib: Option<String>,
    /// Optional calling convention ABI (e.g. `"C"`).
    pub abi: Option<String>,
    /// Extra `#include` search directories (`-I`).
    pub include_dirs: Vec<PathBuf>,
    /// Extra preprocessor defines (`-D NAME` or `-D NAME=VALUE`).
    pub defines: Vec<String>,
    /// Force C++ mode (also auto-detected for `.hpp`/`.hh`/`.hxx` or C++ syntax).
    pub cpp_mode: bool,
    /// Optional path where an `extern "C"` C++ wrapper `.cpp` file is written.
    pub emit_cpp_shim: Option<PathBuf>,
    /// Disable external `clang`/`clang++` invocation and use built-in fallback.
    pub no_clang: bool,
}

/// Complete output from `iris bindgen`.
#[derive(Debug, Clone)]
pub struct BindgenOutput {
    /// Generated IRIS source bindings (`const`, `record`, `extern def`, macro helpers).
    pub iris: String,
    /// Generated `extern "C"` `.cpp` wrapper implementation when requested/needed.
    pub cpp_shim: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ConstItem {
    pub name: String,
    pub ty: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub(crate) struct MacroFnItem {
    pub name: String,
    pub params: Vec<String>,
    pub body_expr: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RecordItem {
    pub name: String,
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub(crate) struct FnParam {
    pub name: String,
    pub iris_ty: String,
    pub c_ty: String,
    pub is_pointer_or_ref: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum ShimCallKind {
    FreeFn {
        cpp_qual: String,
    },
    Constructor {
        cpp_type: String,
    },
    Destructor {
        cpp_type: String,
    },
    Method {
        cpp_type: String,
        method_name: String,
    },
    StaticMethod {
        cpp_type: String,
        method_name: String,
    },
    FieldGetter {
        cpp_type: String,
        field_name: String,
    },
    FieldSetter {
        cpp_type: String,
        field_name: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct FnDeclItem {
    pub iris_name: String,
    pub c_symbol: Option<String>,
    pub params: Vec<FnParam>,
    pub ret_ty: String,
    pub needs_cpp_shim: bool,
    pub shim_kind: ShimCallKind,
}

#[derive(Debug, Default)]
pub(crate) struct ParsedHeader {
    pub consts: Vec<ConstItem>,
    pub macro_fns: Vec<MacroFnItem>,
    pub records: Vec<RecordItem>,
    pub functions: Vec<FnDeclItem>,
}

#[derive(Debug, Default)]
pub(crate) struct TypeContext {
    pub enum_types: HashSet<String>,
    pub typedefs: HashMap<String, String>,
    pub ptr_typedefs: HashSet<String>,
    pub record_names: HashSet<String>,
}

/// Generate IRIS bindings from a C/C++ header file on disk.
pub fn generate_bindings_from_file(header: &Path, opts: &BindgenOptions) -> Result<String, String> {
    Ok(generate_bindings_bundle_from_file(header, opts)?.iris)
}

/// Generate IRIS bindings (plus optional C++ shim) from a C/C++ header file on disk.
pub fn generate_bindings_bundle_from_file(
    header: &Path,
    opts: &BindgenOptions,
) -> Result<BindgenOutput, String> {
    let source = std::fs::read_to_string(header)
        .map_err(|e| format!("cannot read header '{}': {e}", header.display()))?;
    let cpp_mode = opts.cpp_mode || detect_cpp_header(Some(header), &source);
    let pre = preproc::preprocess_header(Some(header), &source, opts, cpp_mode);

    let parsed = if !opts.no_clang {
        match clang_ast::parse_with_clang(header, &pre, opts, cpp_mode) {
            Ok(p) => p,
            Err(_) => cpp_fallback::parse_standalone(&pre, cpp_mode),
        }
    } else {
        cpp_fallback::parse_standalone(&pre, cpp_mode)
    };

    let iris = render_iris_bindings(&parsed, opts);
    let cpp_shim = if opts.emit_cpp_shim.is_some() {
        Some(render_cpp_shim(Some(header), &parsed))
    } else {
        None
    };

    Ok(BindgenOutput { iris, cpp_shim })
}

/// Generate IRIS `const`, `record`, and `extern def` declarations from C/C++ header text.
pub fn generate_bindings(header_src: &str, opts: &BindgenOptions) -> String {
    generate_bindings_bundle(header_src, opts).iris
}

/// Generate IRIS bindings (plus optional C++ shim) from in-memory C/C++ header text.
pub fn generate_bindings_bundle(header_src: &str, opts: &BindgenOptions) -> BindgenOutput {
    let cpp_mode = opts.cpp_mode || detect_cpp_header(None, header_src);
    let pre = preproc::preprocess_standalone(None, header_src, opts);
    let parsed = cpp_fallback::parse_standalone(&pre, cpp_mode);
    let iris = render_iris_bindings(&parsed, opts);
    let cpp_shim = if opts.emit_cpp_shim.is_some() {
        Some(render_cpp_shim(None, &parsed))
    } else {
        None
    };
    BindgenOutput { iris, cpp_shim }
}

fn detect_cpp_header(header_path: Option<&Path>, src: &str) -> bool {
    if let Some(p) = header_path {
        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            if matches!(
                ext.to_ascii_lowercase().as_str(),
                "hpp" | "hh" | "hxx" | "h++"
            ) {
                return true;
            }
        }
    }
    src.contains("namespace ")
        || src.contains("template<")
        || src.contains("template <")
        || src.contains("class ")
        || src.contains("enum class ")
        || src.contains("enum struct ")
}

fn render_iris_bindings(parsed: &ParsedHeader, opts: &BindgenOptions) -> String {
    let mut out = String::from("// Generated by `iris bindgen`\n\n");
    let mut emitted_consts = HashSet::new();

    for c in &parsed.consts {
        if emitted_consts.insert(c.name.clone()) {
            let _ = writeln!(out, "pub const {}: {} = {}", c.name, c.ty, c.value);
        }
    }

    if !parsed.macro_fns.is_empty() {
        if !parsed.consts.is_empty() {
            out.push('\n');
        }
        for m in &parsed.macro_fns {
            if emitted_consts.insert(m.name.clone()) {
                let param_list: Vec<String> =
                    m.params.iter().map(|p| format!("{p}: i64")).collect();
                let _ = writeln!(
                    out,
                    "pub def {}({}) -> i64 {{ {} }}",
                    m.name,
                    param_list.join(", "),
                    m.body_expr
                );
            }
        }
    }

    let mut emitted_records = HashSet::new();
    for rec in &parsed.records {
        if emitted_records.insert(rec.name.clone()) {
            out.push('\n');
            let _ = writeln!(out, "pub record {} {{", rec.name);
            for (fname, fty) in &rec.fields {
                let _ = writeln!(out, "    {fname}: {fty},");
            }
            out.push_str("}\n");
        }
    }

    let mut emitted_fns = HashSet::new();
    for f in &parsed.functions {
        if !emitted_fns.insert(f.iris_name.clone()) {
            continue;
        }
        out.push('\n');
        if f.needs_cpp_shim && opts.emit_cpp_shim.is_none() {
            let _ = writeln!(
                out,
                "// C++ ABI symbol wrapper (generate .cpp glue with `--shim-out <file.cpp>`)"
            );
        }
        if let Some(lib) = &opts.link_lib {
            let _ = writeln!(out, "@link(lib = \"{lib}\")");
        }
        let abi_prefix = opts
            .abi
            .as_deref()
            .map(|a| format!("extern \"{a}\" "))
            .unwrap_or_else(|| "extern ".to_owned());
        let params_str = f
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.iris_ty))
            .collect::<Vec<_>>()
            .join(", ");
        let ret_suffix = if f.ret_ty == "unit" {
            String::new()
        } else {
            format!(" -> {}", f.ret_ty)
        };
        let _ = writeln!(
            out,
            "{abi_prefix}def {}({params_str}){ret_suffix}",
            f.iris_name
        );
    }

    out
}

fn render_cpp_shim(header_path: Option<&Path>, parsed: &ParsedHeader) -> String {
    let mut out = String::from("// Generated by `iris bindgen --shim-out`\n");
    out.push_str("#include <cstdint>\n");
    out.push_str("#include <type_traits>\n");
    if let Some(p) = header_path {
        let abs = std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .replace('\\', "/");
        let clean = abs.strip_prefix("//?/").unwrap_or(&abs);
        let _ = writeln!(out, "#include \"{clean}\"");
    }
    out.push('\n');
    out.push_str(
        "template <typename T>\ninline int64_t __iris_to_i64(T v) {\n    if constexpr (std::is_pointer_v<T>) {\n        return static_cast<int64_t>(reinterpret_cast<intptr_t>(v));\n    } else {\n        return static_cast<int64_t>(v);\n    }\n}\n\n",
    );
    out.push_str("extern \"C\" {\n\n");

    let mut emitted = HashSet::new();
    for f in &parsed.functions {
        if !f.needs_cpp_shim || f.c_symbol.is_some() || !emitted.insert(f.iris_name.clone()) {
            continue;
        }
        let ret_c = iris_to_c_shim_type(&f.ret_ty);
        let params_sig = f
            .params
            .iter()
            .map(|p| format!("{} {}", iris_to_c_shim_type(&p.iris_ty), p.name))
            .collect::<Vec<_>>()
            .join(", ");

        let _ = writeln!(out, "{ret_c} {}({params_sig}) {{", f.iris_name);
        match &f.shim_kind {
            ShimCallKind::Constructor { cpp_type } => {
                let args = format_shim_call_args(&f.params, 0);
                let _ = writeln!(
                    out,
                    "    return static_cast<int64_t>(reinterpret_cast<intptr_t>(new {cpp_type}({args})));"
                );
            }
            ShimCallKind::Destructor { cpp_type } => {
                let _ = writeln!(
                    out,
                    "    delete reinterpret_cast<{cpp_type}*>(static_cast<intptr_t>(handle));"
                );
            }
            ShimCallKind::Method {
                cpp_type,
                method_name,
            } => {
                let args = format_shim_call_args(&f.params, 1);
                let call = format!(
                    "reinterpret_cast<{cpp_type}*>(static_cast<intptr_t>(handle))->{method_name}({args})"
                );
                emit_shim_return(&mut out, &f.ret_ty, &call);
            }
            ShimCallKind::StaticMethod {
                cpp_type,
                method_name,
            } => {
                let args = format_shim_call_args(&f.params, 0);
                let call = format!("{cpp_type}::{method_name}({args})");
                emit_shim_return(&mut out, &f.ret_ty, &call);
            }
            ShimCallKind::FieldGetter {
                cpp_type,
                field_name,
            } => {
                let expr = format!(
                    "reinterpret_cast<{cpp_type}*>(static_cast<intptr_t>(handle))->{field_name}"
                );
                emit_shim_return(&mut out, &f.ret_ty, &expr);
            }
            ShimCallKind::FieldSetter {
                cpp_type,
                field_name,
            } => {
                let val_expr = f
                    .params
                    .get(1)
                    .map(shim_forward_arg)
                    .unwrap_or_else(|| "value".to_owned());
                let _ = writeln!(
                    out,
                    "    reinterpret_cast<{cpp_type}*>(static_cast<intptr_t>(handle))->{field_name} = {val_expr};"
                );
            }
            ShimCallKind::FreeFn { cpp_qual } => {
                let args = format_shim_call_args(&f.params, 0);
                let call = format!("{cpp_qual}({args})");
                emit_shim_return(&mut out, &f.ret_ty, &call);
            }
        }
        out.push_str("}\n\n");
    }

    out.push_str("} // extern \"C\"\n");
    out
}

fn format_shim_call_args(params: &[FnParam], skip: usize) -> String {
    params
        .iter()
        .skip(skip)
        .map(shim_forward_arg)
        .collect::<Vec<_>>()
        .join(", ")
}

fn shim_forward_arg(p: &FnParam) -> String {
    let raw = p.c_ty.trim();
    if p.iris_ty == "str" {
        return p.name.clone();
    }
    if raw.contains("(*)") {
        // Direct cast for inline function pointers like `int (*)(int, void *)`
        if let Some(idx) = raw.find("(*)") {
            let fn_cast = format!("{}(*){}", &raw[..idx], &raw[idx + 3..]);
            return format!(
                "reinterpret_cast<{fn_cast}>(static_cast<intptr_t>({}))",
                p.name
            );
        }
        return format!("reinterpret_cast<{raw}>(static_cast<intptr_t>({}))", p.name);
    }
    if p.is_pointer_or_ref {
        if raw.ends_with('&') && !raw.ends_with("&&") {
            return format!(
                "*reinterpret_cast<typename std::remove_reference<{raw}>::type*>(static_cast<intptr_t>({}))",
                p.name
            );
        }
        if raw.contains('*') {
            return format!("reinterpret_cast<{raw}>(static_cast<intptr_t>({}))", p.name);
        }
        return format!("reinterpret_cast<{raw}>(static_cast<intptr_t>({}))", p.name);
    }
    // Enum / scalar pass-through
    let base_c = raw
        .trim_start_matches("const ")
        .trim_start_matches("volatile ")
        .trim();
    if is_known_enum_or_custom(base_c) {
        return format!("static_cast<{base_c}>({})", p.name);
    }
    p.name.clone()
}

fn is_known_enum_or_custom(c_ty: &str) -> bool {
    c_ty.contains("::") || c_ty.starts_with("enum ")
}

fn emit_shim_return(out: &mut String, iris_ret: &str, expr: &str) {
    match iris_ret {
        "unit" => {
            let _ = writeln!(out, "    {expr};");
        }
        "i64" => {
            let _ = writeln!(out, "    return __iris_to_i64({expr});");
        }
        other => {
            let c_ret = iris_to_c_shim_type(other);
            let _ = writeln!(out, "    return static_cast<{c_ret}>({expr});");
        }
    }
}

pub(crate) fn iris_to_c_shim_type(iris_ty: &str) -> &'static str {
    match iris_ty {
        "unit" => "void",
        "bool" => "bool",
        "i8" => "int8_t",
        "u8" => "uint8_t",
        "i32" => "int32_t",
        "u32" => "uint32_t",
        "i64" => "int64_t",
        "u64" | "usize" => "uint64_t",
        "f32" => "float",
        "f64" => "double",
        "str" => "const char*",
        _ => "int64_t",
    }
}

pub(crate) fn disambiguate_name(
    base: &str,
    params: &[FnParam],
    used: &mut HashMap<String, usize>,
) -> String {
    let count = used.entry(base.to_owned()).or_insert(0);
    *count += 1;
    if *count == 1 {
        return base.to_owned();
    }
    let suffix = if params.is_empty() {
        format!("_{}", *count)
    } else {
        params
            .iter()
            .map(|p| p.iris_ty.as_str())
            .collect::<Vec<_>>()
            .join("_")
    };
    let candidate = format!("{base}_{suffix}");
    let scount = used.entry(candidate.clone()).or_insert(0);
    *scount += 1;
    if *scount == 1 {
        candidate
    } else {
        format!("{candidate}_{}", *scount)
    }
}

pub(crate) fn split_c_decl_type_and_name(decl: &str) -> Option<(String, String)> {
    let trimmed = decl.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip array suffix `name[N]` for field/param names.
    let no_arr = if let Some(idx) = trimmed.find('[') {
        trimmed[..idx].trim()
    } else {
        trimmed
    };
    let mut last_ident_start = None;
    for (i, ch) in no_arr.char_indices().rev() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            last_ident_start = Some(i);
        } else {
            break;
        }
    }
    let start = last_ident_start?;
    let name = no_arr[start..].trim();
    let ty = no_arr[..start].trim();
    if ty.is_empty() || !is_c_ident(name) {
        return None;
    }
    Some((ty.to_owned(), name.to_owned()))
}

pub(crate) fn map_c_type_with_ctx(c_ty: &str, ctx: &TypeContext) -> String {
    let trimmed = c_ty.trim();
    if let Some(aliased) = ctx.typedefs.get(trimmed) {
        return aliased.clone();
    }
    let base = trimmed
        .trim_start_matches("const ")
        .trim_start_matches("volatile ")
        .trim_start_matches("enum ")
        .trim_start_matches("struct ")
        .trim_start_matches("class ")
        .trim();
    if let Some(aliased) = ctx.typedefs.get(base) {
        return aliased.clone();
    }
    if ctx.enum_types.contains(base) {
        return "i32".to_owned();
    }
    if base.ends_with('&') || base.ends_with('*') {
        let stripped = base
            .replace('*', " * ")
            .replace('&', " & ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if stripped == "char *" || stripped == "const char *" {
            return "str".to_owned();
        }
        return "i64".to_owned();
    }
    let mapped = map_c_type_to_iris(c_ty);
    if !ctx.record_names.contains(&mapped) && mapped.contains("::") {
        return "i64".to_owned();
    }
    mapped
}

/// Map a C/C++ type spelling to its corresponding IRIS FFI type.
pub fn map_c_type_to_iris(c_ty: &str) -> String {
    let normalized = c_ty
        .replace('*', " * ")
        .replace('&', " & ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let stripped = normalized
        .replace("const ", "")
        .replace("volatile ", "")
        .replace("struct ", "")
        .replace("class ", "")
        .replace("enum ", "")
        .trim()
        .to_owned();

    if stripped == "char *" {
        return "str".to_owned();
    }
    if stripped.contains('*') || stripped.contains('&') {
        return "i64".to_owned();
    }
    match stripped.as_str() {
        "void" => "unit".to_owned(),
        "bool" | "_Bool" => "bool".to_owned(),
        "float" => "f32".to_owned(),
        "double" | "long double" => "f64".to_owned(),
        "int8_t" | "signed char" => "i8".to_owned(),
        "uint8_t" | "unsigned char" => "u8".to_owned(),
        "int" | "signed" | "signed int" | "int32_t" | "short" | "signed short" | "int16_t"
        | "uint16_t" | "char" => "i32".to_owned(),
        "unsigned" | "unsigned int" | "uint32_t" => "u32".to_owned(),
        "long" | "unsigned long" | "long long" | "int64_t" | "ssize_t" | "ptrdiff_t"
        | "intptr_t" => "i64".to_owned(),
        "unsigned long long" | "uint64_t" | "uintptr_t" => "u64".to_owned(),
        "size_t" => "usize".to_owned(),
        other if is_c_ident(other) => other.to_owned(),
        _ => "i64".to_owned(),
    }
}

pub(crate) fn is_c_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_consts_enums_records_and_extern_defs_from_c_header() {
        let header = r#"
            #define API_VERSION 2
            #define PI_APPROX 3.14159
            #define GREETING "hello"

            enum Status {
                STATUS_OK = 0,
                STATUS_ERR = 5,
                STATUS_BUSY
            };

            typedef struct {
                int64_t id;
                const char* label;
                double weight;
            } Node;

            extern "C" {
                void init_lib(void);
                int32_t add_nums(int32_t a, int32_t b);
                const char* node_label(Node* n);
            }
        "#;

        let out = generate_bindings(
            header,
            &BindgenOptions {
                link_lib: Some("mylib".into()),
                abi: Some("C".into()),
                ..Default::default()
            },
        );
        assert!(out.contains("pub const API_VERSION: i64 = 2"), "{out}");
        assert!(out.contains("pub const PI_APPROX: f64 = 3.14159"), "{out}");
        assert!(out.contains("pub const GREETING: str = \"hello\""), "{out}");
        assert!(out.contains("pub const STATUS_OK: i64 = 0"), "{out}");
        assert!(out.contains("pub const STATUS_ERR: i64 = 5"), "{out}");
        assert!(out.contains("pub const STATUS_BUSY: i64 = 6"), "{out}");
        assert!(
            out.contains("pub record Node {\n    id: i64,\n    label: str,\n    weight: f64,\n}"),
            "{out}"
        );
        assert!(
            out.contains("@link(lib = \"mylib\")\nextern \"C\" def init_lib()"),
            "{out}"
        );
        assert!(
            out.contains("extern \"C\" def add_nums(a: i32, b: i32) -> i32"),
            "{out}"
        );
        assert!(
            out.contains("extern \"C\" def node_label(n: i64) -> str"),
            "{out}"
        );
    }

    #[test]
    fn generates_macros_preprocessor_conditionals_and_cpp_classes_templates_namespaces() {
        let header = r#"
            #define BASE_PORT 8000
            #define ADMIN_PORT (BASE_PORT + (1 << 3))
            #define MULTI_LINE(x, y) \
                ((x) * 2 + (y))

            #ifdef ENABLE_FEATURE
            #define FEATURE_FLAG 1
            #else
            #define FEATURE_FLAG 0
            #endif

            #if ADMIN_PORT > 8005
            #define PORT_OK 1
            #endif

            typedef int (*EventCb)(int code, void* user_data);

            namespace math {
                enum class Mode : unsigned int {
                    Fast = 1,
                    Precise = 4
                };

                template<typename T>
                class Accumulator {
                private:
                    T total;
                public:
                    Accumulator(T seed);
                    ~Accumulator();
                    T add(T delta);
                    static int version();
                };

                using IntAcc = Accumulator<int>;

                int clamp_val(int x, int lo, int hi);
                double overload_fn(double x);
                int overload_fn(int x);
                void set_callback(EventCb cb);
            }
        "#;

        let bundle = generate_bindings_bundle(
            header,
            &BindgenOptions {
                link_lib: Some("cppmath".into()),
                abi: Some("C".into()),
                cpp_mode: true,
                defines: vec!["ENABLE_FEATURE=1".into()],
                emit_cpp_shim: Some(PathBuf::from("shim.cpp")),
                no_clang: true,
                ..Default::default()
            },
        );
        let out = &bundle.iris;
        assert!(out.contains("pub const BASE_PORT: i64 = 8000"), "{out}");
        assert!(out.contains("pub const ADMIN_PORT: i64 = 8008"), "{out}");
        assert!(out.contains("pub const FEATURE_FLAG: i64 = 1"), "{out}");
        assert!(out.contains("pub const PORT_OK: i64 = 1"), "{out}");
        assert!(
            out.contains("pub def MULTI_LINE(x: i64, y: i64) -> i64 { ((x) * 2 + (y)) }"),
            "{out}"
        );
        assert!(out.contains("pub const math_Mode_Fast: i64 = 1"), "{out}");
        assert!(
            out.contains("pub const math_Mode_Precise: i64 = 4"),
            "{out}"
        );
        assert!(
            out.contains("extern \"C\" def math_IntAcc_new(seed: i32) -> i64"),
            "{out}"
        );
        assert!(
            out.contains("extern \"C\" def math_IntAcc_free(handle: i64)"),
            "{out}"
        );
        assert!(
            out.contains("extern \"C\" def math_IntAcc_add(handle: i64, delta: i32) -> i32"),
            "{out}"
        );
        assert!(
            out.contains("extern \"C\" def math_IntAcc_version() -> i32"),
            "{out}"
        );
        assert!(
            out.contains("extern \"C\" def math_clamp_val(x: i32, lo: i32, hi: i32) -> i32"),
            "{out}"
        );
        assert!(
            out.contains("extern \"C\" def math_overload_fn(x: f64) -> f64"),
            "{out}"
        );
        assert!(
            out.contains("extern \"C\" def math_overload_fn_i32(x: i32) -> i32"),
            "{out}"
        );
        assert!(
            out.contains("extern \"C\" def math_set_callback(cb: i64)"),
            "{out}"
        );

        let shim = bundle.cpp_shim.as_deref().unwrap();
        assert!(shim.contains("new math::Accumulator<int>(seed)"), "{shim}");
        assert!(
            shim.contains(
                "delete reinterpret_cast<math::Accumulator<int>*>(static_cast<intptr_t>(handle));"
            ),
            "{shim}"
        );
        assert!(
            shim.contains("return static_cast<double>(math::overload_fn(x));"),
            "{shim}"
        );
        assert!(
            shim.contains("return static_cast<int32_t>(math::overload_fn(x));"),
            "{shim}"
        );
        assert!(
            shim.contains(
                "math::set_callback(reinterpret_cast<EventCb>(static_cast<intptr_t>(cb)));"
            ),
            "{shim}"
        );
    }
}
