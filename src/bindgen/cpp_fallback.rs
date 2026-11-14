//! Standalone C/C++ header parser fallback for `iris bindgen`.
//!
//! Handles C prototypes, `struct`/`typedef struct`, `enum`/`enum class`,
//! `namespace`, `extern "C"`, `typedef`/`using` aliases (including function
//! pointers), `class`/`struct` methods/constructors/destructors with access
//! specifiers, and concrete `template<...>` instantiations via `using`.

use std::collections::{HashMap, HashSet};

use super::preproc::{eval_int_expr, PreprocessedHeader};
use super::{
    disambiguate_name, is_c_ident, map_c_type_with_ctx, split_c_decl_type_and_name, ConstItem,
    FnDeclItem, FnParam, ParsedHeader, RecordItem, ShimCallKind, TypeContext,
};

#[derive(Debug, Clone)]
struct ClassTemplateDef {
    params: Vec<String>,
    tag: String,
    name: String,
    scope: Vec<String>,
    body: String,
}

struct ParseState {
    cpp_mode: bool,
    scope: Vec<String>,
    in_extern_c: bool,
    int_env: HashMap<String, i64>,
    types: TypeContext,
    consts: Vec<ConstItem>,
    seen_consts: HashSet<String>,
    records: Vec<RecordItem>,
    seen_records: HashSet<String>,
    fns: Vec<FnDeclItem>,
    used_fn_names: HashMap<String, usize>,
    templates: HashMap<String, ClassTemplateDef>,
    emitted_specs: HashSet<String>,
}

/// Parse declarations directly from preprocessed C/C++ header text.
pub(crate) fn parse_standalone(pre: &PreprocessedHeader, cpp_mode: bool) -> ParsedHeader {
    let mut st = ParseState {
        cpp_mode,
        scope: Vec::new(),
        in_extern_c: false,
        int_env: pre.int_env.clone(),
        types: TypeContext::default(),
        consts: pre.consts.clone(),
        seen_consts: pre.consts.iter().map(|c| c.name.clone()).collect(),
        records: Vec::new(),
        seen_records: HashSet::new(),
        fns: Vec::new(),
        used_fn_names: HashMap::new(),
        templates: HashMap::new(),
        emitted_specs: HashSet::new(),
    };

    parse_block(&pre.cleaned_source, &mut st);

    ParsedHeader {
        consts: st.consts,
        macro_fns: pre.macro_fns.clone(),
        records: st.records,
        functions: st.fns,
    }
}

fn parse_block(src: &str, st: &mut ParseState) {
    for stmt in split_top_level_items(src) {
        let s = stmt.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        // `extern "C" { ... }` or single-line `extern "C" def...`
        if let Some(rest) = strip_prefix_word(s, "extern") {
            let after_abi = rest.trim_start();
            if let Some(after_c) = after_abi
                .strip_prefix("\"C\"")
                .or_else(|| after_abi.strip_prefix("\"C++\""))
            {
                let is_c = after_abi.starts_with("\"C\"");
                let inner = after_c.trim();
                let prev = st.in_extern_c;
                st.in_extern_c = is_c;
                if let Some(body) = unwrap_outer_braces(inner) {
                    parse_block(body, st);
                } else {
                    parse_block(inner, st);
                }
                st.in_extern_c = prev;
                continue;
            }
        }
        // `namespace a::b { ... }`
        if let Some(rest) = strip_prefix_word(s, "namespace") {
            if let Some((ns_header, body)) = split_header_and_brace_body(rest) {
                let segs: Vec<String> = ns_header
                    .split("::")
                    .map(str::trim)
                    .filter(|seg| is_c_ident(seg))
                    .map(ToOwned::to_owned)
                    .collect();
                let count = segs.len();
                for seg in segs {
                    st.scope.push(seg);
                }
                parse_block(body, st);
                for _ in 0..count {
                    st.scope.pop();
                }
                continue;
            }
        }
        // `template<typename T, ...> class|struct Name { ... }`
        if let Some(rest) = strip_prefix_word(s, "template") {
            if try_parse_template_decl(rest, st) {
                continue;
            }
        }
        // `using Alias = Type;`
        if let Some(rest) = strip_prefix_word(s, "using") {
            if try_parse_using(rest, st) {
                continue;
            }
        }
        // `typedef ... Name;`
        if (s.starts_with("typedef ") || s.starts_with("typedef\t"))
            && try_parse_typedef_stmt(s, st)
        {
            continue;
        }
        // `enum ...`
        if (s.starts_with("enum ") || s.starts_with("enum\t")) && try_parse_enum(s, st) {
            continue;
        }
        // `struct ...` / `class ...`
        if (s.starts_with("struct ")
            || s.starts_with("struct\t")
            || s.starts_with("class ")
            || s.starts_with("class\t"))
            && try_parse_class_or_struct(s, None, st)
        {
            continue;
        }
        // Function prototype
        try_parse_free_fn(s, st);
    }
}

fn split_top_level_items(src: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut angle_depth = 0usize;
    let mut in_str = false;
    let mut in_chr = false;
    let mut prev_bs = false;

    for ch in src.chars() {
        cur.push(ch);
        if prev_bs {
            prev_bs = false;
            continue;
        }
        if ch == '\\' && (in_str || in_chr) {
            prev_bs = true;
            continue;
        }
        if !in_chr && ch == '"' {
            in_str = !in_str;
            continue;
        }
        if !in_str && ch == '\'' {
            in_chr = !in_chr;
            continue;
        }
        if in_str || in_chr {
            continue;
        }
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            '<' if brace_depth == 0 && paren_depth == 0 => {
                // Only treat as template `<...>` opener right after `template` or ident
                if cur.trim_end_matches('<').trim_end().ends_with("template") {
                    angle_depth += 1;
                }
            }
            '>' if angle_depth > 0 => angle_depth = angle_depth.saturating_sub(1),
            '{' => brace_depth += 1,
            '}' => {
                if brace_depth > 0 {
                    brace_depth -= 1;
                    if brace_depth == 0 && paren_depth == 0 && angle_depth == 0 {
                        let t = cur.trim();
                        if t.starts_with("namespace") || t.starts_with("extern") {
                            items.push(cur.clone());
                            cur.clear();
                        }
                    }
                }
            }
            ';' if brace_depth == 0 && paren_depth == 0 && angle_depth == 0 => {
                items.push(cur.clone());
                cur.clear();
            }
            _ => {}
        }
    }
    if !cur.trim().is_empty() {
        items.push(cur);
    }
    items
}

fn try_parse_template_decl(after_kw: &str, st: &mut ParseState) -> bool {
    let trimmed = after_kw.trim_start();
    if !trimmed.starts_with('<') {
        return false;
    }
    let Some(close) = find_matching_angle(trimmed) else {
        return false;
    };
    let tparams_str = &trimmed[1..close];
    let mut tparams = Vec::new();
    for part in tparams_str.split(',') {
        let p = part.trim();
        let ident = p.split_whitespace().last().unwrap_or("");
        if is_c_ident(ident) {
            tparams.push(ident.to_owned());
        }
    }
    let rest = trimmed[close + 1..].trim();
    // Explicit instantiation: `template class Box<int>;`
    if let Some((header, body)) = split_header_and_brace_body(rest) {
        let h_trimmed = header.trim();
        let (tag, name_part) = if let Some(n) = strip_prefix_word(h_trimmed, "class") {
            ("class", n)
        } else if let Some(n) = strip_prefix_word(h_trimmed, "struct") {
            ("struct", n)
        } else {
            return false;
        };
        let cname = name_part.split([':', '<']).next().unwrap_or("").trim();
        if !is_c_ident(cname) {
            return false;
        }
        let def = ClassTemplateDef {
            params: tparams,
            tag: tag.to_owned(),
            name: cname.to_owned(),
            scope: st.scope.clone(),
            body: body.to_owned(),
        };
        let qual = scoped_c_name(&st.scope, cname);
        st.templates.insert(qual, def.clone());
        st.templates.entry(cname.to_owned()).or_insert(def);
        return true;
    }
    false
}

fn try_parse_using(after_kw: &str, st: &mut ParseState) -> bool {
    let trimmed = after_kw.trim();
    if let Some(ns_rest) = strip_prefix_word(trimmed, "namespace") {
        let _ = ns_rest;
        return true;
    }
    let body = trimmed.trim_end_matches(';').trim();
    let Some((alias, rhs)) = body.split_once('=') else {
        return false;
    };
    let alias = alias.trim();
    let rhs = rhs.trim();
    if !is_c_ident(alias) {
        return false;
    }
    // Check if rhs is `Template<ConcreteArgs>`
    if let (Some(lt), Some(gt)) = (rhs.find('<'), rhs.rfind('>')) {
        if lt < gt {
            let tmpl_name = rhs[..lt].trim();
            let args: Vec<String> = rhs[lt + 1..gt]
                .split(',')
                .map(|a| a.trim().to_owned())
                .filter(|a| !a.is_empty())
                .collect();
            let lookup_qual = if tmpl_name.contains("::") || st.scope.is_empty() {
                tmpl_name.to_owned()
            } else {
                format!("{}::{tmpl_name}", st.scope.join("::"))
            };
            if let Some(def) = st
                .templates
                .get(&lookup_qual)
                .or_else(|| st.templates.get(tmpl_name))
                .cloned()
            {
                if def.params.len() == args.len() {
                    let spec_cpp = format!(
                        "{}<{}>",
                        scoped_c_name(&def.scope, &def.name),
                        args.join(", ")
                    );
                    if st.emitted_specs.insert(spec_cpp.clone()) {
                        let mut substituted = def.body.clone();
                        for (tp, arg) in def.params.iter().zip(args.iter()) {
                            substituted = replace_ident_tokens(&substituted, tp, arg);
                        }
                        let iris_alias = scoped_iris_name(&st.scope, alias);
                        let synth_decl = format!("{} {} {{\n{substituted}\n}};", def.tag, def.name);
                        st.types.record_names.insert(alias.to_owned());
                        st.types
                            .record_names
                            .insert(scoped_c_name(&st.scope, alias));
                        try_parse_class_or_struct(&synth_decl, Some((&iris_alias, &spec_cpp)), st);
                        return true;
                    }
                }
            }
        }
    }

    let mapped = if rhs.contains("(*)") {
        st.types.ptr_typedefs.insert(alias.to_owned());
        st.types
            .ptr_typedefs
            .insert(scoped_c_name(&st.scope, alias));
        "i64".to_owned()
    } else {
        let m = map_c_type_with_ctx(rhs, &st.types);
        if (rhs.contains('*') || rhs.contains('&') || st.types.ptr_typedefs.contains(rhs))
            && m == "i64"
        {
            st.types.ptr_typedefs.insert(alias.to_owned());
            st.types
                .ptr_typedefs
                .insert(scoped_c_name(&st.scope, alias));
        }
        m
    };
    st.types.typedefs.insert(alias.to_owned(), mapped.clone());
    st.types
        .typedefs
        .insert(scoped_c_name(&st.scope, alias), mapped);
    true
}

fn try_parse_typedef_stmt(stmt: &str, st: &mut ParseState) -> bool {
    let body = stmt
        .trim()
        .strip_prefix("typedef")
        .unwrap_or("")
        .trim_end_matches(';')
        .trim();
    if body.starts_with("struct ") || body.starts_with("struct\t") || body.starts_with("struct{") {
        if let Some((open, close)) = find_brace_pair(body) {
            let after = body[close + 1..].trim();
            if is_c_ident(after) {
                let inner = &body[open + 1..close];
                let synth = format!("struct {after} {{\n{inner}\n}};");
                return try_parse_class_or_struct(&synth, None, st);
            }
        }
    }
    if body.starts_with("enum ") || body.starts_with("enum\t") || body.starts_with("enum{") {
        if let Some((_, close)) = find_brace_pair(body) {
            let after = body[close + 1..].trim();
            if is_c_ident(after) {
                st.types.enum_types.insert(after.to_owned());
                let stripped = body[..=close].to_owned() + ";";
                return try_parse_enum(&stripped, st);
            }
        }
    }
    // Function pointer typedef: `typedef int (*Cb)(int, void*)`
    if body.contains("(*") {
        if let Some(idx) = body.find("(*") {
            let after = &body[idx + 2..];
            if let Some(close) = after.find(')') {
                let name = after[..close].trim();
                if is_c_ident(name) {
                    st.types.typedefs.insert(name.to_owned(), "i64".to_owned());
                    st.types
                        .typedefs
                        .insert(scoped_c_name(&st.scope, name), "i64".to_owned());
                    st.types.ptr_typedefs.insert(name.to_owned());
                    st.types.ptr_typedefs.insert(scoped_c_name(&st.scope, name));
                    return true;
                }
            }
        }
    }
    // Simple typedef: `typedef unsigned int u32_alias`
    if let Some((ty, alias)) = split_c_decl_type_and_name(body) {
        let mapped = map_c_type_with_ctx(&ty, &st.types);
        st.types.typedefs.insert(alias.clone(), mapped.clone());
        st.types
            .typedefs
            .insert(scoped_c_name(&st.scope, &alias), mapped.clone());
        if (ty.contains('*') || ty.contains('&') || st.types.ptr_typedefs.contains(ty.trim()))
            && mapped == "i64"
        {
            st.types.ptr_typedefs.insert(alias.clone());
            st.types
                .ptr_typedefs
                .insert(scoped_c_name(&st.scope, &alias));
        }
        return true;
    }
    false
}

fn try_parse_enum(stmt: &str, st: &mut ParseState) -> bool {
    let s = stmt.trim().trim_end_matches(';').trim();
    let rest = strip_prefix_word(s, "enum").unwrap_or("").trim_start();
    let is_scoped =
        strip_prefix_word(rest, "class").is_some() || strip_prefix_word(rest, "struct").is_some();
    let after_class = strip_prefix_word(rest, "class")
        .or_else(|| strip_prefix_word(rest, "struct"))
        .unwrap_or(rest)
        .trim_start();

    let Some(open) = after_class.find('{') else {
        return false;
    };
    let Some(close) = after_class.rfind('}') else {
        return false;
    };
    if close <= open {
        return false;
    }
    let header_part = after_class[..open].trim();
    let enum_name = header_part.split(':').next().unwrap_or("").trim();
    if is_c_ident(enum_name) {
        st.types.enum_types.insert(enum_name.to_owned());
        st.types
            .enum_types
            .insert(scoped_c_name(&st.scope, enum_name));
    }

    let body = &after_class[open + 1..close];
    let mut next_val: i64 = 0;
    for entry in body.split(',') {
        let e = entry.trim();
        if e.is_empty() {
            continue;
        }
        let (vname, val) = if let Some((k, v)) = e.split_once('=') {
            let v_eval = eval_int_expr(v.trim(), &st.int_env).unwrap_or(next_val);
            (k.trim(), v_eval)
        } else {
            (e, next_val)
        };
        if is_c_ident(vname) {
            st.int_env.insert(vname.to_owned(), val);
            let full_name = if is_scoped && is_c_ident(enum_name) {
                scoped_iris_name(&st.scope, &format!("{enum_name}_{vname}"))
            } else {
                scoped_iris_name(&st.scope, vname)
            };
            if st.seen_consts.insert(full_name.clone()) {
                st.consts.push(ConstItem {
                    name: full_name,
                    ty: "i64".to_owned(),
                    value: val.to_string(),
                });
            }
            next_val = val.saturating_add(1);
        }
    }
    true
}

fn try_parse_class_or_struct(
    stmt: &str,
    override_cpp: Option<(&str, &str)>,
    st: &mut ParseState,
) -> bool {
    let s = stmt.trim().trim_end_matches(';').trim();
    let (tag, after_tag) = if let Some(r) = strip_prefix_word(s, "class") {
        ("class", r)
    } else if let Some(r) = strip_prefix_word(s, "struct") {
        ("struct", r)
    } else {
        return false;
    };

    let Some(open) = after_tag.find('{') else {
        return false;
    };
    let Some(close) = after_tag.rfind('}') else {
        return false;
    };
    if close <= open {
        return false;
    }

    let header = after_tag[..open].trim();
    let has_bases = header.contains(':');
    let raw_name = header.split(':').next().unwrap_or("").trim();
    if !is_c_ident(raw_name) {
        return false;
    }
    let body = &after_tag[open + 1..close];
    let trailing_alias = after_tag[close + 1..].trim();
    let effective_raw = if is_c_ident(trailing_alias) {
        trailing_alias
    } else {
        raw_name
    };

    let (iris_prefix, cpp_qual) = match override_cpp {
        Some((p, c)) => (p.to_owned(), c.to_owned()),
        None => (
            scoped_iris_name(&st.scope, effective_raw),
            scoped_c_name(&st.scope, effective_raw),
        ),
    };
    st.types.record_names.insert(iris_prefix.clone());
    st.types.record_names.insert(effective_raw.to_owned());

    let mut cur_access = if tag == "class" { "private" } else { "public" };
    let mut public_fields: Vec<(String, String, String)> = Vec::new();
    let mut ctors: Vec<Vec<FnParam>> = Vec::new();
    let mut has_user_dtor = false;
    let mut has_methods = false;
    let mut has_non_public = false;
    let mut methods: Vec<(String, String, Vec<FnParam>, bool)> = Vec::new();

    for member_raw in split_class_members(body) {
        let mut m = member_raw.trim();
        // Strip leading `public:` / `private:` / `protected:` labels
        while let Some(after) = strip_access_label(m, &mut cur_access) {
            m = after.trim();
        }
        if m.is_empty() {
            continue;
        }
        let m_no_body = strip_inline_fn_body(m);
        let m_clean = m_no_body.trim().trim_end_matches(';').trim();
        if m_clean.is_empty() {
            continue;
        }
        if cur_access != "public" {
            if m_clean.contains('(') {
                has_methods = true;
            } else {
                has_non_public = true;
            }
            continue;
        }

        // Destructor?
        if let Some(after_tilde) = m_clean.strip_prefix('~').or_else(|| {
            m_clean
                .strip_prefix("virtual ")
                .and_then(|v| v.trim_start().strip_prefix('~'))
        }) {
            if strip_prefix_word(after_tilde.trim_start(), raw_name).is_some() {
                has_methods = true;
                has_user_dtor = true;
                continue;
            }
        }

        // Member method or constructor?
        if let Some(open_p) = find_top_fn_paren(m_clean) {
            if let Some(close_p) = find_matching_paren(m_clean, open_p) {
                has_methods = true;
                let before = m_clean[..open_p].trim();
                let params_raw = &m_clean[open_p + 1..close_p];
                let is_static = strip_prefix_word(before, "static").is_some();
                let before_nostatic = before
                    .trim_start_matches("virtual ")
                    .trim_start_matches("inline ")
                    .trim_start_matches("static ")
                    .trim_start_matches("explicit ")
                    .trim();
                if before_nostatic.starts_with('~') {
                    has_user_dtor = true;
                    continue;
                }
                let params = parse_c_param_list(params_raw, &st.types);

                let ctor_ident = before_nostatic.split('<').next().unwrap_or("").trim();
                if ctor_ident == raw_name {
                    ctors.push(params);
                    continue;
                }
                if let Some((ret_raw, mname)) = split_c_decl_type_and_name(before_nostatic) {
                    if is_c_ident(&mname) {
                        let ret_iris = map_c_type_with_ctx(&ret_raw, &st.types);
                        methods.push((mname, ret_iris, params, is_static));
                    }
                }
                continue;
            }
        }

        // Field declaration
        if let Some((fty, fname)) = split_c_decl_type_and_name(m_clean) {
            if is_c_ident(&fname) {
                let iris_ty = map_c_type_with_ctx(&fty, &st.types);
                public_fields.push((fname, iris_ty, fty));
            }
        }
    }

    let is_pod = tag != "class"
        && !has_methods
        && !has_bases
        && !has_non_public
        && !public_fields.is_empty()
        && override_cpp.is_none();

    if is_pod && st.seen_records.insert(iris_prefix.clone()) {
        st.records.push(RecordItem {
            name: iris_prefix.clone(),
            fields: public_fields
                .iter()
                .map(|(n, ty, _)| (n.clone(), ty.clone()))
                .collect(),
        });
        if !st.cpp_mode {
            return true;
        }
    }

    if has_methods || tag == "class" || override_cpp.is_some() || has_bases {
        if ctors.is_empty() && (!methods.is_empty() || !public_fields.is_empty()) {
            ctors.push(Vec::new());
        }
        for ctor_params in ctors {
            let cname = disambiguate_name(
                &format!("{iris_prefix}_new"),
                &ctor_params,
                &mut st.used_fn_names,
            );
            st.fns.push(FnDeclItem {
                iris_name: cname,
                c_symbol: None,
                params: ctor_params,
                ret_ty: "i64".to_owned(),
                needs_cpp_shim: true,
                shim_kind: ShimCallKind::Constructor {
                    cpp_type: cpp_qual.clone(),
                },
            });
        }
        if has_user_dtor || !methods.is_empty() || tag == "class" || override_cpp.is_some() {
            let free_name =
                disambiguate_name(&format!("{iris_prefix}_free"), &[], &mut st.used_fn_names);
            st.fns.push(FnDeclItem {
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
                    cpp_type: cpp_qual.clone(),
                },
            });
        }
        for (mname, ret_iris, raw_params, is_static) in methods {
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
            let base = format!("{iris_prefix}_{mname}");
            let iris_name = disambiguate_name(&base, &raw_params, &mut st.used_fn_names);
            st.fns.push(FnDeclItem {
                iris_name,
                c_symbol: None,
                params: full_params,
                ret_ty: ret_iris,
                needs_cpp_shim: true,
                shim_kind: if is_static {
                    ShimCallKind::StaticMethod {
                        cpp_type: cpp_qual.clone(),
                        method_name: mname,
                    }
                } else {
                    ShimCallKind::Method {
                        cpp_type: cpp_qual.clone(),
                        method_name: mname,
                    }
                },
            });
        }
        if !is_pod {
            for (fname, f_iris, f_c_ty) in public_fields {
                let get_name = disambiguate_name(
                    &format!("{iris_prefix}_get_{fname}"),
                    &[],
                    &mut st.used_fn_names,
                );
                st.fns.push(FnDeclItem {
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
                        cpp_type: cpp_qual.clone(),
                        field_name: fname.clone(),
                    },
                });
                if !f_c_ty.starts_with("const ") && !f_c_ty.contains('[') {
                    let set_name = disambiguate_name(
                        &format!("{iris_prefix}_set_{fname}"),
                        &[],
                        &mut st.used_fn_names,
                    );
                    st.fns.push(FnDeclItem {
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
                            cpp_type: cpp_qual.clone(),
                            field_name: fname,
                        },
                    });
                }
            }
        }
    }
    true
}

fn try_parse_free_fn(stmt: &str, st: &mut ParseState) -> bool {
    let cleaned_body = strip_inline_fn_body(stmt);
    let mut s = cleaned_body.trim().trim_end_matches(';').trim();
    if s.is_empty() || s.starts_with("typedef ") || s.contains('{') {
        return false;
    }
    for prefix in ["static ", "inline ", "constexpr "] {
        if let Some(r) = s.strip_prefix(prefix) {
            if prefix == "static " {
                return false;
            }
            s = r.trim();
        }
    }
    let Some(open_paren) = find_top_fn_paren(s) else {
        return false;
    };
    let Some(close_paren) = find_matching_paren(s, open_paren) else {
        return false;
    };
    let before = s[..open_paren].trim();
    let Some((ret_raw, fn_name)) = split_c_decl_type_and_name(before) else {
        return false;
    };
    if !is_c_ident(&fn_name) {
        return false;
    }
    let ret = map_c_type_with_ctx(&ret_raw, &st.types);
    let params = parse_c_param_list(&s[open_paren + 1..close_paren], &st.types);
    let cpp_qual = scoped_c_name(&st.scope, &fn_name);
    let base_iris = scoped_iris_name(&st.scope, &fn_name);
    let needs_shim = st.cpp_mode && (!st.in_extern_c || !st.scope.is_empty());

    let iris_name = disambiguate_name(&base_iris, &params, &mut st.used_fn_names);
    let c_symbol = if !needs_shim && iris_name == fn_name {
        Some(fn_name)
    } else {
        None
    };

    st.fns.push(FnDeclItem {
        iris_name,
        c_symbol,
        params,
        ret_ty: ret,
        needs_cpp_shim: needs_shim,
        shim_kind: ShimCallKind::FreeFn { cpp_qual },
    });
    true
}

fn parse_c_param_list(params_raw: &str, types: &TypeContext) -> Vec<FnParam> {
    let trimmed = params_raw.trim();
    if trimmed.is_empty() || trimmed == "void" {
        return Vec::new();
    }
    let mut params = Vec::new();
    for (idx, p) in split_top_level_commas(trimmed).into_iter().enumerate() {
        let pt = p.trim();
        if pt.is_empty() || pt == "..." {
            continue;
        }
        // Strip default value `= val`
        let pt_no_def = pt.split('=').next().unwrap_or(pt).trim();
        if pt_no_def.contains("(*") {
            let pname = extract_fn_ptr_param_name(pt_no_def).unwrap_or_else(|| format!("arg{idx}"));
            params.push(FnParam {
                name: pname,
                iris_ty: "i64".to_owned(),
                c_ty: pt_no_def.to_owned(),
                is_pointer_or_ref: true,
            });
            continue;
        }
        if let Some((pty, pname)) = split_c_decl_type_and_name(pt_no_def) {
            let final_name = if is_c_ident(&pname) {
                pname
            } else {
                format!("arg{idx}")
            };
            let iris_ty = map_c_type_with_ctx(&pty, types);
            let is_ptr = is_pointer_or_ref_type(&pty, &iris_ty, types);
            params.push(FnParam {
                name: final_name,
                iris_ty,
                c_ty: pty,
                is_pointer_or_ref: is_ptr,
            });
        } else {
            let iris_ty = map_c_type_with_ctx(pt_no_def, types);
            let is_ptr = is_pointer_or_ref_type(pt_no_def, &iris_ty, types);
            params.push(FnParam {
                name: format!("arg{idx}"),
                iris_ty,
                c_ty: pt_no_def.to_owned(),
                is_pointer_or_ref: is_ptr,
            });
        }
    }
    params
}

fn is_pointer_or_ref_type(c_ty: &str, iris_ty: &str, types: &TypeContext) -> bool {
    if iris_ty != "i64" {
        return false;
    }
    let trimmed = c_ty.trim();
    let base = trimmed
        .trim_start_matches("const ")
        .trim_start_matches("volatile ")
        .trim();
    trimmed.contains('*')
        || trimmed.contains('&')
        || types.ptr_typedefs.contains(trimmed)
        || types.ptr_typedefs.contains(base)
}

fn extract_fn_ptr_param_name(decl: &str) -> Option<String> {
    let idx = decl.find("(*")?;
    let after = &decl[idx + 2..];
    let end = after.find(')')?;
    let cand = after[..end].trim();
    if is_c_ident(cand) {
        Some(cand.to_owned())
    } else {
        None
    }
}

fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut paren = 0usize;
    let mut angle = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            '<' => angle += 1,
            '>' => angle = angle.saturating_sub(1),
            ',' if paren == 0 && angle == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn split_class_members(body: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut cur = String::new();
    let mut brace = 0usize;
    let mut paren = 0usize;
    for ch in body.chars() {
        cur.push(ch);
        match ch {
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            '{' => brace += 1,
            '}' => {
                if brace > 0 {
                    brace -= 1;
                    if brace == 0 && paren == 0 {
                        members.push(cur.clone());
                        cur.clear();
                    }
                }
            }
            ';' if brace == 0 && paren == 0 => {
                members.push(cur.clone());
                cur.clear();
            }
            _ => {}
        }
    }
    if !cur.trim().is_empty() {
        members.push(cur);
    }
    members
}

fn strip_access_label<'a>(s: &'a str, access: &mut &str) -> Option<&'a str> {
    for (kw, label) in [
        ("public", "public"),
        ("private", "private"),
        ("protected", "protected"),
    ] {
        if let Some(after) = strip_prefix_word(s, kw) {
            if let Some(rest) = after.trim_start().strip_prefix(':') {
                *access = label;
                return Some(rest);
            }
        }
    }
    None
}

fn strip_inline_fn_body(s: &str) -> String {
    if let Some(open_b) = s.find('{') {
        s[..open_b].trim().to_owned()
    } else {
        s.to_owned()
    }
}

fn find_top_fn_paren(s: &str) -> Option<usize> {
    let mut in_angles = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => in_angles += 1,
            '>' => in_angles = in_angles.saturating_sub(1),
            '(' if in_angles == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

fn find_matching_paren(s: &str, open_idx: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (rel, ch) in s[open_idx..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let close_idx = open_idx + rel;
                    return (close_idx > open_idx).then_some(close_idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_prefix_word<'a>(s: &'a str, word: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(word)?;
    match rest.chars().next() {
        None => Some(""),
        Some(c) if !c.is_ascii_alphanumeric() && c != '_' => Some(rest),
        _ => None,
    }
}

fn unwrap_outer_braces(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.starts_with('{') && t.ends_with('}') && t.len() >= 2 {
        Some(&t[1..t.len() - 1])
    } else {
        None
    }
}

fn split_header_and_brace_body(s: &str) -> Option<(&str, &str)> {
    let open = s.find('{')?;
    let close = s.rfind('}')?;
    if close <= open {
        return None;
    }
    Some((s[..open].trim(), &s[open + 1..close]))
}

fn find_brace_pair(s: &str) -> Option<(usize, usize)> {
    let open = s.find('{')?;
    let mut depth = 0usize;
    for (i, ch) in s[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, open + i));
                }
            }
            _ => {}
        }
    }
    None
}

fn find_matching_angle(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn replace_ident_tokens(src: &str, target: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_alphabetic() || chars[i] == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            if ident == target {
                out.push_str(replacement);
            } else {
                out.push_str(&ident);
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
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
