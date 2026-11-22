//! Clang AST-dump driver (`clang -Xclang -ast-dump=json`) for `iris bindgen`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::preproc::{find_clang_binary, is_user_header_path, PreprocessedHeader};
use super::{
    disambiguate_name, iris_to_c_shim_type, is_c_ident, map_c_type_with_ctx, BindgenOptions,
    ConstItem, FnDeclItem, FnParam, ParsedHeader, RecordItem, ShimCallKind, TypeContext,
};

struct WalkCtx<'a> {
    target_header: &'a Path,
    include_dirs: &'a [PathBuf],
    cpp_mode: bool,
    in_extern_c: bool,
    scope: Vec<String>,
    current_file: String,
    in_user: bool,
    types: TypeContext,
    consts: Vec<ConstItem>,
    seen_consts: HashSet<String>,
    records: Vec<RecordItem>,
    seen_records: HashSet<String>,
    fns: Vec<FnDeclItem>,
    used_fn_names: HashMap<String, usize>,
    /// Canonical instantiated C++ template spellings -> preferred alias name (e.g. `math::Acc<int>` -> `math_IntAcc`)
    template_aliases: HashMap<String, String>,
    /// Concrete instantiated C++ classes queued during AST walk
    emitted_specs: HashSet<String>,
    /// Anonymous `typedef struct { ... } Name;` record ID -> Name mapping
    anonymous_record_names: HashMap<String, String>,
    /// Function pointer typedefs / pointer typedefs (`typedef int (*Cb)(...);`)
    ptr_typedefs: HashSet<String>,
}

/// Parse declarations from `header_path` using `clang`/`clang++` JSON AST dump.
pub(crate) fn parse_with_clang(
    header_path: &Path,
    pre: &PreprocessedHeader,
    opts: &BindgenOptions,
    cpp_mode: bool,
) -> Result<ParsedHeader, String> {
    let clang =
        find_clang_binary(cpp_mode).ok_or_else(|| "clang binary not found on PATH".to_owned())?;

    let ast_json = run_clang_ast_json(&clang, header_path, &pre.cleaned_source, opts, cpp_mode)?;
    let root: Value =
        serde_json::from_str(&ast_json).map_err(|e| format!("invalid clang AST JSON: {e}"))?;

    let mut ctx = WalkCtx {
        target_header: header_path,
        include_dirs: &opts.include_dirs,
        cpp_mode,
        in_extern_c: false,
        scope: Vec::new(),
        current_file: String::new(),
        in_user: false,
        types: TypeContext::default(),
        consts: pre.consts.clone(),
        seen_consts: pre.consts.iter().map(|c| c.name.clone()).collect(),
        records: Vec::new(),
        seen_records: HashSet::new(),
        fns: Vec::new(),
        used_fn_names: HashMap::new(),
        template_aliases: HashMap::new(),
        emitted_specs: HashSet::new(),
        anonymous_record_names: HashMap::new(),
        ptr_typedefs: HashSet::new(),
    };

    // Pass 1: collect type aliases & typedefs in user files first so template alias
    // names (e.g. `using IntAcc = Acc<int>;`) are known before emitting template methods.
    if let Some(inner) = root.get("inner").and_then(Value::as_array) {
        collect_aliases_pass(inner, &mut ctx);
        ctx.current_file.clear();
        ctx.in_user = false;
        walk_nodes(inner, &mut ctx);
    }

    Ok(ParsedHeader {
        consts: ctx.consts,
        macro_fns: pre.macro_fns.clone(),
        records: ctx.records,
        functions: ctx.fns,
    })
}

fn run_clang_ast_json(
    clang: &str,
    header_path: &Path,
    cleaned_source: &str,
    opts: &BindgenOptions,
    cpp_mode: bool,
) -> Result<String, String> {
    // If C++ header defines template aliases like `using IntAcc = Box<int>;`, create a
    // tiny temporary probe TU that includes the header and takes `sizeof(...)` so Clang
    // eagerly instantiates those template specializations in the AST dump.
    let mut temp_probe: Option<PathBuf> = None;
    let probe_aliases = if cpp_mode {
        extract_using_template_targets(cleaned_source)
    } else {
        Vec::new()
    };

    let input_path: PathBuf = if !probe_aliases.is_empty() {
        let abs_hdr_raw = std::fs::canonicalize(header_path)
            .unwrap_or_else(|_| header_path.to_path_buf())
            .to_string_lossy()
            .replace('\\', "/");
        let abs_hdr = abs_hdr_raw.strip_prefix("//?/").unwrap_or(&abs_hdr_raw);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "iris_bindgen_probe_{}_{}.cpp",
            std::process::id(),
            stamp
        ));
        let mut src = format!(
            "#include \"{abs_hdr}\"\nnamespace __iris_bindgen_probe {{\ninline void __probe() {{\n"
        );
        for alias in &probe_aliases {
            src.push_str(&format!("  (void)sizeof({alias});\n"));
        }
        src.push_str("}\n}\n");
        if std::fs::write(&p, src).is_ok() {
            temp_probe = Some(p.clone());
            p
        } else {
            header_path.to_path_buf()
        }
    } else {
        header_path.to_path_buf()
    };

    let invoke = |path: &Path| -> Result<std::process::Output, String> {
        let mut cmd = Command::new(clang);
        if cpp_mode {
            cmd.arg("-x").arg("c++").arg("-std=c++17");
            if cfg!(windows) {
                cmd.arg("-D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH");
            }
        } else {
            cmd.arg("-x").arg("c");
        }
        cmd.arg("-fsyntax-only")
            .arg("-Xclang")
            .arg("-ast-dump=json");
        if let Some(parent) = header_path.parent() {
            if !parent.as_os_str().is_empty() {
                cmd.arg("-I").arg(parent);
            }
        }
        for dir in &opts.include_dirs {
            cmd.arg("-I").arg(dir);
        }
        for def in &opts.defines {
            cmd.arg(format!("-D{def}"));
        }
        cmd.arg(path);
        cmd.output()
            .map_err(|e| format!("failed to launch clang: {e}"))
    };

    let mut out = invoke(&input_path)?;
    if !out.status.success() && temp_probe.is_some() {
        let probe_stderr = String::from_utf8_lossy(&out.stderr);
        let probe_broke = out.stdout.is_empty() || probe_stderr.contains("__iris_bindgen_probe");
        let fallback = invoke(header_path)?;
        if probe_broke || fallback.status.success() {
            out = fallback;
        }
    }
    if let Some(p) = temp_probe {
        let _ = std::fs::remove_file(p);
    }

    if !out.status.success() && out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn extract_using_template_targets(cleaned: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut ns_stack: Vec<String> = Vec::new();
    let mut brace_depth = 0usize;
    let mut ns_depths: Vec<(String, usize)> = Vec::new();

    for line in cleaned.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("namespace") {
            if rest.chars().next().is_some_and(char::is_whitespace) {
                let name = rest.trim().trim_end_matches('{').trim().to_owned();
                if !name.is_empty() && !name.contains(';') {
                    for seg in name.split("::") {
                        let s = seg.trim();
                        if is_c_ident(s) {
                            ns_stack.push(s.to_owned());
                            ns_depths.push((s.to_owned(), brace_depth));
                        }
                    }
                }
            }
        }
        if let Some(rest) = t.strip_prefix("using") {
            if rest.chars().next().is_some_and(char::is_whitespace) {
                if let Some((lhs, rhs)) = rest.split_once('=') {
                    let alias = lhs.trim();
                    let rhs_clean = rhs.trim().trim_end_matches(';').trim();
                    if is_c_ident(alias) && rhs_clean.contains('<') && rhs_clean.contains('>') {
                        if ns_stack.is_empty() {
                            targets.push(alias.to_owned());
                        } else {
                            targets.push(format!("{}::{alias}", ns_stack.join("::")));
                        }
                    }
                }
            }
        }
        for ch in t.chars() {
            match ch {
                '{' => brace_depth += 1,
                '}' => {
                    brace_depth = brace_depth.saturating_sub(1);
                    while ns_depths.last().is_some_and(|(_, d)| *d == brace_depth) {
                        ns_depths.pop();
                        ns_stack.pop();
                    }
                }
                _ => {}
            }
        }
    }
    targets
}

fn update_location(node: &Value, ctx: &mut WalkCtx<'_>) -> bool {
    let loc = node.get("loc");
    // Clang omits `loc.file` on consecutive nodes in the same file while still
    // emitting `includedFrom` (which names the includer, e.g. the temp probe TU).
    // Never fall back to `includedFrom`, or unchanged user nodes look non-user.
    let explicit_file = loc
        .and_then(|l| l.get("file"))
        .or_else(|| {
            loc.and_then(|l| l.get("expansionLoc"))
                .and_then(|e| e.get("file"))
        })
        .and_then(Value::as_str);

    if let Some(file) = explicit_file {
        ctx.current_file = file.to_owned();
        ctx.in_user = is_user_header_path(file, Some(ctx.target_header), ctx.include_dirs);
    }
    ctx.in_user
}

fn collect_aliases_pass(nodes: &[Value], ctx: &mut WalkCtx<'_>) {
    for node in nodes {
        let in_user = update_location(node, ctx);
        let kind = node.get("kind").and_then(Value::as_str).unwrap_or("");
        match kind {
            "NamespaceDecl" => {
                let is_inline = node
                    .get("isInline")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let name = node.get("name").and_then(Value::as_str).unwrap_or("");
                if name == "__iris_bindgen_probe" {
                    continue;
                }
                let pushed = !is_inline && !name.is_empty();
                if pushed {
                    ctx.scope.push(name.to_owned());
                }
                if let Some(inner) = node.get("inner").and_then(Value::as_array) {
                    collect_aliases_pass(inner, ctx);
                }
                if pushed {
                    ctx.scope.pop();
                }
            }
            "LinkageSpecDecl" => {
                if let Some(inner) = node.get("inner").and_then(Value::as_array) {
                    collect_aliases_pass(inner, ctx);
                }
            }
            "TypedefDecl" | "TypeAliasDecl" if in_user => {
                if let Some(name) = node.get("name").and_then(Value::as_str) {
                    if let Some(owned_id) = find_owned_tag_decl_id(node) {
                        ctx.anonymous_record_names
                            .insert(owned_id, scoped_iris_name(&ctx.scope, name));
                    }
                    if let Some(qual) = node
                        .get("type")
                        .and_then(|t| t.get("qualType"))
                        .and_then(Value::as_str)
                    {
                        let scoped = scoped_c_name(&ctx.scope, name);
                        let is_fn_ptr_or_ptr = qual.contains("(*)")
                            || (qual.contains('*') && qual != "const char *" && qual != "char *");
                        let iris_alias = if qual.contains("(*)") {
                            "i64".to_owned()
                        } else {
                            map_c_type_with_ctx(qual, &ctx.types)
                        };
                        if is_fn_ptr_or_ptr {
                            ctx.ptr_typedefs.insert(name.to_owned());
                            ctx.ptr_typedefs.insert(scoped.clone());
                        }
                        ctx.types
                            .typedefs
                            .insert(name.to_owned(), iris_alias.clone());
                        ctx.types.typedefs.insert(scoped.clone(), iris_alias);

                        if qual.contains('<') && qual.contains('>') {
                            let canonical = normalize_template_key(qual);
                            let short = canonical
                                .rsplit("::")
                                .next()
                                .unwrap_or(&canonical)
                                .to_owned();
                            let preferred = scoped_iris_name(&ctx.scope, name);
                            ctx.template_aliases
                                .entry(canonical)
                                .or_insert_with(|| preferred.clone());
                            ctx.template_aliases
                                .entry(short)
                                .or_insert_with(|| preferred.clone());
                            // Also store fully qualified alias key under current scope
                            if !ctx.scope.is_empty() && !qual.contains("::") {
                                let with_ns = normalize_template_key(&format!(
                                    "{}::{}",
                                    ctx.scope.join("::"),
                                    qual
                                ));
                                ctx.template_aliases.entry(with_ns).or_insert(preferred);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn walk_nodes(nodes: &[Value], ctx: &mut WalkCtx<'_>) {
    for node in nodes {
        let in_user = update_location(node, ctx);
        let kind = node.get("kind").and_then(Value::as_str).unwrap_or("");
        if node
            .get("isImplicit")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }

        match kind {
            "NamespaceDecl" => {
                let name = node.get("name").and_then(Value::as_str).unwrap_or("");
                if name == "__iris_bindgen_probe" {
                    continue;
                }
                let is_inline = node
                    .get("isInline")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let pushed = !is_inline && !name.is_empty();
                if pushed {
                    ctx.scope.push(name.to_owned());
                }
                if let Some(inner) = node.get("inner").and_then(Value::as_array) {
                    walk_nodes(inner, ctx);
                }
                if pushed {
                    ctx.scope.pop();
                }
            }
            "LinkageSpecDecl" => {
                let prev = ctx.in_extern_c;
                let lang = node.get("language").and_then(Value::as_str).unwrap_or("");
                if lang == "C" {
                    ctx.in_extern_c = true;
                } else if lang == "C++" {
                    ctx.in_extern_c = false;
                }
                if let Some(inner) = node.get("inner").and_then(Value::as_array) {
                    walk_nodes(inner, ctx);
                }
                ctx.in_extern_c = prev;
            }
            "EnumDecl" if in_user => handle_enum_decl(node, ctx),
            "CXXRecordDecl" | "RecordDecl" if in_user => handle_record_decl(node, None, ctx),
            "ClassTemplateDecl" if in_user => handle_class_template_decl(node, ctx),
            "ClassTemplateSpecializationDecl" if in_user => {
                handle_template_specialization(node, ctx);
            }
            "FunctionDecl" if in_user => handle_free_fn_decl(node, ctx),
            _ => {}
        }
    }
}

fn handle_enum_decl(node: &Value, ctx: &mut WalkCtx<'_>) {
    let enum_name = node.get("name").and_then(Value::as_str).unwrap_or("");
    let is_scoped = node.get("scopedEnumTag").is_some();
    if !enum_name.is_empty() {
        ctx.types.enum_types.insert(enum_name.to_owned());
        ctx.types
            .enum_types
            .insert(scoped_c_name(&ctx.scope, enum_name));
    }
    let Some(inner) = node.get("inner").and_then(Value::as_array) else {
        return;
    };
    let mut next_val: i64 = 0;
    for child in inner {
        if child.get("kind").and_then(Value::as_str) != Some("EnumConstantDecl") {
            continue;
        }
        let Some(cname) = child.get("name").and_then(Value::as_str) else {
            continue;
        };
        let val = extract_constant_expr_i64(child).unwrap_or(next_val);
        next_val = val.saturating_add(1);

        let full_name = if is_scoped && !enum_name.is_empty() {
            scoped_iris_name(&ctx.scope, &format!("{enum_name}_{cname}"))
        } else if !ctx.scope.is_empty() {
            scoped_iris_name(&ctx.scope, cname)
        } else {
            cname.to_owned()
        };
        if ctx.seen_consts.insert(full_name.clone()) {
            ctx.consts.push(ConstItem {
                name: full_name,
                ty: "i64".to_owned(),
                value: val.to_string(),
            });
        }
    }
}

fn extract_constant_expr_i64(node: &Value) -> Option<i64> {
    if node.get("kind").and_then(Value::as_str) == Some("ConstantExpr") {
        if let Some(s) = node.get("value").and_then(Value::as_str) {
            if let Ok(v) = s.parse::<i64>() {
                return Some(v);
            }
        }
    }
    if let Some(inner) = node.get("inner").and_then(Value::as_array) {
        for child in inner {
            if let Some(v) = extract_constant_expr_i64(child) {
                return Some(v);
            }
        }
    }
    None
}

fn handle_class_template_decl(node: &Value, ctx: &mut WalkCtx<'_>) {
    let Some(inner) = node.get("inner").and_then(Value::as_array) else {
        return;
    };
    for child in inner {
        if child.get("kind").and_then(Value::as_str) == Some("ClassTemplateSpecializationDecl") {
            handle_template_specialization(child, ctx);
        }
    }
}

fn handle_template_specialization(node: &Value, ctx: &mut WalkCtx<'_>) {
    let complete = node
        .get("completeDefinition")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !complete {
        return;
    }
    let Some(raw_name) = node.get("name").and_then(Value::as_str) else {
        return;
    };
    let mut targs = Vec::new();
    if let Some(inner) = node.get("inner").and_then(Value::as_array) {
        for child in inner {
            if child.get("kind").and_then(Value::as_str) == Some("TemplateArgument") {
                if let Some(ty) = child.get("type").and_then(|t| {
                    t.as_str()
                        .or_else(|| t.get("qualType").and_then(Value::as_str))
                }) {
                    targs.push(ty.to_owned());
                }
            }
        }
    }
    let full_cpp_spec = if targs.is_empty() {
        scoped_c_name(&ctx.scope, raw_name)
    } else {
        format!(
            "{}<{}>",
            scoped_c_name(&ctx.scope, raw_name),
            targs.join(", ")
        )
    };
    let norm_key = normalize_template_key(&full_cpp_spec);
    if !ctx.emitted_specs.insert(norm_key.clone()) {
        return;
    }
    let iris_prefix = ctx
        .template_aliases
        .get(&norm_key)
        .or_else(|| {
            ctx.template_aliases.get(&normalize_template_key(&format!(
                "{raw_name}<{}>",
                targs.join(", ")
            )))
        })
        .cloned()
        .unwrap_or_else(|| {
            let arg_suffix: Vec<String> = targs
                .iter()
                .map(|a| sanitize_type_suffix(&map_c_type_with_ctx(a, &ctx.types)))
                .collect();
            let base = if arg_suffix.is_empty() {
                raw_name.to_owned()
            } else {
                format!("{raw_name}_{}", arg_suffix.join("_"))
            };
            scoped_iris_name(&ctx.scope, &base)
        });
    handle_record_decl(node, Some((&iris_prefix, &full_cpp_spec)), ctx);
}

fn handle_record_decl(node: &Value, override_cpp: Option<(&str, &str)>, ctx: &mut WalkCtx<'_>) {
    let complete = node
        .get("completeDefinition")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !complete {
        return;
    }
    let node_id = node.get("id").and_then(Value::as_str).unwrap_or("");
    let raw_name = node
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| node.get("typedefNameDecl").and_then(Value::as_str))
        .or_else(|| ctx.anonymous_record_names.get(node_id).map(String::as_str))
        .unwrap_or("");
    if raw_name.is_empty() {
        return;
    }

    let tag = node
        .get("tagUsed")
        .and_then(Value::as_str)
        .unwrap_or("struct");
    let has_bases = node
        .get("bases")
        .and_then(Value::as_array)
        .is_some_and(|b| !b.is_empty());

    let (iris_prefix, cpp_qual_name) = match override_cpp {
        Some((p, c)) => (p.to_owned(), c.to_owned()),
        None => (
            scoped_iris_name(&ctx.scope, raw_name),
            scoped_c_name(&ctx.scope, raw_name),
        ),
    };

    ctx.types.record_names.insert(iris_prefix.clone());
    ctx.types.record_names.insert(raw_name.to_owned());

    let mut cur_access = if tag == "class" { "private" } else { "public" };
    let mut public_fields: Vec<(String, String, String)> = Vec::new();
    let mut ctors: Vec<Vec<FnParam>> = Vec::new();
    let mut has_user_dtor = false;
    let mut dtor_deleted_or_private = false;
    let mut has_methods = false;
    let mut has_non_public_fields = false;
    let mut methods: Vec<(&Value, String, bool)> = Vec::new();

    if let Some(inner) = node.get("inner").and_then(Value::as_array) {
        for child in inner {
            let ck = child.get("kind").and_then(Value::as_str).unwrap_or("");
            if child
                .get("isImplicit")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                continue;
            }
            match ck {
                "AccessSpecDecl" => {
                    if let Some(acc) = child.get("access").and_then(Value::as_str) {
                        cur_access = acc;
                    }
                }
                "FieldDecl" => {
                    let eff_acc = child
                        .get("access")
                        .and_then(Value::as_str)
                        .unwrap_or(cur_access);
                    if eff_acc != "public" {
                        has_non_public_fields = true;
                        continue;
                    }
                    let Some(fname) = child.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let fqual = child
                        .get("type")
                        .and_then(|t| t.get("qualType"))
                        .and_then(Value::as_str)
                        .unwrap_or("int64_t");
                    let iris_ty = map_c_type_with_ctx(fqual, &ctx.types);
                    public_fields.push((fname.to_owned(), iris_ty, fqual.to_owned()));
                }
                "CXXConstructorDecl" => {
                    has_methods = true;
                    let eff_acc = child
                        .get("access")
                        .and_then(Value::as_str)
                        .unwrap_or(cur_access);
                    let deleted = child
                        .get("explicitlyDeleted")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if eff_acc == "public" && !deleted {
                        ctors.push(extract_params_from_node(child, ctx));
                    }
                }
                "CXXDestructorDecl" => {
                    has_methods = true;
                    has_user_dtor = true;
                    let eff_acc = child
                        .get("access")
                        .and_then(Value::as_str)
                        .unwrap_or(cur_access);
                    let deleted = child
                        .get("explicitlyDeleted")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if eff_acc != "public" || deleted {
                        dtor_deleted_or_private = true;
                    }
                }
                "CXXMethodDecl" => {
                    has_methods = true;
                    let eff_acc = child
                        .get("access")
                        .and_then(Value::as_str)
                        .unwrap_or(cur_access);
                    let deleted = child
                        .get("explicitlyDeleted")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if eff_acc != "public" || deleted {
                        continue;
                    }
                    let Some(mname) = child.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    if !is_c_ident(mname) {
                        continue;
                    }
                    let is_static =
                        child.get("storageClass").and_then(Value::as_str) == Some("static");
                    methods.push((child, mname.to_owned(), is_static));
                }
                _ => {}
            }
        }
    }

    // POD C-style struct (no C++ methods/ctors/inheritance/private data)
    let is_pod_record = tag != "class"
        && !has_methods
        && !has_bases
        && !has_non_public_fields
        && !public_fields.is_empty()
        && override_cpp.is_none();

    if is_pod_record && ctx.seen_records.insert(iris_prefix.clone()) {
        ctx.records.push(RecordItem {
            name: iris_prefix.clone(),
            fields: public_fields
                .iter()
                .map(|(n, ty, _)| (n.clone(), ty.clone()))
                .collect(),
        });
        if !ctx.cpp_mode {
            return;
        }
    }

    // Class or struct with C++ methods / constructors / inheritance -> opaque handle wrappers
    if has_methods || tag == "class" || override_cpp.is_some() || has_bases {
        // Check if implicit default constructor makes sense when none are user-declared
        let needs_default_ctor = ctors.is_empty()
            && !node
                .get("definitionData")
                .and_then(|d| d.get("hasUserDeclaredConstructor"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if needs_default_ctor && (!methods.is_empty() || !public_fields.is_empty()) {
            ctors.push(Vec::new());
        }

        for ctor_params in ctors {
            let name = disambiguate_name(
                &format!("{iris_prefix}_new"),
                &ctor_params,
                &mut ctx.used_fn_names,
            );
            ctx.fns.push(FnDeclItem {
                iris_name: name,
                c_symbol: None,
                params: ctor_params,
                ret_ty: "i64".to_owned(),
                needs_cpp_shim: true,
                shim_kind: ShimCallKind::Constructor {
                    cpp_type: cpp_qual_name.clone(),
                },
            });
        }

        if !dtor_deleted_or_private
            && (has_user_dtor || !methods.is_empty() || tag == "class" || override_cpp.is_some())
        {
            let free_name =
                disambiguate_name(&format!("{iris_prefix}_free"), &[], &mut ctx.used_fn_names);
            ctx.fns.push(FnDeclItem {
                iris_name: free_name,
                c_symbol: None,
                params: vec![FnParam {
                    name: "handle".to_owned(),
                    iris_ty: "i64".to_owned(),
                    c_ty: "int64_t".to_owned(),
                    is_pointer_or_ref: false,
                }],
                ret_ty: "unit".to_owned(),
                needs_cpp_shim: true,
                shim_kind: ShimCallKind::Destructor {
                    cpp_type: cpp_qual_name.clone(),
                },
            });
        }

        for (mnode, mname, is_static) in methods {
            let ret_raw = extract_return_qualtype(mnode);
            let ret_iris = map_c_type_with_ctx(&ret_raw, &ctx.types);
            let raw_params = extract_params_from_node(mnode, ctx);
            let mut full_params = Vec::new();
            if !is_static {
                full_params.push(FnParam {
                    name: "handle".to_owned(),
                    iris_ty: "i64".to_owned(),
                    c_ty: "int64_t".to_owned(),
                    is_pointer_or_ref: false,
                });
            }
            full_params.extend(raw_params.iter().cloned());

            let base_name = format!("{iris_prefix}_{mname}");
            let iris_name = disambiguate_name(&base_name, &raw_params, &mut ctx.used_fn_names);
            let shim_kind = if is_static {
                ShimCallKind::StaticMethod {
                    cpp_type: cpp_qual_name.clone(),
                    method_name: mname,
                }
            } else {
                ShimCallKind::Method {
                    cpp_type: cpp_qual_name.clone(),
                    method_name: mname,
                }
            };
            ctx.fns.push(FnDeclItem {
                iris_name,
                c_symbol: None,
                params: full_params,
                ret_ty: ret_iris,
                needs_cpp_shim: true,
                shim_kind,
            });
        }

        // Public field accessors for non-POD C++ classes/methods
        if !is_pod_record {
            for (fname, f_iris, f_c_ty) in public_fields {
                let get_name = disambiguate_name(
                    &format!("{iris_prefix}_get_{fname}"),
                    &[],
                    &mut ctx.used_fn_names,
                );
                ctx.fns.push(FnDeclItem {
                    iris_name: get_name,
                    c_symbol: None,
                    params: vec![FnParam {
                        name: "handle".to_owned(),
                        iris_ty: "i64".to_owned(),
                        c_ty: "int64_t".to_owned(),
                        is_pointer_or_ref: false,
                    }],
                    ret_ty: f_iris.clone(),
                    needs_cpp_shim: true,
                    shim_kind: ShimCallKind::FieldGetter {
                        cpp_type: cpp_qual_name.clone(),
                        field_name: fname.clone(),
                    },
                });
                if !f_c_ty.starts_with("const ") && !f_c_ty.contains('[') {
                    let set_name = disambiguate_name(
                        &format!("{iris_prefix}_set_{fname}"),
                        &[],
                        &mut ctx.used_fn_names,
                    );
                    ctx.fns.push(FnDeclItem {
                        iris_name: set_name,
                        c_symbol: None,
                        params: vec![
                            FnParam {
                                name: "handle".to_owned(),
                                iris_ty: "i64".to_owned(),
                                c_ty: "int64_t".to_owned(),
                                is_pointer_or_ref: false,
                            },
                            FnParam {
                                name: "value".to_owned(),
                                iris_ty: f_iris,
                                c_ty: f_c_ty,
                                is_pointer_or_ref: false,
                            },
                        ],
                        ret_ty: "unit".to_owned(),
                        needs_cpp_shim: true,
                        shim_kind: ShimCallKind::FieldSetter {
                            cpp_type: cpp_qual_name.clone(),
                            field_name: fname,
                        },
                    });
                }
            }
        }
    }
}

fn handle_free_fn_decl(node: &Value, ctx: &mut WalkCtx<'_>) {
    if node.get("storageClass").and_then(Value::as_str) == Some("static")
        || node
            .get("explicitlyDeleted")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return;
    }
    let Some(raw_name) = node.get("name").and_then(Value::as_str) else {
        return;
    };
    if !is_c_ident(raw_name) {
        return;
    }
    let ret_raw = extract_return_qualtype(node);
    let ret_iris = map_c_type_with_ctx(&ret_raw, &ctx.types);
    let params = extract_params_from_node(node, ctx);

    let cpp_qual = scoped_c_name(&ctx.scope, raw_name);
    let base_iris = scoped_iris_name(&ctx.scope, raw_name);
    let needs_shim = ctx.cpp_mode && (!ctx.in_extern_c || !ctx.scope.is_empty());

    let iris_name = disambiguate_name(&base_iris, &params, &mut ctx.used_fn_names);
    let direct_c_sym = if !needs_shim && iris_name == raw_name {
        Some(raw_name.to_owned())
    } else {
        None
    };

    let needs_cpp_shim = needs_shim || (iris_name != raw_name && !ctx.in_extern_c);
    ctx.fns.push(FnDeclItem {
        iris_name,
        c_symbol: direct_c_sym,
        params,
        ret_ty: ret_iris,
        needs_cpp_shim,
        shim_kind: ShimCallKind::FreeFn { cpp_qual },
    });
}

fn extract_params_from_node(node: &Value, ctx: &WalkCtx<'_>) -> Vec<FnParam> {
    let mut params = Vec::new();
    let Some(inner) = node.get("inner").and_then(Value::as_array) else {
        return params;
    };
    let mut idx = 0usize;
    for child in inner {
        if child.get("kind").and_then(Value::as_str) != Some("ParmVarDecl") {
            continue;
        }
        let pname = child
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && is_c_ident(s))
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("arg{idx}"));
        let qual = child
            .get("type")
            .and_then(|t| t.get("qualType"))
            .and_then(Value::as_str)
            .unwrap_or("int64_t");
        let iris_ty = map_c_type_with_ctx(qual, &ctx.types);
        let trimmed_q = qual.trim();
        let is_ptr_or_ref = ((qual.contains('*') || qual.contains('&')) && iris_ty == "i64")
            || ctx.ptr_typedefs.contains(trimmed_q);
        params.push(FnParam {
            name: pname,
            iris_ty,
            c_ty: iris_or_raw_c_type(qual),
            is_pointer_or_ref: is_ptr_or_ref,
        });
        idx += 1;
    }
    params
}

fn iris_or_raw_c_type(qual: &str) -> String {
    let t = qual.trim();
    if t.is_empty() {
        "int64_t".to_owned()
    } else {
        t.to_owned()
    }
}

fn extract_return_qualtype(node: &Value) -> String {
    let Some(qual) = node
        .get("type")
        .and_then(|t| t.get("qualType"))
        .and_then(Value::as_str)
    else {
        return "void".to_owned();
    };
    // Function signature qualType looks like `int (int, double)` or `const char *(Node *)`.
    if let Some(paren_idx) = find_outer_fn_param_paren(qual) {
        return qual[..paren_idx].trim().to_owned();
    }
    qual.trim().to_owned()
}

fn find_outer_fn_param_paren(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut angle = 0i32;
    for (i, &b) in bytes.iter().enumerate().rev() {
        match b {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 && angle == 0 {
                    // Check we didn't match function-pointer return paren like `(*)(int)`
                    return Some(i);
                }
            }
            b'>' => angle += 1,
            b'<' => angle -= 1,
            _ => {}
        }
    }
    None
}

fn find_owned_tag_decl_id(node: &Value) -> Option<String> {
    if let Some(owned) = node.get("ownedTagDecl") {
        if let Some(id) = owned.get("id").and_then(Value::as_str) {
            return Some(id.to_owned());
        }
    }
    if node.get("isTagOwned").and_then(Value::as_bool) == Some(true) {
        if let Some(id) = node
            .get("decl")
            .and_then(|d| d.get("id"))
            .and_then(Value::as_str)
        {
            return Some(id.to_owned());
        }
    }
    if let Some(inner) = node.get("inner").and_then(Value::as_array) {
        for child in inner {
            if let Some(found) = find_owned_tag_decl_id(child) {
                return Some(found);
            }
        }
    }
    None
}

fn scoped_iris_name(scope: &[String], name: &str) -> String {
    if scope.is_empty() {
        name.to_owned()
    } else {
        format!("{}_{name}", scope.join("_"))
    }
}

fn scoped_c_name(scope: &[String], name: &str) -> String {
    if scope.is_empty() {
        name.to_owned()
    } else {
        format!("{}::{name}", scope.join("::"))
    }
}

fn normalize_template_key(qual: &str) -> String {
    let mut out = qual
        .trim()
        .replace("class ", "")
        .replace("struct ", "")
        .replace("const ", "");
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    out.replace(" <", "<")
        .replace("< ", "<")
        .replace(" >", ">")
        .replace(", ", ",")
}

fn sanitize_type_suffix(iris_ty: &str) -> String {
    let mut s = String::with_capacity(iris_ty.len());
    for ch in iris_ty.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch);
        } else {
            s.push('_');
        }
    }
    s
}

#[allow(dead_code)]
fn _keep_shim_import_used() -> String {
    iris_to_c_shim_type("i64").to_owned()
}
