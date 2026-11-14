//! File-based module compiler with bring resolution.
//!
//! [`FileCompiler`] resolves `bring "path.iris"` and `bring std.name`
//! declarations by reading files from disk (and the embedded stdlib).
//! It performs BFS resolution with cycle detection.
//!
//! Supports **incremental compilation** via [`crate::cache::BuildCache`]:
//! files whose content hash has not changed since the last build are
//! skipped during re-parsing.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::cache::BuildCache;
use crate::error::Error;
use crate::lower::substitute_ast_type;
use crate::parser::ast::{
    AstBlock, AstExpr, AstFunction, AstHandlerArm, AstMacroDef, AstModule, AstModuleDef, AstStmt,
    AstTraitDef, AstType, AstWhenPattern, BringPath, Ident,
};
use crate::parser::lexer::Span;
use crate::parser::parse::Parser;

/// One file's text as the lexer saw it, placed at `base` in a [`SourceMap`].
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub base: u32,
    /// Path on disk, or `<stdlib>/<name>.iris` for an embedded module.
    pub path: PathBuf,
    /// Preprocessed text; spans index into it after subtracting `base`.
    pub text: String,
}

/// Gives every file of a multi-file program its own disjoint span range, so
/// a byte offset in the merged program identifies one file and position.
/// The main file is first, at base 0, so its offsets are its own; a brought
/// file's diagnostics are rendered in that file (`diagnostics::localize`).
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
    next_base: u32,
}

impl SourceMap {
    fn add(&mut self, path: PathBuf, text: String) -> u32 {
        let base = self.next_base;
        // A one-byte gap keeps a file's end-of-text span out of the next file.
        self.next_base = base.saturating_add(text.len() as u32).saturating_add(1);
        self.files.push(SourceFile { base, path, text });
        base
    }

    /// The file containing `byte`, and the offset within that file's text.
    pub fn lookup(&self, byte: u32) -> Option<(&SourceFile, u32)> {
        let index = self.files.partition_point(|file| file.base <= byte);
        let file = self.files.get(index.checked_sub(1)?)?;
        let offset = byte - file.base;
        (offset as usize <= file.text.len()).then_some((file, offset))
    }

    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    /// The lines of every file, for native runtime errors. The standard
    /// library's are not reported: a failure inside it is reported at the
    /// line that called it.
    pub fn lines(&self) -> crate::ir::module::SourceLines {
        crate::ir::module::SourceLines::from_files(
            self.files
                .iter()
                .map(|f| {
                    let reported = !f.path.starts_with("<stdlib>");
                    crate::ir::module::SourceFileLines::new(
                        display_path(&f.path),
                        f.base,
                        &f.text,
                        reported,
                    )
                })
                .collect(),
        )
    }

    /// A map holding just one file at base 0, for single-source compiles.
    pub fn single(path: PathBuf, text: String) -> Self {
        let mut map = Self::default();
        map.add(path, text);
        map
    }
}

/// The source maps of recent compiles, by canonical main-file path, so an
/// error can be rendered in the file it is in after the compiler is gone.
/// Bounded: each holds every file's text.
static SOURCE_MAPS: std::sync::Mutex<Vec<(PathBuf, std::sync::Arc<SourceMap>)>> =
    std::sync::Mutex::new(Vec::new());
const SOURCE_MAPS_KEPT: usize = 16;

fn register_source_map(main: &Path, map: SourceMap) {
    let Ok(mut maps) = SOURCE_MAPS.lock() else {
        return;
    };
    maps.retain(|(path, _)| path != main);
    if maps.len() >= SOURCE_MAPS_KEPT {
        maps.remove(0);
    }
    maps.push((main.to_path_buf(), std::sync::Arc::new(map)));
}

/// The files of the latest compile of `main`, if it is one of the recent ones.
pub fn source_map_for(main: &Path) -> Option<std::sync::Arc<SourceMap>> {
    let key = main.canonicalize().unwrap_or_else(|_| main.to_path_buf());
    let maps = SOURCE_MAPS.lock().ok()?;
    maps.iter()
        .rev()
        .find(|(path, _)| *path == key)
        .map(|(_, map)| map.clone())
}

/// A path as a message shows it: relative to the working directory when it is
/// inside it, without Windows' `\\?\` prefix, with `/` separators.
pub(crate) fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let plain = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text));
    let shown = std::env::current_dir()
        .ok()
        .and_then(|cwd| plain.strip_prefix(&cwd).ok().map(Path::to_path_buf))
        .unwrap_or(plain);
    shown.to_string_lossy().replace('\\', "/")
}

/// Compiles `.iris` files from disk, resolving all `bring` declarations.
pub struct FileCompiler {
    /// Extra search directories for bring resolution (beyond the file's directory).
    search_paths: Vec<PathBuf>,
    /// Incremental build cache.
    cache: BuildCache,
    /// When set, each parsed file is placed at its own base offset and
    /// recorded here (see [`SourceMap`]).
    source_map: Option<std::cell::RefCell<SourceMap>>,
    /// Demand-driven incremental query engine for memoized compilation.
    query: std::cell::RefCell<crate::query::QueryEngine>,
}

impl FileCompiler {
    pub fn new() -> Self {
        // Try to locate a project root from CWD.
        let cache = if let Ok(cwd) = std::env::current_dir() {
            BuildCache::open(&cwd)
        } else {
            BuildCache::disabled()
        };
        Self {
            search_paths: Vec::new(),
            cache,
            source_map: Some(std::cell::RefCell::new(SourceMap::default())),
            query: std::cell::RefCell::new(crate::query::QueryEngine::new()),
        }
    }

    /// Create a compiler with an explicit cache.
    pub fn with_cache(cache: BuildCache) -> Self {
        Self {
            search_paths: Vec::new(),
            cache,
            source_map: Some(std::cell::RefCell::new(SourceMap::default())),
            query: std::cell::RefCell::new(crate::query::QueryEngine::new()),
        }
    }

    pub fn with_search_paths(paths: Vec<PathBuf>) -> Self {
        Self {
            search_paths: paths,
            cache: BuildCache::disabled(),
            source_map: Some(std::cell::RefCell::new(SourceMap::default())),
            query: std::cell::RefCell::new(crate::query::QueryEngine::new()),
        }
    }

    /// Give every parsed file a disjoint span range and record it. Always
    /// on now; kept so a caller that needs the map says so.
    pub fn enable_source_map(&mut self) {
        self.source_map = Some(std::cell::RefCell::new(SourceMap::default()));
    }

    /// The files parsed so far, if [`Self::enable_source_map`] was called.
    pub fn take_source_map(&mut self) -> Option<SourceMap> {
        self.source_map.take().map(std::cell::RefCell::into_inner)
    }

    /// Set extra search paths (used by LSP / builder to find packages/libraries).
    pub fn set_search_paths(&mut self, paths: Vec<PathBuf>) {
        self.search_paths = paths;
    }

    /// Compile a file on disk, BFS-resolving all brings and returning the full merged AST.
    pub fn compile_file_to_ast(
        &self,
        path: &Path,
        extra_paths: &[&Path],
    ) -> Result<AstModule, Error> {
        let src = std::fs::read_to_string(path)?;
        self.compile_file_to_ast_with_text(path, &src, extra_paths)
    }

    /// Like [`compile_file_to_ast`] but uses the provided `source` text for the
    /// main file instead of reading from disk.  Brings are still resolved from
    /// disk relative to `path`'s directory.
    pub fn compile_file_to_ast_with_text(
        &self,
        path: &Path,
        source: &str,
        extra_paths: &[&Path],
    ) -> Result<AstModule, Error> {
        let canonical = path.canonicalize().map_err(Error::Io)?;
        let base_dir = canonical.parent().unwrap_or(Path::new(".")).to_path_buf();

        let mut search: Vec<PathBuf> = vec![base_dir.clone()];
        search.extend(extra_paths.iter().map(|p| p.to_path_buf()));
        search.extend(self.search_paths.iter().cloned());

        // The main file first, at base 0, whatever this compiler parsed before.
        if let Some(map) = &self.source_map {
            *map.borrow_mut() = SourceMap::default();
        }
        let result = self
            .parse_source_from(source, &canonical)
            .and_then(|main_ast| self.resolve_brings(main_ast, &canonical, &base_dir, &search));
        if let Some(map) = &self.source_map {
            register_source_map(&canonical, map.borrow().clone());
        }
        result
    }

    /// Disable the incremental cache for this compiler instance.
    pub fn disable_cache(&mut self) {
        self.cache = BuildCache::disabled();
    }

    /// Add an extra search path for bring resolution.
    pub fn add_search_path(&mut self, path: PathBuf) {
        self.search_paths.push(path);
    }

    /// Flush the build cache manifest to disk.
    pub fn flush_cache(&mut self) {
        self.cache.flush();
    }

    /// Incrementally compiles `source` under `name`, reusing memoized AST/IR/emit
    /// results when content or AST structure is unchanged.
    pub fn compile_incremental(
        &self,
        name: &str,
        source: &str,
        emit: crate::EmitKind,
    ) -> Result<String, Error> {
        let mut qe = self.query.borrow_mut();
        let sid = qe.set_source(name, source);
        qe.compile(sid, name, emit)
    }

    /// Returns cumulative incremental query statistics for this compiler instance.
    pub fn query_stats(&self) -> crate::query::QueryStats {
        self.query.borrow().stats()
    }

    /// BFS-resolve all `bring` declarations and merge dependencies into `main_ast`.
    fn resolve_brings(
        &self,
        main_ast: AstModule,
        canonical: &Path,
        base_dir: &Path,
        search: &[PathBuf],
    ) -> Result<AstModule, Error> {
        let mut merged = main_ast;
        let mut visited: HashSet<PathBuf> = HashSet::new();
        visited.insert(canonical.to_path_buf());

        let mut queue: VecDeque<(BringPath, PathBuf)> = VecDeque::new();
        for bring in &merged.brings.clone() {
            queue.push_back((bring.path.clone(), base_dir.to_path_buf()));
        }

        while let Some((bring_path, from_dir)) = queue.pop_front() {
            match &bring_path {
                BringPath::File(rel_path) => {
                    // If the stem matches an inline module name, skip —
                    // flatten_inline_modules will handle it later.
                    let stem = rel_path.trim_end_matches(".iris");
                    if merged.modules.iter().any(|m| m.name.name == stem) {
                        continue;
                    }
                    // Resolve relative to `from_dir`, then search_paths.
                    let resolved = self.resolve_file_path(rel_path, &from_dir, search)?;
                    if !visited.contains(&resolved) {
                        visited.insert(resolved.clone());
                        let dep_src = std::fs::read_to_string(&resolved)?;
                        let mut dep_ast = self.parse_source_from(&dep_src, &resolved)?;
                        let dep_dir = resolved.parent().unwrap_or(Path::new(".")).to_path_buf();
                        for dep_bring in &dep_ast.brings {
                            queue.push_back((dep_bring.path.clone(), dep_dir.clone()));
                        }
                        let stem = {
                            let path_str = resolved.to_string_lossy();
                            let marker_fwd = "iris_packages/";
                            let marker_bwd = "iris_packages\\";
                            let marker_pos = path_str
                                .rfind(marker_fwd)
                                .or_else(|| path_str.rfind(marker_bwd));
                            if let Some(pos) = marker_pos {
                                let after = &path_str[pos + "iris_packages/".len()..];
                                after
                                    .split(['/', '\\'])
                                    .next()
                                    .unwrap_or("module")
                                    .replace(['.', '-'], "_")
                            } else {
                                resolved
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("module")
                                    .replace(['.', '-'], "_")
                            }
                        };
                        mangle_module_symbols(&mut dep_ast, &stem);
                        self.merge_dep(&mut merged, dep_ast);
                    }
                }
                BringPath::Stdlib(name) => {
                    let key = format!("__stdlib:{}", name);
                    let key_path = PathBuf::from(&key);
                    if !visited.contains(&key_path) {
                        visited.insert(key_path);
                        if let Some(src) = crate::stdlib::stdlib_source(name) {
                            let mut dep_ast = self.parse_source_from(
                                src,
                                &PathBuf::from(format!("<stdlib>/{name}.iris")),
                            )?;
                            for dep_bring in &dep_ast.brings {
                                queue.push_back((dep_bring.path.clone(), base_dir.to_path_buf()));
                            }
                            let mod_name = name.replace(['.', '-'], "_");
                            mangle_module_symbols(&mut dep_ast, &mod_name);
                            self.merge_dep(&mut merged, dep_ast);
                        }
                    }
                }
            }
        }

        Ok(merged)
    }

    /// Parse one file. With a source map enabled, its tokens are shifted to
    /// the file's base offset so its spans stay distinct after merging.
    fn parse_source_from(&self, src: &str, path: &Path) -> Result<AstModule, Error> {
        // Run preprocessor to resolve #if/#ifdef/#define directives; its
        // tokens carry the macros' values.
        let pp = crate::preprocessor::Preprocessor::new();
        let processed = pp
            .process_full(src, &display_path(path))
            .map_err(Error::Preprocessor)?;
        let mut tokens = processed.tokenize()?;
        let processed = processed.text;
        let mut base = 0;
        if let Some(map) = &self.source_map {
            base = map.borrow_mut().add(path.to_path_buf(), processed.clone());
            for token in &mut tokens {
                token.span.start.0 += base;
                token.span.end.0 += base;
            }
        }
        let mut parser = Parser::new(&tokens).with_source(display_path(path), &processed, base);
        let (module, errors) = parser.parse_module_recovering();
        if errors.is_empty() {
            return Ok(module);
        }
        // Every independent error travels in the result; the caller renders
        // them. Printing here put raw, unlocated copies on stderr ahead of the
        // rendered diagnostic -- and into the language server's and JSON
        // output, which must contain nothing else.
        // Spans of brought files are shifted by the file's base offset; the
        // hints index this file's own text, so they apply to the main file.
        let errors = if base == 0 {
            crate::parser::hints::improve(&processed, &tokens, errors)
        } else {
            errors
        };
        Err(Error::Parse(
            crate::error::ParseError::collect(errors).expect("errors is non-empty, checked above"),
        ))
    }

    fn resolve_file_path(
        &self,
        rel_path: &str,
        from_dir: &Path,
        search: &[PathBuf],
    ) -> Result<PathBuf, Error> {
        let find_entry_point = |dir: &Path| -> Option<PathBuf> {
            if dir.is_file() {
                return Some(dir.to_path_buf());
            }
            if dir.is_dir() {
                let lib = dir.join("lib.iris");
                if lib.exists() {
                    return Some(lib);
                }
                let main = dir.join("main.iris");
                if main.exists() {
                    return Some(main);
                }
                let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("lib");
                let named = dir.join(format!("{}.iris", name));
                if named.exists() {
                    return Some(named);
                }
            }
            if dir.extension().is_none() {
                let with_ext = dir.with_extension("iris");
                if with_ext.is_file() {
                    return Some(with_ext);
                }
            }
            None
        };

        if let Some(res) = find_entry_point(&from_dir.join(rel_path)) {
            return res.canonicalize().map_err(Error::Io);
        }

        let mut cur = Some(from_dir.to_path_buf());
        while let Some(dir) = cur {
            let dep_dir = dir.join(".iris").join("deps").join(rel_path);
            if let Some(res) = find_entry_point(&dep_dir) {
                return res.canonicalize().map_err(Error::Io);
            }
            cur = dir.parent().map(|p| p.to_path_buf());
        }

        for dir in search {
            if let Some(res) = find_entry_point(&dir.join(rel_path)) {
                return res.canonicalize().map_err(Error::Io);
            }
        }

        // Check iris_packages/ directories (up to 3 parent levels).
        {
            // Try both the raw rel_path and with .iris stripped (for directory packages).
            let stem = rel_path.trim_end_matches(".iris");
            let mut cur = Some(from_dir.to_path_buf());
            let mut depth = 0;
            while let Some(dir) = cur {
                let pkg_dir = dir.join("iris_packages").join(stem);
                if let Some(res) = find_entry_point(&pkg_dir) {
                    return res.canonicalize().map_err(Error::Io);
                }
                cur = dir.parent().map(|p| p.to_path_buf());
                depth += 1;
                if depth >= 3 {
                    break;
                }
            }
        }

        Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("cannot find brought module: {}", rel_path),
        )))
    }

    fn merge_dep(&self, main_ast: &mut AstModule, dep: AstModule) {
        main_ast.private_items.extend(dep.private_items);
        main_ast.extern_fns.extend(dep.extern_fns);
        main_ast.functions.extend(dep.functions);
        main_ast.structs.extend(dep.structs);
        main_ast.enums.extend(dep.enums);
        main_ast.consts.extend(dep.consts);
        // Program state is merged by name; a clash is reported by the lowerer.
        main_ast.state_vars.extend(dep.state_vars);
        main_ast.type_aliases.extend(dep.type_aliases);
        main_ast.traits.extend(dep.traits);
        main_ast.impls.extend(dep.impls);
        main_ast.models.extend(dep.models);
    }
}

// ---------------------------------------------------------------------------
// Module Namespace Mangling AST Rewriter
// ---------------------------------------------------------------------------

fn collect_local_symbols(ast: &AstModule) -> HashSet<String> {
    let mut symbols = HashSet::new();
    for f in &ast.functions {
        symbols.insert(f.name.name.clone());
    }
    for s in &ast.structs {
        symbols.insert(s.name.name.clone());
    }
    for e in &ast.enums {
        symbols.insert(e.name.name.clone());
    }
    for c in &ast.consts {
        symbols.insert(c.name.name.clone());
    }
    for ta in &ast.type_aliases {
        symbols.insert(ta.name.clone());
    }
    for t in &ast.traits {
        symbols.insert(t.name.name.clone());
    }
    for m in &ast.models {
        symbols.insert(m.name.name.clone());
    }
    symbols
}

fn rewrite_type(ty: &mut AstType, symbols: &HashSet<String>, prefix: &str) {
    match ty {
        AstType::Scalar(_, _) => {}
        AstType::Tensor { .. } => {}
        AstType::Named(ref mut name, _) => {
            if symbols.contains(name) {
                *name = format!("{}__{}", prefix, name);
            }
        }
        AstType::Tuple(ref mut tys, _) => {
            for t in tys {
                rewrite_type(t, symbols, prefix);
            }
        }
        AstType::Array { ref mut elem, .. } => {
            rewrite_type(elem, symbols, prefix);
        }
        AstType::Option(ref mut elem, _) => {
            rewrite_type(elem, symbols, prefix);
        }
        AstType::Result(ref mut ok, ref mut err, _) => {
            rewrite_type(ok, symbols, prefix);
            rewrite_type(err, symbols, prefix);
        }
        AstType::Chan(ref mut elem, _) => {
            rewrite_type(elem, symbols, prefix);
        }
        AstType::Atomic(ref mut elem, _) => {
            rewrite_type(elem, symbols, prefix);
        }
        AstType::Mutex(ref mut elem, _) => {
            rewrite_type(elem, symbols, prefix);
        }
        AstType::Grad(ref mut elem, _) => {
            rewrite_type(elem, symbols, prefix);
        }
        AstType::Sparse(ref mut elem, _) => {
            rewrite_type(elem, symbols, prefix);
        }
        AstType::List(ref mut elem, _) => {
            rewrite_type(elem, symbols, prefix);
        }
        AstType::Map(ref mut k, ref mut v, _) => {
            rewrite_type(k, symbols, prefix);
            rewrite_type(v, symbols, prefix);
        }
        AstType::Generic {
            ref mut name,
            ref mut args,
            ..
        } => {
            if symbols.contains(name) {
                *name = format!("{}__{}", prefix, name);
            }
            for arg in args {
                rewrite_type(arg, symbols, prefix);
            }
        }
        AstType::Fn {
            ref mut params,
            ref mut ret,
            ..
        } => {
            for p in params {
                rewrite_type(p, symbols, prefix);
            }
            rewrite_type(ret, symbols, prefix);
        }
        AstType::ConstInt(_, _) => {}
        AstType::AssocType { .. } => {}
        AstType::WeakRef(ref mut elem, _) => {
            rewrite_type(elem, symbols, prefix);
        }
        AstType::DynTrait {
            ref mut trait_name, ..
        } => {
            if symbols.contains(trait_name) {
                *trait_name = format!("{}__{}", prefix, trait_name);
            }
        }
        AstType::MaskEffectType { .. } => {}
        AstType::Ref(inner, _) => {
            rewrite_type(inner, symbols, prefix);
        }
        AstType::RefMut(inner, _) => {
            rewrite_type(inner, symbols, prefix);
        }
        AstType::Slice(inner, _) => {
            rewrite_type(inner, symbols, prefix);
        }
    }
}

/// Names bound by the local scopes enclosing the code being rewritten.
///
/// Merging a brought module renames its items to `module__name`, but only
/// where a name still refers to the item: a parameter or local named like one
/// of the module's functions shadows it, as it does during lowering
/// (`resolve_unqualified_name` consults the scope first). Rewriting the
/// shadowed uses too turned `std.ros2`'s `msg_type: str` parameters into
/// references to its `msg_type` function.
///
/// Each binding records whether it is callable -- a parameter of function
/// type, or a `val`/`var` annotated with one or initialised by a lambda --
/// because a call `name(...)` only reaches the local when it holds a closure;
/// otherwise the lowerer calls the function of that name, and so must find
/// it under its mangled name.
type BoundNames = Vec<(String, bool)>;

fn is_bound(bound: &BoundNames, name: &str) -> bool {
    bound.iter().any(|(b, _)| b == name)
}

/// Whether the innermost binding of `name` holds a function value.
fn is_callable_binding(bound: &BoundNames, name: &str) -> bool {
    bound
        .iter()
        .rev()
        .find(|(b, _)| b == name)
        .is_some_and(|(_, callable)| *callable)
}

fn is_fn_type(ty: &AstType) -> bool {
    matches!(ty, AstType::Fn { .. })
}

fn plain(name: &str) -> (String, bool) {
    (name.to_string(), false)
}

fn qualify(name: &mut String, symbols: &HashSet<String>, prefix: &str, bound: &BoundNames) {
    if symbols.contains(name.as_str()) && !is_bound(bound, name) {
        *name = format!("{}__{}", prefix, name);
    }
}

fn qualify_callee(name: &mut String, symbols: &HashSet<String>, prefix: &str, bound: &BoundNames) {
    if symbols.contains(name.as_str()) && !is_callable_binding(bound, name) {
        *name = format!("{}__{}", prefix, name);
    }
}

/// The scope a function body starts in: its parameters.
fn param_bindings(params: &[crate::parser::ast::AstParam]) -> BoundNames {
    params
        .iter()
        .map(|p| (p.name.name.clone(), is_fn_type(&p.ty)))
        .collect()
}

/// The variables a `when` pattern binds.
fn pattern_bindings(pat: &AstWhenPattern, out: &mut BoundNames) {
    match pat {
        AstWhenPattern::EnumVariant { bindings, .. } => {
            out.extend(bindings.iter().map(|b| plain(b)))
        }
        AstWhenPattern::OptionSome { binding }
        | AstWhenPattern::ResultOk { binding }
        | AstWhenPattern::ResultErr { binding } => {
            if let Some(b) = binding {
                out.push(plain(b));
            }
        }
        AstWhenPattern::Tuple(pats) | AstWhenPattern::Or(pats) => {
            for p in pats {
                pattern_bindings(p, out);
            }
        }
        AstWhenPattern::Slice { prefix, rest } => {
            for p in prefix {
                pattern_bindings(p, out);
            }
            if let Some(r) = rest {
                out.push(plain(r));
            }
        }
        AstWhenPattern::Binding { name, pattern, .. } => {
            out.push(plain(name));
            pattern_bindings(pattern, out);
        }
        AstWhenPattern::Struct { fields, .. } => {
            for (_, p) in fields {
                pattern_bindings(p, out);
            }
        }
        AstWhenPattern::OptionNone
        | AstWhenPattern::Wildcard
        | AstWhenPattern::IntLit(_)
        | AstWhenPattern::FloatLit(_)
        | AstWhenPattern::BoolLit(_)
        | AstWhenPattern::StringLit(_)
        | AstWhenPattern::Range { .. } => {}
    }
}

fn rewrite_handler_arms(
    arms: &mut [AstHandlerArm],
    symbols: &HashSet<String>,
    prefix: &str,
    bound: &mut BoundNames,
) {
    for arm in arms {
        let mark = bound.len();
        bound.extend(arm.params.iter().map(|p| plain(&p.name)));
        if let Some(resume) = &arm.resume_param {
            // `resume(v)` calls it, so it is callable.
            bound.push((resume.name.clone(), true));
        }
        rewrite_expr(&mut arm.body, symbols, prefix, bound);
        bound.truncate(mark);
    }
}

fn rewrite_expr(
    expr: &mut AstExpr,
    symbols: &HashSet<String>,
    prefix: &str,
    bound: &mut BoundNames,
) {
    match expr {
        AstExpr::Ident(ref mut ident) => {
            qualify(&mut ident.name, symbols, prefix, bound);
        }
        AstExpr::IntLit { .. }
        | AstExpr::FloatLit { .. }
        | AstExpr::BoolLit { .. }
        | AstExpr::StringLit { .. } => {}
        AstExpr::BinOp {
            ref mut lhs,
            ref mut rhs,
            ..
        } => {
            rewrite_expr(lhs, symbols, prefix, bound);
            rewrite_expr(rhs, symbols, prefix, bound);
        }
        AstExpr::Call {
            ref mut callee,
            ref mut args,
            ..
        } => {
            // Only a local holding a closure shadows a module function here.
            qualify_callee(&mut callee.name, symbols, prefix, bound);
            for arg in args {
                rewrite_expr(arg, symbols, prefix, bound);
            }
        }
        AstExpr::UnaryOp { ref mut expr, .. } => {
            rewrite_expr(expr, symbols, prefix, bound);
        }
        AstExpr::If {
            ref mut cond,
            ref mut then_block,
            ref mut else_block,
            ..
        } => {
            rewrite_expr(cond, symbols, prefix, bound);
            rewrite_block(then_block, symbols, prefix, bound);
            if let Some(ref mut else_b) = else_block {
                rewrite_block(else_b, symbols, prefix, bound);
            }
        }
        AstExpr::Block(ref mut block) => {
            rewrite_block(block, symbols, prefix, bound);
        }
        AstExpr::Index {
            ref mut base,
            ref mut indices,
            ..
        } => {
            rewrite_expr(base, symbols, prefix, bound);
            for idx in indices {
                rewrite_expr(idx, symbols, prefix, bound);
            }
        }
        AstExpr::Cast {
            ref mut expr,
            ref mut ty,
            ..
        } => {
            rewrite_expr(expr, symbols, prefix, bound);
            rewrite_type(ty, symbols, prefix);
        }
        AstExpr::StructLit {
            ref mut name,
            ref mut fields,
            ref mut spread,
            ..
        } => {
            if let Some(ref mut s) = spread {
                rewrite_expr(s, symbols, prefix, bound);
            }
            // A type name, which no local can shadow.
            if symbols.contains(name) {
                *name = format!("{}__{}", prefix, name);
            }
            for (_, ref mut val) in fields {
                rewrite_expr(val, symbols, prefix, bound);
            }
        }
        AstExpr::FieldAccess { ref mut base, .. } => {
            rewrite_expr(base, symbols, prefix, bound);
        }
        AstExpr::When {
            ref mut scrutinee,
            ref mut arms,
            ..
        } => {
            rewrite_expr(scrutinee, symbols, prefix, bound);
            for arm in arms {
                rewrite_when_pattern(&mut arm.pattern, symbols, prefix);
                let mark = bound.len();
                pattern_bindings(&arm.pattern, bound);
                if let Some(ref mut guard) = arm.guard {
                    rewrite_expr(guard, symbols, prefix, bound);
                }
                rewrite_expr(&mut arm.body, symbols, prefix, bound);
                bound.truncate(mark);
            }
        }
        AstExpr::Tuple {
            ref mut elements, ..
        } => {
            for elem in elements {
                rewrite_expr(elem, symbols, prefix, bound);
            }
        }
        AstExpr::TupleIndex { ref mut base, .. } => {
            rewrite_expr(base, symbols, prefix, bound);
        }
        AstExpr::ArrayLit { ref mut elems, .. } => {
            for elem in elems {
                rewrite_expr(elem, symbols, prefix, bound);
            }
        }
        AstExpr::Lambda {
            ref mut params,
            ref mut body,
            ..
        } => {
            for param in params.iter_mut() {
                rewrite_type(&mut param.ty, symbols, prefix);
                if let Some(ref mut def_val) = param.default {
                    rewrite_expr(def_val, symbols, prefix, bound);
                }
            }
            let mark = bound.len();
            bound.extend(param_bindings(params));
            rewrite_expr(body, symbols, prefix, bound);
            bound.truncate(mark);
        }
        AstExpr::Await { ref mut expr, .. } => {
            rewrite_expr(expr, symbols, prefix, bound);
        }
        AstExpr::Try { ref mut expr, .. } => {
            rewrite_expr(expr, symbols, prefix, bound);
        }
        AstExpr::MethodCall {
            ref mut base,
            ref mut args,
            ..
        } => {
            rewrite_expr(base, symbols, prefix, bound);
            for arg in args {
                rewrite_expr(arg, symbols, prefix, bound);
            }
        }
        AstExpr::Mask { ref mut body, .. } => {
            rewrite_block(body, symbols, prefix, bound);
        }
        AstExpr::Handle {
            ref mut expr,
            ref mut arms,
            ..
        } => {
            rewrite_expr(expr, symbols, prefix, bound);
            rewrite_handler_arms(arms, symbols, prefix, bound);
        }
        AstExpr::NullCoal {
            ref mut expr,
            ref mut default,
            ..
        } => {
            rewrite_expr(expr, symbols, prefix, bound);
            rewrite_expr(default, symbols, prefix, bound);
        }
        AstExpr::MapLiteral {
            ref mut entries, ..
        } => {
            for (k, v) in entries {
                rewrite_expr(k, symbols, prefix, bound);
                rewrite_expr(v, symbols, prefix, bound);
            }
        }
        AstExpr::Ref { ref mut expr, .. } => {
            rewrite_expr(expr, symbols, prefix, bound);
        }
        AstExpr::RefMut { ref mut expr, .. } => {
            rewrite_expr(expr, symbols, prefix, bound);
        }
        AstExpr::TryCatch {
            ref mut body,
            ref catch_param,
            ref mut catch_body,
            ..
        } => {
            rewrite_expr(body, symbols, prefix, bound);
            let mark = bound.len();
            bound.push(plain(catch_param));
            rewrite_expr(catch_body, symbols, prefix, bound);
            bound.truncate(mark);
        }
        AstExpr::Raise { ref mut args, .. } => {
            for a in args {
                rewrite_expr(a, symbols, prefix, bound);
            }
        }
        AstExpr::Deref { ref mut expr, .. } => {
            rewrite_expr(expr, symbols, prefix, bound);
        }
        AstExpr::Move { ref mut expr, .. } => {
            rewrite_expr(expr, symbols, prefix, bound);
        }
        AstExpr::Unsafe { ref mut body, .. } => {
            rewrite_expr(body, symbols, prefix, bound);
        }
        AstExpr::Splat { ref mut expr, .. } => {
            rewrite_expr(expr, symbols, prefix, bound);
        }
        AstExpr::MacroCall { .. } => {
            // Macro calls should have been expanded before rewriting;
            // if one remains, leave it (will be caught downstream or expanded on next pass).
        }
    }
}

fn rewrite_when_pattern(pat: &mut AstWhenPattern, symbols: &HashSet<String>, prefix: &str) {
    match pat {
        AstWhenPattern::EnumVariant {
            ref mut enum_name, ..
        } => {
            if symbols.contains(enum_name) {
                *enum_name = format!("{}__{}", prefix, enum_name);
            }
        }
        AstWhenPattern::OptionSome { .. } => {}
        AstWhenPattern::OptionNone => {}
        AstWhenPattern::ResultOk { .. } => {}
        AstWhenPattern::ResultErr { .. } => {}
        AstWhenPattern::Wildcard => {}
        AstWhenPattern::IntLit(_) => {}
        AstWhenPattern::FloatLit(_) => {}
        AstWhenPattern::BoolLit(_) => {}
        AstWhenPattern::StringLit(_) => {}
        AstWhenPattern::Tuple(ref mut pats) => {
            for p in pats {
                rewrite_when_pattern(p, symbols, prefix);
            }
        }
        AstWhenPattern::Range { .. } => {}
        AstWhenPattern::Or(ref mut pats) => {
            for p in pats {
                rewrite_when_pattern(p, symbols, prefix);
            }
        }
        AstWhenPattern::Slice {
            prefix: ref mut prefix_pats,
            ..
        } => {
            for p in prefix_pats {
                rewrite_when_pattern(p, symbols, prefix);
            }
        }
        AstWhenPattern::Binding {
            ref mut pattern, ..
        } => {
            rewrite_when_pattern(pattern, symbols, prefix);
        }
        AstWhenPattern::Struct { .. } => {}
    }
}

/// A block is a scope: its `val`/`var` bindings end with it.
fn rewrite_block(
    block: &mut AstBlock,
    symbols: &HashSet<String>,
    prefix: &str,
    bound: &mut BoundNames,
) {
    let mark = bound.len();
    for stmt in &mut block.stmts {
        rewrite_stmt(stmt, symbols, prefix, bound);
    }
    if let Some(ref mut tail) = block.tail {
        rewrite_expr(tail, symbols, prefix, bound);
    }
    bound.truncate(mark);
}

/// Rewrites one statement; a binding it introduces stays in `bound` for the
/// rest of the enclosing block.
fn rewrite_stmt(
    stmt: &mut AstStmt,
    symbols: &HashSet<String>,
    prefix: &str,
    bound: &mut BoundNames,
) {
    match stmt {
        AstStmt::Let {
            ref name,
            ref mut ty,
            ref mut init,
            ..
        } => {
            if let Some(ref mut t) = ty {
                rewrite_type(t, symbols, prefix);
            }
            rewrite_expr(init, symbols, prefix, bound);
            let callable = ty.as_ref().is_some_and(is_fn_type)
                || matches!(init.as_ref(), AstExpr::Lambda { .. });
            bound.push((name.name.clone(), callable));
        }
        AstStmt::Expr(ref mut expr) => {
            rewrite_expr(expr, symbols, prefix, bound);
        }
        AstStmt::While {
            ref mut cond,
            ref mut body,
            ..
        } => {
            rewrite_expr(cond, symbols, prefix, bound);
            rewrite_block(body, symbols, prefix, bound);
        }
        AstStmt::Loop { ref mut body, .. } => {
            rewrite_block(body, symbols, prefix, bound);
        }
        AstStmt::Break { .. } | AstStmt::Continue { .. } => {}
        AstStmt::Yield { ref mut expr, .. } => {
            rewrite_expr(expr, symbols, prefix, bound);
        }
        AstStmt::ForRange {
            ref var,
            ref mut start,
            ref mut end,
            ref mut step,
            ref mut body,
            ..
        } => {
            rewrite_expr(start, symbols, prefix, bound);
            rewrite_expr(end, symbols, prefix, bound);
            if let Some(ref mut s) = step {
                rewrite_expr(s, symbols, prefix, bound);
            }
            let mark = bound.len();
            bound.push(plain(&var.name));
            rewrite_block(body, symbols, prefix, bound);
            bound.truncate(mark);
        }
        AstStmt::Assign {
            ref mut target,
            ref mut value,
            ..
        } => {
            rewrite_expr(target, symbols, prefix, bound);
            rewrite_expr(value, symbols, prefix, bound);
        }
        AstStmt::LetTuple {
            ref names,
            ref mut init,
            ..
        } => {
            rewrite_expr(init, symbols, prefix, bound);
            bound.extend(names.iter().map(|n| plain(&n.name)));
        }
        AstStmt::Return { ref mut value, .. } => {
            if let Some(ref mut val) = value {
                rewrite_expr(val, symbols, prefix, bound);
            }
        }
        AstStmt::Spawn {
            ref mut body,
            ref mut group,
            ..
        } => {
            if let Some(ref mut g) = group {
                rewrite_expr(g, symbols, prefix, bound);
            }
            let mark = bound.len();
            for s in body {
                rewrite_stmt(s, symbols, prefix, bound);
            }
            bound.truncate(mark);
        }
        AstStmt::ParFor {
            ref var,
            ref mut start,
            ref mut end,
            ref mut body,
            ..
        } => {
            rewrite_expr(start, symbols, prefix, bound);
            rewrite_expr(end, symbols, prefix, bound);
            let mark = bound.len();
            bound.push(plain(&var.name));
            rewrite_block(body, symbols, prefix, bound);
            bound.truncate(mark);
        }
        AstStmt::ForEach {
            ref var,
            ref mut iter,
            ref mut body,
            ..
        } => {
            rewrite_expr(iter, symbols, prefix, bound);
            let mark = bound.len();
            bound.push(plain(&var.name));
            rewrite_block(body, symbols, prefix, bound);
            bound.truncate(mark);
        }
        AstStmt::MaskStmt { ref mut body, .. } => {
            rewrite_block(body, symbols, prefix, bound);
        }
        AstStmt::HandleStmt {
            ref mut expr,
            ref mut arms,
            ..
        } => {
            rewrite_expr(expr, symbols, prefix, bound);
            rewrite_handler_arms(arms, symbols, prefix, bound);
        }
        AstStmt::Defer { ref mut expr, .. } => {
            rewrite_expr(expr, symbols, prefix, bound);
        }
        AstStmt::Select {
            ref mut arms,
            ref mut default,
            ..
        } => {
            for arm in arms {
                rewrite_expr(&mut arm.channel, symbols, prefix, bound);
                let mark = bound.len();
                bound.push(plain(&arm.binding));
                rewrite_block(&mut arm.body, symbols, prefix, bound);
                bound.truncate(mark);
            }
            if let Some(ref mut d) = default {
                rewrite_block(d, symbols, prefix, bound);
            }
        }
    }
}

/// Records the mangled names of this module's non-`pub` items **onto the module
/// itself**, so the information survives the merge into the flat AST.
///
/// Brought modules are merged into one flat AST and every symbol is renamed to
/// `module__name`. References *inside* the defining module are rewritten to the
/// mangled name directly, so they keep working; a reference from another module
/// goes through `resolve_unqualified_name`, which used to accept any
/// `prefix__name` match regardless of visibility. That is how a private function
/// stayed callable across a module boundary (#13).
///
/// This lived in a `thread_local` first, which silently never fired: compilation
/// runs on a spawned thread (`lib.rs` gives it a larger stack), so the mangling
/// and the resolution happen on *different* threads and the set read back was
/// always empty. A process-global would have worked but is shared by tests
/// compiling concurrently. Carrying it on the AST is both thread-safe and
/// correct when the merged AST is served from cache.
///
/// Traits are absent because `AstTraitDef` carries no `is_pub` — a trait cannot
/// currently be declared private, so there is nothing to enforce.
fn record_private_items(ast: &mut AstModule, mod_name: &str) {
    let mut found: Vec<String> = Vec::new();
    {
        let mut note = |is_pub: bool, name: &str| {
            if !is_pub {
                found.push(format!("{}__{}", mod_name, name));
            }
        };
        for f in &ast.functions {
            note(f.is_pub, &f.name.name);
        }
        for st in &ast.structs {
            note(st.is_pub, &st.name.name);
        }
        for e in &ast.enums {
            note(e.is_pub, &e.name.name);
        }
        for c in &ast.consts {
            note(c.is_pub, &c.name.name);
        }
        for ta in &ast.type_aliases {
            note(ta.is_pub, &ta.name);
        }
    }
    ast.private_items.extend(found);
}

pub(crate) fn mangle_module_symbols(ast: &mut AstModule, mod_name: &str) {
    // Before renaming, while the original names are still in place.
    record_private_items(ast, mod_name);
    let symbols = collect_local_symbols(ast);
    if symbols.is_empty() {
        return;
    }

    // 1. Mangle the definitions themselves
    for f in &mut ast.functions {
        if symbols.contains(&f.name.name) {
            f.name.name = format!("{}__{}", mod_name, f.name.name);
        }
        let mut bound = param_bindings(&f.params);
        rewrite_block(&mut f.body, &symbols, mod_name, &mut bound);
        for p in &mut f.params {
            rewrite_type(&mut p.ty, &symbols, mod_name);
            if let Some(ref mut def_val) = p.default {
                rewrite_expr(def_val, &symbols, mod_name, &mut BoundNames::new());
            }
        }
        rewrite_type(&mut f.return_ty, &symbols, mod_name);
    }

    for s in &mut ast.structs {
        if symbols.contains(&s.name.name) {
            s.name.name = format!("{}__{}", mod_name, s.name.name);
        }
        for f in &mut s.fields {
            rewrite_type(&mut f.ty, &symbols, mod_name);
        }
    }

    for e in &mut ast.enums {
        if symbols.contains(&e.name.name) {
            e.name.name = format!("{}__{}", mod_name, e.name.name);
        }
        for v in &mut e.variants {
            for f in &mut v.fields {
                rewrite_type(f, &symbols, mod_name);
            }
        }
    }

    for c in &mut ast.consts {
        if symbols.contains(&c.name.name) {
            c.name.name = format!("{}__{}", mod_name, c.name.name);
        }
        if let Some(ref mut t) = c.ty {
            rewrite_type(t, &symbols, mod_name);
        }
        rewrite_expr(&mut c.value, &symbols, mod_name, &mut BoundNames::new());
    }

    for ta in &mut ast.type_aliases {
        if symbols.contains(&ta.name) {
            ta.name = format!("{}__{}", mod_name, ta.name);
        }
        rewrite_type(&mut ta.ty, &symbols, mod_name);
    }

    for t in &mut ast.traits {
        if symbols.contains(&t.name.name) {
            t.name.name = format!("{}__{}", mod_name, t.name.name);
        }
        for m in &mut t.methods {
            for p in &mut m.params {
                rewrite_type(&mut p.ty, &symbols, mod_name);
            }
            rewrite_type(&mut m.return_ty, &symbols, mod_name);
        }
    }

    for m in &mut ast.models {
        if symbols.contains(&m.name.name) {
            m.name.name = format!("{}__{}", mod_name, m.name.name);
        }
    }

    for i in &mut ast.impls {
        if symbols.contains(&i.trait_name) {
            i.trait_name = format!("{}__{}", mod_name, i.trait_name);
        }
        if symbols.contains(&i.type_name) {
            i.type_name = format!("{}__{}", mod_name, i.type_name);
        }
        for f in &mut i.methods {
            if symbols.contains(&f.name.name) {
                f.name.name = format!("{}__{}", mod_name, f.name.name);
            }
            let mut bound = param_bindings(&f.params);
            rewrite_block(&mut f.body, &symbols, mod_name, &mut bound);
            for p in &mut f.params {
                rewrite_type(&mut p.ty, &symbols, mod_name);
                if let Some(ref mut def_val) = p.default {
                    rewrite_expr(def_val, &symbols, mod_name, &mut BoundNames::new());
                }
            }
            rewrite_type(&mut f.return_ty, &symbols, mod_name);
        }
    }
}

/// Check if an AST block contains any `yield` statements (recursive).
fn block_has_yield(block: &AstBlock) -> bool {
    stmts_have_yield(&block.stmts) || block.tail.as_ref().is_some_and(|e| expr_has_yield(e))
}

fn stmts_have_yield(stmts: &[AstStmt]) -> bool {
    stmts.iter().any(stmt_has_yield)
}

fn stmt_has_yield(stmt: &AstStmt) -> bool {
    match stmt {
        AstStmt::Yield { .. } => true,
        AstStmt::While { body, .. } => block_has_yield(body),
        AstStmt::Loop { body, .. } => block_has_yield(body),
        AstStmt::ForRange { body, .. } => block_has_yield(body),
        AstStmt::ForEach { body, .. } => block_has_yield(body),
        AstStmt::ParFor { body, .. } => block_has_yield(body),
        AstStmt::Spawn { body, .. } => body.iter().any(stmt_has_yield),
        AstStmt::MaskStmt { body, .. } => block_has_yield(body),
        AstStmt::Expr(e) => expr_has_yield(e),
        AstStmt::Return { value, .. } => value.as_ref().is_some_and(|e| expr_has_yield(e)),
        AstStmt::Let { init, .. } => expr_has_yield(init),
        AstStmt::LetTuple { init, .. } => expr_has_yield(init),
        AstStmt::Assign { target, value, .. } => expr_has_yield(target) || expr_has_yield(value),
        AstStmt::Defer { expr, .. } => expr_has_yield(expr),
        AstStmt::HandleStmt { expr, .. } => expr_has_yield(expr),
        AstStmt::Select { arms, default, .. } => {
            arms.iter()
                .any(|a| block_has_yield(&a.body) || expr_has_yield(&a.channel))
                || default.as_ref().is_some_and(|d| block_has_yield(d))
        }
        AstStmt::Break { .. } | AstStmt::Continue { .. } => false,
    }
}

fn expr_has_yield(expr: &AstExpr) -> bool {
    match expr {
        AstExpr::Block(b) => block_has_yield(b),
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            expr_has_yield(cond)
                || block_has_yield(then_block)
                || else_block.as_ref().is_some_and(block_has_yield)
        }
        AstExpr::When {
            scrutinee, arms, ..
        } => expr_has_yield(scrutinee) || arms.iter().any(|arm| expr_has_yield(&arm.body)),
        AstExpr::BinOp { lhs, rhs, .. } => expr_has_yield(lhs) || expr_has_yield(rhs),
        AstExpr::Call { args, .. } => args.iter().any(expr_has_yield),
        AstExpr::MethodCall { args, .. } => args.iter().any(expr_has_yield),
        AstExpr::Index { base, indices, .. } => {
            expr_has_yield(base) || indices.iter().any(expr_has_yield)
        }
        AstExpr::Tuple { elements, .. } => elements.iter().any(expr_has_yield),
        AstExpr::ArrayLit { elems, .. } => elems.iter().any(expr_has_yield),
        AstExpr::StructLit { fields, .. } => fields.iter().any(|(_, e)| expr_has_yield(e)),
        AstExpr::UnaryOp { expr: inner, .. } => expr_has_yield(inner),
        AstExpr::Lambda { body, .. } => expr_has_yield(body),
        AstExpr::Await { expr: inner, .. } => expr_has_yield(inner),
        AstExpr::Cast { expr: inner, .. } => expr_has_yield(inner),
        AstExpr::NullCoal {
            expr: inner,
            default,
            ..
        } => expr_has_yield(inner) || expr_has_yield(default),
        AstExpr::Mask { body, .. } => block_has_yield(body),
        AstExpr::Handle {
            expr: inner, arms, ..
        } => expr_has_yield(inner) || arms.iter().any(|arm| expr_has_yield(&arm.body)),
        AstExpr::TryCatch {
            body, catch_body, ..
        } => expr_has_yield(body) || expr_has_yield(catch_body),
        AstExpr::Raise { args, .. } => args.iter().any(expr_has_yield),
        AstExpr::Move { expr: inner, .. } => expr_has_yield(inner),
        AstExpr::Unsafe { body, .. } => expr_has_yield(body),
        AstExpr::Ref { expr: inner, .. }
        | AstExpr::RefMut { expr: inner, .. }
        | AstExpr::Deref { expr: inner, .. } => expr_has_yield(inner),
        AstExpr::Try { expr: inner, .. } => expr_has_yield(inner),
        AstExpr::FieldAccess { base, .. } => expr_has_yield(base),
        AstExpr::TupleIndex { base, .. } => expr_has_yield(base),
        AstExpr::MapLiteral { entries, .. } => entries
            .iter()
            .any(|(k, v)| expr_has_yield(k) || expr_has_yield(v)),
        _ => false,
    }
}

/// Replace `yield expr` with `push(__acc, expr)` in a block (recursive).
fn replace_yield_in_block(block: &mut AstBlock, acc: &str) {
    replace_yield_in_stmts(&mut block.stmts, acc);
    if let Some(ref mut tail) = block.tail {
        replace_yield_in_expr(tail, acc);
    }
}

fn replace_yield_in_stmts(stmts: &mut [AstStmt], acc: &str) {
    for stmt in stmts.iter_mut() {
        replace_yield_in_stmt(stmt, acc);
    }
}

fn replace_yield_in_stmt(stmt: &mut AstStmt, acc: &str) {
    match stmt {
        AstStmt::Yield { expr, .. } => {
            let yielded = std::mem::replace(
                expr,
                Box::new(AstExpr::IntLit {
                    value: 0,
                    span: Span::new(0, 0),
                }),
            );
            *stmt = AstStmt::Expr(Box::new(AstExpr::Call {
                callee: Ident {
                    name: "push".to_string(),
                    span: Span::new(0, 0),
                },
                args: vec![
                    AstExpr::Ident(Ident {
                        name: acc.to_string(),
                        span: Span::new(0, 0),
                    }),
                    *yielded,
                ],
                named_args: vec![],
                span: Span::new(0, 0),
            }));
        }
        AstStmt::While { body, .. } => replace_yield_in_block(body, acc),
        AstStmt::Loop { body, .. } => replace_yield_in_block(body, acc),
        AstStmt::ForRange { body, .. } => replace_yield_in_block(body, acc),
        AstStmt::ForEach { body, .. } => replace_yield_in_block(body, acc),
        AstStmt::ParFor { body, .. } => replace_yield_in_block(body, acc),
        AstStmt::Spawn { body, .. } => {
            for s in body.iter_mut() {
                replace_yield_in_stmt(s, acc);
            }
        }
        AstStmt::MaskStmt { body, .. } => replace_yield_in_block(body, acc),
        AstStmt::Select { arms, default, .. } => {
            for arm in arms.iter_mut() {
                replace_yield_in_block(&mut arm.body, acc);
            }
            if let Some(ref mut d) = default {
                replace_yield_in_block(d, acc);
            }
        }
        AstStmt::Expr(ref mut e) => replace_yield_in_expr(e, acc),
        AstStmt::Let { init, .. } => replace_yield_in_expr(init, acc),
        AstStmt::LetTuple { init, .. } => replace_yield_in_expr(init, acc),
        AstStmt::Return { value, .. } => {
            if let Some(ref mut v) = value {
                replace_yield_in_expr(v, acc);
            }
        }
        AstStmt::Assign { target, value, .. } => {
            replace_yield_in_expr(target, acc);
            replace_yield_in_expr(value, acc);
        }
        AstStmt::Defer { expr, .. } => replace_yield_in_expr(expr, acc),
        AstStmt::HandleStmt { expr, .. } => replace_yield_in_expr(expr, acc),
        AstStmt::Break { .. } | AstStmt::Continue { .. } => {}
    }
}

fn replace_yield_in_expr(expr: &mut AstExpr, acc: &str) {
    match expr {
        AstExpr::Block(b) => replace_yield_in_block(b, acc),
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            replace_yield_in_expr(cond, acc);
            replace_yield_in_block(then_block, acc);
            if let Some(ref mut b) = else_block {
                replace_yield_in_block(b, acc);
            }
        }
        AstExpr::When {
            scrutinee, arms, ..
        } => {
            replace_yield_in_expr(scrutinee, acc);
            for arm in arms.iter_mut() {
                replace_yield_in_expr(&mut arm.body, acc);
            }
        }
        AstExpr::BinOp { lhs, rhs, .. } => {
            replace_yield_in_expr(lhs, acc);
            replace_yield_in_expr(rhs, acc);
        }
        AstExpr::Call { args, .. } => {
            for a in args.iter_mut() {
                replace_yield_in_expr(a, acc);
            }
        }
        AstExpr::MethodCall { args, .. } => {
            for a in args.iter_mut() {
                replace_yield_in_expr(a, acc);
            }
        }
        AstExpr::Index { base, indices, .. } => {
            replace_yield_in_expr(base, acc);
            for i in indices.iter_mut() {
                replace_yield_in_expr(i, acc);
            }
        }
        AstExpr::Tuple { elements, .. } => {
            for e in elements.iter_mut() {
                replace_yield_in_expr(e, acc);
            }
        }
        AstExpr::ArrayLit { elems, .. } => {
            for e in elems.iter_mut() {
                replace_yield_in_expr(e, acc);
            }
        }
        AstExpr::StructLit { fields, .. } => {
            for (_, e) in fields.iter_mut() {
                replace_yield_in_expr(e, acc);
            }
        }
        AstExpr::UnaryOp { expr: inner, .. } => replace_yield_in_expr(inner, acc),
        AstExpr::Lambda { body, .. } => replace_yield_in_expr(body, acc),
        AstExpr::Await { expr: inner, .. } => replace_yield_in_expr(inner, acc),
        AstExpr::Cast { expr: inner, .. } => replace_yield_in_expr(inner, acc),
        AstExpr::NullCoal {
            expr: inner,
            default,
            ..
        } => {
            replace_yield_in_expr(inner, acc);
            replace_yield_in_expr(default, acc);
        }
        AstExpr::Mask { body, .. } => replace_yield_in_block(body, acc),
        AstExpr::Handle {
            expr: inner, arms, ..
        } => {
            replace_yield_in_expr(inner, acc);
            for arm in arms.iter_mut() {
                replace_yield_in_expr(&mut arm.body, acc);
            }
        }
        AstExpr::TryCatch {
            body, catch_body, ..
        } => {
            replace_yield_in_expr(body, acc);
            replace_yield_in_expr(catch_body, acc);
        }
        AstExpr::Raise { args, .. } => {
            for a in args.iter_mut() {
                replace_yield_in_expr(a, acc);
            }
        }
        AstExpr::Move { expr: inner, .. } => replace_yield_in_expr(inner, acc),
        AstExpr::Unsafe { body, .. } => replace_yield_in_expr(body, acc),
        AstExpr::Ref { expr: inner, .. }
        | AstExpr::RefMut { expr: inner, .. }
        | AstExpr::Deref { expr: inner, .. } => replace_yield_in_expr(inner, acc),
        AstExpr::Try { expr: inner, .. } => replace_yield_in_expr(inner, acc),
        AstExpr::FieldAccess { base, .. } => replace_yield_in_expr(base, acc),
        AstExpr::TupleIndex { base, .. } => replace_yield_in_expr(base, acc),
        AstExpr::MapLiteral { entries, .. } => {
            for (k, v) in entries.iter_mut() {
                replace_yield_in_expr(k, acc);
                replace_yield_in_expr(v, acc);
            }
        }
        _ => {}
    }
}

/// Desugar `yield <expr>` statements into list accumulator pattern:
///   `var __iris_yield = list()` prepended
///   `yield x` → `push(__iris_yield, x)`
///   tail becomes `__iris_yield`
pub(crate) fn desugar_yield(ast: &mut AstModule) {
    let acc_name = "__iris_yield";
    let mk_list_call = || -> AstExpr {
        AstExpr::Call {
            callee: Ident {
                name: "list".to_string(),
                span: Span::new(0, 0),
            },
            args: vec![],
            named_args: vec![],
            span: Span::new(0, 0),
        }
    };
    let mk_acc_ident = || -> AstExpr {
        AstExpr::Ident(Ident {
            name: acc_name.to_string(),
            span: Span::new(0, 0),
        })
    };
    // The accumulator has the type the function declares it returns
    // (`-> list<i64>`), which nothing else would tell its empty `list()`.
    let mk_init_stmt = |ret: &AstType| -> AstStmt {
        AstStmt::Let {
            name: Ident {
                name: acc_name.to_string(),
                span: Span::new(0, 0),
            },
            ty: matches!(ret, AstType::List(..)).then(|| ret.clone()),
            init: Box::new(mk_list_call()),
            is_var: false,
            span: Span::new(0, 0),
        }
    };
    for f in &mut ast.functions {
        if stmts_have_yield(&f.body.stmts)
            || f.body.tail.as_ref().is_some_and(|e| expr_has_yield(e))
        {
            f.body.stmts.insert(0, mk_init_stmt(&f.return_ty));
            replace_yield_in_stmts(&mut f.body.stmts, acc_name);
            if let Some(ref mut tail) = f.body.tail {
                replace_yield_in_expr(tail, acc_name);
            }
            if f.body.tail.is_none() {
                f.body.tail = Some(Box::new(mk_acc_ident()));
            }
        }
    }
    for i in &mut ast.impls {
        for f in &mut i.methods {
            if stmts_have_yield(&f.body.stmts)
                || f.body.tail.as_ref().is_some_and(|e| expr_has_yield(e))
            {
                f.body.stmts.insert(0, mk_init_stmt(&f.return_ty));
                replace_yield_in_stmts(&mut f.body.stmts, acc_name);
                if let Some(ref mut tail) = f.body.tail {
                    replace_yield_in_expr(tail, acc_name);
                }
                if f.body.tail.is_none() {
                    f.body.tail = Some(Box::new(mk_acc_ident()));
                }
            }
        }
    }
}

/// Walk every expression in a block, replacing `Self` type references with `concrete`.
fn replace_self_in_block(block: &mut AstBlock, concrete: &str) {
    let type_subs = [(
        String::from("Self"),
        AstType::Named(concrete.to_string(), Span::new(0, 0)),
    )]
    .into_iter()
    .collect::<HashMap<_, _>>();
    for stmt in &mut block.stmts {
        replace_self_in_stmt(stmt, &type_subs, concrete);
    }
    if let Some(ref mut tail) = block.tail {
        replace_self_in_expr(tail, &type_subs, concrete);
    }
}

fn replace_self_in_stmt(stmt: &mut AstStmt, type_subs: &HashMap<String, AstType>, concrete: &str) {
    match stmt {
        AstStmt::Let { ty, init, .. } => {
            if let Some(ref mut t) = ty {
                *t = substitute_ast_type(t, type_subs, &HashMap::new());
            }
            replace_self_in_expr(init, type_subs, concrete);
        }
        AstStmt::Expr(e) => replace_self_in_expr(e, type_subs, concrete),
        AstStmt::While { body, cond, .. } => {
            replace_self_in_expr(cond, type_subs, concrete);
            replace_self_in_block(body, concrete);
        }
        AstStmt::Loop { body, .. } => {
            replace_self_in_block(body, concrete);
        }
        AstStmt::ForRange {
            start,
            end,
            step,
            body,
            ..
        } => {
            replace_self_in_expr(start, type_subs, concrete);
            replace_self_in_expr(end, type_subs, concrete);
            if let Some(ref mut s) = step {
                replace_self_in_expr(s, type_subs, concrete);
            }
            replace_self_in_block(body, concrete);
        }
        AstStmt::Assign { target, value, .. } => {
            replace_self_in_expr(target, type_subs, concrete);
            replace_self_in_expr(value, type_subs, concrete);
        }
        AstStmt::LetTuple { init, .. } => {
            replace_self_in_expr(init, type_subs, concrete);
        }
        AstStmt::Return { value, .. } => {
            if let Some(ref mut v) = value {
                replace_self_in_expr(v, type_subs, concrete);
            }
        }
        AstStmt::Spawn { body, group, .. } => {
            if let Some(ref mut g) = group {
                replace_self_in_expr(g, type_subs, concrete);
            }
            // Spawn body is Vec<AstStmt> not AstBlock
            let mut temp_block = AstBlock {
                stmts: std::mem::take(body),
                tail: None,
                span: Span::new(0, 0),
            };
            replace_self_in_block(&mut temp_block, concrete);
            *body = temp_block.stmts;
        }
        AstStmt::ParFor { body, .. } => {
            replace_self_in_block(body, concrete);
        }
        AstStmt::ForEach { iter, body, .. } => {
            replace_self_in_expr(iter, type_subs, concrete);
            replace_self_in_block(body, concrete);
        }
        AstStmt::MaskStmt { body, .. } => {
            replace_self_in_block(body, concrete);
        }
        AstStmt::HandleStmt {
            expr,
            arms: _,
            return_ty,
            ..
        } => {
            replace_self_in_expr(expr, type_subs, concrete);
            **return_ty = substitute_ast_type(return_ty, type_subs, &HashMap::new());
        }
        AstStmt::Defer { expr, .. } => {
            replace_self_in_expr(expr, type_subs, concrete);
        }
        AstStmt::Select { arms, default, .. } => {
            for arm in arms.iter_mut() {
                replace_self_in_expr(&mut arm.channel, type_subs, concrete);
                replace_self_in_block(&mut arm.body, concrete);
            }
            if let Some(ref mut d) = default {
                replace_self_in_block(d, concrete);
            }
        }
        AstStmt::Yield { expr, .. } => {
            replace_self_in_expr(expr, type_subs, concrete);
        }
        AstStmt::Break { .. } | AstStmt::Continue { .. } => {}
    }
}

fn replace_self_in_expr(expr: &mut AstExpr, type_subs: &HashMap<String, AstType>, concrete: &str) {
    match expr {
        AstExpr::StructLit {
            name,
            fields,
            spread,
            ..
        } => {
            if name == "Self" {
                *name = concrete.to_string();
            }
            for (_, val) in fields.iter_mut() {
                replace_self_in_expr(val, type_subs, concrete);
            }
            if let Some(ref mut s) = spread {
                replace_self_in_expr(s, type_subs, concrete);
            }
        }
        AstExpr::BinOp { lhs, rhs, .. } => {
            replace_self_in_expr(lhs, type_subs, concrete);
            replace_self_in_expr(rhs, type_subs, concrete);
        }
        AstExpr::Call {
            args, named_args, ..
        } => {
            for a in args.iter_mut() {
                replace_self_in_expr(a, type_subs, concrete);
            }
            for (_, v) in named_args.iter_mut() {
                replace_self_in_expr(v, type_subs, concrete);
            }
        }
        AstExpr::UnaryOp { expr: e, .. } => replace_self_in_expr(e, type_subs, concrete),
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            replace_self_in_expr(cond, type_subs, concrete);
            replace_self_in_block(then_block, concrete);
            if let Some(ref mut eb) = else_block {
                replace_self_in_block(eb, concrete);
            }
        }
        AstExpr::Block(b) => replace_self_in_block(b, concrete),
        AstExpr::Index { base, indices, .. } => {
            replace_self_in_expr(base, type_subs, concrete);
            for i in indices.iter_mut() {
                replace_self_in_expr(i, type_subs, concrete);
            }
        }
        AstExpr::Cast { expr: e, ty, .. } => {
            replace_self_in_expr(e, type_subs, concrete);
            *ty = substitute_ast_type(ty, type_subs, &HashMap::new());
        }
        AstExpr::FieldAccess { base, .. } => replace_self_in_expr(base, type_subs, concrete),
        AstExpr::When {
            scrutinee, arms, ..
        } => {
            replace_self_in_expr(scrutinee, type_subs, concrete);
            for arm in arms.iter_mut() {
                if let Some(ref mut g) = arm.guard {
                    replace_self_in_expr(g, type_subs, concrete);
                }
                replace_self_in_expr(&mut arm.body, type_subs, concrete);
            }
        }
        AstExpr::Tuple { elements, .. } => {
            for e in elements.iter_mut() {
                replace_self_in_expr(e, type_subs, concrete);
            }
        }
        AstExpr::TupleIndex { base, .. } => replace_self_in_expr(base, type_subs, concrete),
        AstExpr::ArrayLit { elems, .. } => {
            for e in elems.iter_mut() {
                replace_self_in_expr(e, type_subs, concrete);
            }
        }
        AstExpr::Lambda { params, body, .. } => {
            for p in params.iter_mut() {
                p.ty = substitute_ast_type(&p.ty, type_subs, &HashMap::new());
            }
            replace_self_in_expr(body, type_subs, concrete);
        }
        AstExpr::Await { expr: e, .. } => replace_self_in_expr(e, type_subs, concrete),
        AstExpr::Try { expr: e, .. } => replace_self_in_expr(e, type_subs, concrete),
        AstExpr::NullCoal {
            expr: e, default, ..
        } => {
            replace_self_in_expr(e, type_subs, concrete);
            replace_self_in_expr(default, type_subs, concrete);
        }
        AstExpr::MethodCall { base, args, .. } => {
            replace_self_in_expr(base, type_subs, concrete);
            for a in args.iter_mut() {
                replace_self_in_expr(a, type_subs, concrete);
            }
        }
        AstExpr::Mask { body, .. } => replace_self_in_block(body, concrete),
        AstExpr::Handle {
            expr: e,
            arms: _,
            return_ty,
            ..
        } => {
            replace_self_in_expr(e, type_subs, concrete);
            **return_ty = substitute_ast_type(return_ty, type_subs, &HashMap::new());
        }
        AstExpr::MapLiteral { entries, .. } => {
            for (k, v) in entries.iter_mut() {
                replace_self_in_expr(k, type_subs, concrete);
                replace_self_in_expr(v, type_subs, concrete);
            }
        }
        AstExpr::Ref { expr: e, .. } => replace_self_in_expr(e, type_subs, concrete),
        AstExpr::RefMut { expr: e, .. } => replace_self_in_expr(e, type_subs, concrete),
        AstExpr::Deref { expr: e, .. } => replace_self_in_expr(e, type_subs, concrete),
        AstExpr::TryCatch {
            body, catch_body, ..
        } => {
            replace_self_in_expr(body, type_subs, concrete);
            replace_self_in_expr(catch_body, type_subs, concrete);
        }
        AstExpr::Raise { args, .. } => {
            for a in args.iter_mut() {
                replace_self_in_expr(a, type_subs, concrete);
            }
        }
        AstExpr::Move { expr: e, .. } => replace_self_in_expr(e, type_subs, concrete),
        AstExpr::Unsafe { body, .. } => replace_self_in_expr(body, type_subs, concrete),
        AstExpr::Splat { expr: e, .. } => replace_self_in_expr(e, type_subs, concrete),
        AstExpr::MacroCall { args, .. } => {
            for a in args.iter_mut() {
                replace_self_in_expr(a, type_subs, concrete);
            }
        }
        AstExpr::Ident(_)
        | AstExpr::IntLit { .. }
        | AstExpr::FloatLit { .. }
        | AstExpr::BoolLit { .. }
        | AstExpr::StringLit { .. } => {}
    }
}

/// For each `impl TraitName for TypeName { ... }`, inject default method bodies from the trait
/// definition for any methods the impl does not provide. Runs after parsing but before lowering.
pub(crate) fn inject_default_impl_methods(ast: &mut AstModule) {
    let trait_map: HashMap<String, &AstTraitDef> = ast
        .traits
        .iter()
        .map(|t| (t.name.name.clone(), t))
        .collect();
    for impl_def in &mut ast.impls {
        if impl_def.trait_name.is_empty() {
            continue;
        }
        let Some(trait_def) = trait_map.get(&impl_def.trait_name) else {
            continue;
        };
        let concrete_ty_name = impl_def.type_name.clone();
        let type_subs = [(
            String::from("Self"),
            AstType::Named(concrete_ty_name.clone(), Span::new(0, 0)),
        )]
        .into_iter()
        .collect::<HashMap<_, _>>();
        for trait_method in &trait_def.methods {
            let Some(body) = &trait_method.body else {
                continue;
            };
            if impl_def
                .methods
                .iter()
                .any(|m| m.name.name == trait_method.name.name)
            {
                continue;
            }
            let mut params = trait_method.params.clone();
            for param in &mut params {
                if param.name.name == "self" {
                    if let AstType::Named(ref n, _) = param.ty {
                        if n == "self" || n == "Self" {
                            param.ty = AstType::Named(concrete_ty_name.clone(), param.ty.span());
                            continue;
                        }
                    }
                }
                param.ty = substitute_ast_type(&param.ty, &type_subs, &HashMap::new());
            }
            let return_ty =
                substitute_ast_type(&trait_method.return_ty, &type_subs, &HashMap::new());
            let mut method_body = body.clone();
            replace_self_in_block(&mut method_body, &concrete_ty_name);
            impl_def.methods.push(AstFunction {
                name: trait_method.name.clone(),
                is_pub: false,
                type_params: vec![],
                params,
                return_ty,
                effects: trait_method.effects.clone(),
                body: method_body,
                span: trait_method.span,
                is_async: false,
                is_const: false,
                attrs: vec![],
                doc_comment: None,
            });
        }
    }
}

/// Recursively substitute macro param names with argument expressions in a body AST.
fn substitute_macro_args(body: &AstExpr, params: &[String], args: &[AstExpr]) -> AstExpr {
    match body {
        AstExpr::Ident(ident) => {
            if let Some(pos) = params.iter().position(|p| p == &ident.name) {
                args[pos].clone()
            } else {
                body.clone()
            }
        }
        AstExpr::IntLit { .. }
        | AstExpr::FloatLit { .. }
        | AstExpr::BoolLit { .. }
        | AstExpr::StringLit { .. } => body.clone(),
        AstExpr::BinOp { op, lhs, rhs, span } => AstExpr::BinOp {
            op: *op,
            lhs: Box::new(substitute_macro_args(lhs, params, args)),
            rhs: Box::new(substitute_macro_args(rhs, params, args)),
            span: *span,
        },
        AstExpr::UnaryOp { op, expr, span } => AstExpr::UnaryOp {
            op: *op,
            expr: Box::new(substitute_macro_args(expr, params, args)),
            span: *span,
        },
        AstExpr::Call {
            callee,
            args: call_args,
            named_args,
            span,
        } => AstExpr::Call {
            callee: callee.clone(),
            args: call_args
                .iter()
                .map(|a| substitute_macro_args(a, params, args))
                .collect(),
            named_args: named_args
                .iter()
                .map(|(n, a)| (n.clone(), substitute_macro_args(a, params, args)))
                .collect(),
            span: *span,
        },
        AstExpr::If {
            cond,
            then_block,
            else_block,
            span,
        } => AstExpr::If {
            cond: Box::new(substitute_macro_args(cond, params, args)),
            then_block: substitute_macro_block(then_block, params, args),
            else_block: else_block
                .as_ref()
                .map(|b| substitute_macro_block(b, params, args)),
            span: *span,
        },
        AstExpr::Block(block) => AstExpr::Block(substitute_macro_block(block, params, args)),
        AstExpr::Index {
            base,
            indices,
            span,
        } => AstExpr::Index {
            base: Box::new(substitute_macro_args(base, params, args)),
            indices: indices
                .iter()
                .map(|i| substitute_macro_args(i, params, args))
                .collect(),
            span: *span,
        },
        AstExpr::Cast { expr, ty, span } => AstExpr::Cast {
            expr: Box::new(substitute_macro_args(expr, params, args)),
            ty: ty.clone(),
            span: *span,
        },
        AstExpr::StructLit {
            name,
            fields,
            field_spans,
            spread,
            span,
        } => AstExpr::StructLit {
            name: name.clone(),
            fields: fields
                .iter()
                .map(|(n, f)| (n.clone(), substitute_macro_args(f, params, args)))
                .collect(),
            field_spans: field_spans.clone(),
            spread: spread
                .as_ref()
                .map(|s| Box::new(substitute_macro_args(s, params, args))),
            span: *span,
        },
        AstExpr::FieldAccess { base, field, span } => AstExpr::FieldAccess {
            base: Box::new(substitute_macro_args(base, params, args)),
            field: field.clone(),
            span: *span,
        },
        AstExpr::When {
            scrutinee,
            arms,
            span,
        } => AstExpr::When {
            scrutinee: Box::new(substitute_macro_args(scrutinee, params, args)),
            arms: arms
                .iter()
                .map(|arm| crate::parser::ast::AstWhenArm {
                    pattern: arm.pattern.clone(),
                    guard: arm
                        .guard
                        .as_ref()
                        .map(|g| Box::new(substitute_macro_args(g, params, args))),
                    body: Box::new(substitute_macro_args(&arm.body, params, args)),
                    span: arm.span,
                    enum_name: arm.enum_name.clone(),
                    variant_name: arm.variant_name.clone(),
                })
                .collect(),
            span: *span,
        },
        AstExpr::Tuple { elements, span } => AstExpr::Tuple {
            elements: elements
                .iter()
                .map(|e| substitute_macro_args(e, params, args))
                .collect(),
            span: *span,
        },
        AstExpr::TupleIndex { base, index, span } => AstExpr::TupleIndex {
            base: Box::new(substitute_macro_args(base, params, args)),
            index: *index,
            span: *span,
        },
        AstExpr::ArrayLit { elems, span } => AstExpr::ArrayLit {
            elems: elems
                .iter()
                .map(|e| substitute_macro_args(e, params, args))
                .collect(),
            span: *span,
        },
        AstExpr::Lambda {
            params: lam_params,
            body: lam_body,
            span,
        } => AstExpr::Lambda {
            params: lam_params.clone(),
            body: Box::new(substitute_macro_args(lam_body, params, args)),
            span: *span,
        },
        AstExpr::Await { expr, span } => AstExpr::Await {
            expr: Box::new(substitute_macro_args(expr, params, args)),
            span: *span,
        },
        AstExpr::Try { expr, span } => AstExpr::Try {
            expr: Box::new(substitute_macro_args(expr, params, args)),
            span: *span,
        },
        AstExpr::NullCoal {
            expr,
            default,
            span,
        } => AstExpr::NullCoal {
            expr: Box::new(substitute_macro_args(expr, params, args)),
            default: Box::new(substitute_macro_args(default, params, args)),
            span: *span,
        },
        AstExpr::MethodCall {
            base,
            method,
            args: mc_args,
            span,
        } => AstExpr::MethodCall {
            base: Box::new(substitute_macro_args(base, params, args)),
            method: method.clone(),
            args: mc_args
                .iter()
                .map(|a| substitute_macro_args(a, params, args))
                .collect(),
            span: *span,
        },
        AstExpr::Mask {
            effects,
            body: mask_body,
            span,
        } => AstExpr::Mask {
            effects: effects.clone(),
            body: substitute_macro_block(mask_body, params, args),
            span: *span,
        },
        AstExpr::Handle {
            expr,
            arms,
            return_ty,
            span,
        } => AstExpr::Handle {
            expr: Box::new(substitute_macro_args(expr, params, args)),
            arms: arms.clone(),
            return_ty: return_ty.clone(),
            span: *span,
        },
        AstExpr::MapLiteral { entries, span } => AstExpr::MapLiteral {
            entries: entries
                .iter()
                .map(|(k, v)| {
                    (
                        substitute_macro_args(k, params, args),
                        substitute_macro_args(v, params, args),
                    )
                })
                .collect(),
            span: *span,
        },
        AstExpr::Ref { expr: e, span } => AstExpr::Ref {
            expr: Box::new(substitute_macro_args(e, params, args)),
            span: *span,
        },
        AstExpr::RefMut { expr: e, span } => AstExpr::RefMut {
            expr: Box::new(substitute_macro_args(e, params, args)),
            span: *span,
        },
        AstExpr::Deref { expr: e, span } => AstExpr::Deref {
            expr: Box::new(substitute_macro_args(e, params, args)),
            span: *span,
        },
        AstExpr::TryCatch {
            body,
            catch_param,
            catch_body,
            span,
        } => AstExpr::TryCatch {
            body: Box::new(substitute_macro_args(body, params, args)),
            catch_param: catch_param.clone(),
            catch_body: Box::new(substitute_macro_args(catch_body, params, args)),
            span: *span,
        },
        AstExpr::Raise {
            effect_name,
            args: r_args,
            span,
        } => AstExpr::Raise {
            effect_name: effect_name.clone(),
            args: r_args
                .iter()
                .map(|a| substitute_macro_args(a, params, args))
                .collect(),
            span: *span,
        },
        AstExpr::Move { expr, span } => AstExpr::Move {
            expr: Box::new(substitute_macro_args(expr, params, args)),
            span: *span,
        },
        AstExpr::Unsafe { body, span } => AstExpr::Unsafe {
            body: Box::new(substitute_macro_args(body, params, args)),
            span: *span,
        },
        AstExpr::Splat { expr, span } => AstExpr::Splat {
            expr: Box::new(substitute_macro_args(expr, params, args)),
            span: *span,
        },
        AstExpr::MacroCall { .. } => body.clone(), // Already expanded, shouldn't reach here
    }
}

fn substitute_macro_block(
    block: &crate::parser::ast::AstBlock,
    params: &[String],
    args: &[AstExpr],
) -> crate::parser::ast::AstBlock {
    crate::parser::ast::AstBlock {
        stmts: block
            .stmts
            .iter()
            .map(|s| substitute_macro_stmt(s, params, args))
            .collect(),
        tail: block
            .tail
            .as_ref()
            .map(|t| Box::new(substitute_macro_args(t, params, args))),
        span: block.span,
    }
}

fn substitute_macro_stmt(
    stmt: &crate::parser::ast::AstStmt,
    params: &[String],
    args: &[AstExpr],
) -> crate::parser::ast::AstStmt {
    match stmt {
        crate::parser::ast::AstStmt::Let {
            name,
            ty,
            init,
            is_var,
            span,
        } => crate::parser::ast::AstStmt::Let {
            name: name.clone(),
            ty: ty.clone(),
            init: Box::new(substitute_macro_args(init, params, args)),
            is_var: *is_var,
            span: *span,
        },
        crate::parser::ast::AstStmt::Expr(expr) => {
            crate::parser::ast::AstStmt::Expr(Box::new(substitute_macro_args(expr, params, args)))
        }
        crate::parser::ast::AstStmt::While {
            label,
            cond,
            body,
            span,
        } => crate::parser::ast::AstStmt::While {
            label: label.clone(),
            cond: Box::new(substitute_macro_args(cond, params, args)),
            body: substitute_macro_block(body, params, args),
            span: *span,
        },
        crate::parser::ast::AstStmt::Loop { label, body, span } => {
            crate::parser::ast::AstStmt::Loop {
                label: label.clone(),
                body: substitute_macro_block(body, params, args),
                span: *span,
            }
        }
        crate::parser::ast::AstStmt::Break { label, span } => crate::parser::ast::AstStmt::Break {
            label: label.clone(),
            span: *span,
        },
        crate::parser::ast::AstStmt::Continue { label, span } => {
            crate::parser::ast::AstStmt::Continue {
                label: label.clone(),
                span: *span,
            }
        }
        crate::parser::ast::AstStmt::ForRange {
            label,
            var,
            start,
            end,
            inclusive,
            step,
            body,
            span,
        } => crate::parser::ast::AstStmt::ForRange {
            label: label.clone(),
            var: var.clone(),
            start: Box::new(substitute_macro_args(start, params, args)),
            end: Box::new(substitute_macro_args(end, params, args)),
            inclusive: *inclusive,
            step: step
                .as_ref()
                .map(|s| Box::new(substitute_macro_args(s, params, args))),
            body: substitute_macro_block(body, params, args),
            span: *span,
        },
        crate::parser::ast::AstStmt::Assign {
            target,
            op,
            value,
            span,
        } => crate::parser::ast::AstStmt::Assign {
            target: Box::new(substitute_macro_args(target, params, args)),
            op: *op,
            value: Box::new(substitute_macro_args(value, params, args)),
            span: *span,
        },
        crate::parser::ast::AstStmt::LetTuple {
            names,
            init,
            is_var,
            span,
        } => crate::parser::ast::AstStmt::LetTuple {
            names: names.clone(),
            init: Box::new(substitute_macro_args(init, params, args)),
            is_var: *is_var,
            span: *span,
        },
        crate::parser::ast::AstStmt::Return { value, span } => {
            crate::parser::ast::AstStmt::Return {
                value: value
                    .as_ref()
                    .map(|v| Box::new(substitute_macro_args(v, params, args))),
                span: *span,
            }
        }
        crate::parser::ast::AstStmt::Spawn {
            body: spawn_body,
            span,
            group,
        } => crate::parser::ast::AstStmt::Spawn {
            body: spawn_body
                .iter()
                .map(|s| substitute_macro_stmt(s, params, args))
                .collect(),
            span: *span,
            group: group
                .as_ref()
                .map(|g| Box::new(substitute_macro_args(g, params, args))),
        },
        crate::parser::ast::AstStmt::ParFor {
            label,
            var,
            start,
            end,
            inclusive,
            body,
            span,
        } => crate::parser::ast::AstStmt::ParFor {
            label: label.clone(),
            var: var.clone(),
            start: Box::new(substitute_macro_args(start, params, args)),
            end: Box::new(substitute_macro_args(end, params, args)),
            inclusive: *inclusive,
            body: substitute_macro_block(body, params, args),
            span: *span,
        },
        crate::parser::ast::AstStmt::ForEach {
            label,
            var,
            iter,
            body,
            span,
        } => crate::parser::ast::AstStmt::ForEach {
            label: label.clone(),
            var: var.clone(),
            iter: Box::new(substitute_macro_args(iter, params, args)),
            body: substitute_macro_block(body, params, args),
            span: *span,
        },
        crate::parser::ast::AstStmt::MaskStmt {
            effects,
            body,
            span,
        } => crate::parser::ast::AstStmt::MaskStmt {
            effects: effects.clone(),
            body: substitute_macro_block(body, params, args),
            span: *span,
        },
        crate::parser::ast::AstStmt::HandleStmt {
            expr,
            arms,
            return_ty,
            span,
        } => crate::parser::ast::AstStmt::HandleStmt {
            expr: Box::new(substitute_macro_args(expr, params, args)),
            arms: arms.clone(),
            return_ty: return_ty.clone(),
            span: *span,
        },
        crate::parser::ast::AstStmt::Defer { expr, span } => crate::parser::ast::AstStmt::Defer {
            expr: Box::new(substitute_macro_args(expr, params, args)),
            span: *span,
        },
        crate::parser::ast::AstStmt::Select {
            arms,
            default,
            span,
        } => crate::parser::ast::AstStmt::Select {
            arms: arms.clone(),
            default: default.clone(),
            span: *span,
        },
        crate::parser::ast::AstStmt::Yield { expr, span } => crate::parser::ast::AstStmt::Yield {
            expr: Box::new(substitute_macro_args(expr, params, args)),
            span: *span,
        },
    }
}

/// Walk an expression tree and expand all `MacroCall` nodes using macro definitions.
/// Keeps iterating until no more MacroCall nodes remain (handles nested macros).
pub(crate) fn expand_macros(ast: &mut AstModule) {
    // Build macro lookup table
    let macros: std::collections::HashMap<String, &AstMacroDef> = ast
        .macros
        .iter()
        .map(|m| (m.name.name.clone(), m))
        .collect();

    if macros.is_empty() {
        return;
    }

    fn expand_in_expr(
        expr: &mut AstExpr,
        macros: &std::collections::HashMap<String, &AstMacroDef>,
    ) {
        match expr {
            AstExpr::MacroCall { name, args, .. } => {
                if let Some(macro_def) = macros.get(&name.name) {
                    if macro_def.params.len() != args.len() {
                        return; // Skip malformed macro calls
                    }
                    let expanded = substitute_macro_args(&macro_def.body, &macro_def.params, args);
                    *expr = expanded;
                    // Recurse to handle nested macros in the expanded result
                    expand_in_expr(expr, macros);
                }
            }
            _ => {
                // Recurse into all sub-expressions
                expand_in_expr_recurse(expr, macros);
            }
        }
    }

    fn expand_in_expr_recurse(
        expr: &mut AstExpr,
        macros: &std::collections::HashMap<String, &AstMacroDef>,
    ) {
        match expr {
            AstExpr::Ident(_)
            | AstExpr::IntLit { .. }
            | AstExpr::FloatLit { .. }
            | AstExpr::BoolLit { .. }
            | AstExpr::StringLit { .. } => {}
            AstExpr::BinOp { lhs, rhs, .. } => {
                expand_in_expr(lhs, macros);
                expand_in_expr(rhs, macros);
            }
            AstExpr::UnaryOp { expr: e, .. }
            | AstExpr::Cast { expr: e, .. }
            | AstExpr::Await { expr: e, .. }
            | AstExpr::Try { expr: e, .. } => {
                expand_in_expr(e, macros);
            }
            AstExpr::Call { args, .. } | AstExpr::Tuple { elements: args, .. } => {
                for a in args.iter_mut() {
                    expand_in_expr(a, macros);
                }
            }
            AstExpr::MethodCall { base, args, .. } => {
                expand_in_expr(base, macros);
                for a in args.iter_mut() {
                    expand_in_expr(a, macros);
                }
            }
            AstExpr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                expand_in_expr(cond, macros);
                for s in then_block.stmts.iter_mut() {
                    expand_in_stmt(s, macros);
                }
                if let Some(t) = &mut then_block.tail {
                    expand_in_expr(t, macros);
                }
                if let Some(eb) = else_block {
                    for s in eb.stmts.iter_mut() {
                        expand_in_stmt(s, macros);
                    }
                    if let Some(t) = &mut eb.tail {
                        expand_in_expr(t, macros);
                    }
                }
            }
            AstExpr::Block(block) => {
                for s in block.stmts.iter_mut() {
                    expand_in_stmt(s, macros);
                }
                if let Some(t) = &mut block.tail {
                    expand_in_expr(t, macros);
                }
            }
            AstExpr::Index { base, indices, .. } => {
                expand_in_expr(base, macros);
                for i in indices.iter_mut() {
                    expand_in_expr(i, macros);
                }
            }
            AstExpr::StructLit { fields, spread, .. } => {
                for (_, f) in fields.iter_mut() {
                    expand_in_expr(f, macros);
                }
                if let Some(s) = spread {
                    expand_in_expr(s, macros);
                }
            }
            AstExpr::FieldAccess { base, .. } | AstExpr::TupleIndex { base, .. } => {
                expand_in_expr(base, macros);
            }
            AstExpr::When {
                scrutinee, arms, ..
            } => {
                expand_in_expr(scrutinee, macros);
                for arm in arms.iter_mut() {
                    if let Some(g) = &mut arm.guard {
                        expand_in_expr(g, macros);
                    }
                    expand_in_expr(&mut arm.body, macros);
                }
            }
            AstExpr::ArrayLit { elems, .. } => {
                for e in elems.iter_mut() {
                    expand_in_expr(e, macros);
                }
            }
            AstExpr::Lambda { body, .. } => {
                expand_in_expr(body, macros);
            }
            AstExpr::NullCoal {
                expr: e, default, ..
            } => {
                expand_in_expr(e, macros);
                expand_in_expr(default, macros);
            }
            AstExpr::MapLiteral { entries, .. } => {
                for (k, v) in entries.iter_mut() {
                    expand_in_expr(k, macros);
                    expand_in_expr(v, macros);
                }
            }
            AstExpr::Mask { body, .. } => {
                for s in body.stmts.iter_mut() {
                    expand_in_stmt(s, macros);
                }
                if let Some(t) = &mut body.tail {
                    expand_in_expr(t, macros);
                }
            }
            AstExpr::Handle { expr: e, arms, .. } => {
                expand_in_expr(e, macros);
                for arm in arms.iter_mut() {
                    expand_in_expr(&mut arm.body, macros);
                }
            }
            AstExpr::Ref { expr: e, .. }
            | AstExpr::RefMut { expr: e, .. }
            | AstExpr::Deref { expr: e, .. }
            | AstExpr::Move { expr: e, .. } => {
                expand_in_expr(e, macros);
            }
            AstExpr::Unsafe { body, .. } => {
                expand_in_expr(body, macros);
            }
            AstExpr::Splat { expr: e, .. } => {
                expand_in_expr(e, macros);
            }
            AstExpr::TryCatch {
                body, catch_body, ..
            } => {
                expand_in_expr(body, macros);
                expand_in_expr(catch_body, macros);
            }
            AstExpr::Raise { args, .. } => {
                for a in args.iter_mut() {
                    expand_in_expr(a, macros);
                }
            }
            AstExpr::MacroCall { .. } => {
                expand_in_expr(expr, macros);
            }
        }
    }

    fn expand_in_stmt(
        stmt: &mut crate::parser::ast::AstStmt,
        macros: &std::collections::HashMap<String, &AstMacroDef>,
    ) {
        match stmt {
            crate::parser::ast::AstStmt::Let { init, .. } => expand_in_expr(init, macros),
            crate::parser::ast::AstStmt::Expr(expr) => expand_in_expr(expr, macros),
            crate::parser::ast::AstStmt::While { cond, body, .. } => {
                expand_in_expr(cond, macros);
                for s in body.stmts.iter_mut() {
                    expand_in_stmt(s, macros);
                }
                if let Some(t) = &mut body.tail {
                    expand_in_expr(t, macros);
                }
            }
            crate::parser::ast::AstStmt::Loop { body, .. } => {
                for s in body.stmts.iter_mut() {
                    expand_in_stmt(s, macros);
                }
                if let Some(t) = &mut body.tail {
                    expand_in_expr(t, macros);
                }
            }
            crate::parser::ast::AstStmt::Break { .. }
            | crate::parser::ast::AstStmt::Continue { .. } => {}
            crate::parser::ast::AstStmt::ForRange {
                start, end, body, ..
            } => {
                expand_in_expr(start, macros);
                expand_in_expr(end, macros);
                for s in body.stmts.iter_mut() {
                    expand_in_stmt(s, macros);
                }
                if let Some(t) = &mut body.tail {
                    expand_in_expr(t, macros);
                }
            }
            crate::parser::ast::AstStmt::Assign { target, value, .. } => {
                expand_in_expr(target, macros);
                expand_in_expr(value, macros);
            }
            crate::parser::ast::AstStmt::LetTuple { init, .. } => expand_in_expr(init, macros),
            crate::parser::ast::AstStmt::Return { value, .. } => {
                if let Some(v) = value {
                    expand_in_expr(v, macros);
                }
            }
            crate::parser::ast::AstStmt::Spawn { body, group, .. } => {
                if let Some(g) = group {
                    expand_in_expr(g, macros);
                }
                for s in body.iter_mut() {
                    expand_in_stmt(s, macros);
                }
            }
            crate::parser::ast::AstStmt::ParFor {
                start, end, body, ..
            } => {
                expand_in_expr(start, macros);
                expand_in_expr(end, macros);
                for s in body.stmts.iter_mut() {
                    expand_in_stmt(s, macros);
                }
                if let Some(t) = &mut body.tail {
                    expand_in_expr(t, macros);
                }
            }
            crate::parser::ast::AstStmt::ForEach { iter, body, .. } => {
                expand_in_expr(iter, macros);
                for s in body.stmts.iter_mut() {
                    expand_in_stmt(s, macros);
                }
                if let Some(t) = &mut body.tail {
                    expand_in_expr(t, macros);
                }
            }
            crate::parser::ast::AstStmt::MaskStmt { body, .. } => {
                for s in body.stmts.iter_mut() {
                    expand_in_stmt(s, macros);
                }
                if let Some(t) = &mut body.tail {
                    expand_in_expr(t, macros);
                }
            }
            crate::parser::ast::AstStmt::HandleStmt { expr, arms, .. } => {
                expand_in_expr(expr, macros);
                for arm in arms.iter_mut() {
                    expand_in_expr(&mut arm.body, macros);
                }
            }
            crate::parser::ast::AstStmt::Defer { expr, .. } => expand_in_expr(expr, macros),
            crate::parser::ast::AstStmt::Select { arms, default, .. } => {
                for arm in arms.iter_mut() {
                    expand_in_expr(&mut arm.channel, macros);
                }
                if let Some(d) = default {
                    for s in d.stmts.iter_mut() {
                        expand_in_stmt(s, macros);
                    }
                    if let Some(t) = &mut d.tail {
                        expand_in_expr(t, macros);
                    }
                }
            }
            crate::parser::ast::AstStmt::Yield { expr, .. } => expand_in_expr(expr, macros),
        }
    }

    // Expand in all function bodies
    for func in &mut ast.functions {
        for s in func.body.stmts.iter_mut() {
            expand_in_stmt(s, &macros);
        }
        if let Some(t) = &mut func.body.tail {
            expand_in_expr(t, &macros);
        }
    }
    // Expand in all const initializers
    for c in &mut ast.consts {
        expand_in_expr(&mut c.value, &macros);
    }
    // Expand in all trait method bodies (default implementations)
    for trait_def in &mut ast.traits {
        for method in &mut trait_def.methods {
            if let Some(body) = &mut method.body {
                for s in body.stmts.iter_mut() {
                    expand_in_stmt(s, &macros);
                }
                if let Some(t) = &mut body.tail {
                    expand_in_expr(t, &macros);
                }
            }
        }
    }
    // Expand in all impl method bodies
    for impl_def in &mut ast.impls {
        for method in &mut impl_def.methods {
            for s in method.body.stmts.iter_mut() {
                expand_in_stmt(s, &macros);
            }
            if let Some(t) = &mut method.body.tail {
                expand_in_expr(t, &macros);
            }
        }
    }
}

/// Flatten all inline `mod name { ... }` blocks into the parent module.
/// Each item is prefixed with `modname__` (same convention as `bring`).
/// Also resolves `bring` declarations that reference inline modules.
pub(crate) fn flatten_inline_modules(ast: &mut AstModule) {
    // First, resolve `bring` declarations that reference inline modules.
    resolve_inline_module_bring(ast);

    // Process recursively: flatten nested modules.
    let mut all_modules = std::mem::take(&mut ast.modules);
    for m in &mut all_modules {
        flatten_inline_modules_for(m);
        // After flattening, merge the module's items into the parent.
        ast.enums.extend(std::mem::take(&mut m.enums));
        ast.structs.extend(std::mem::take(&mut m.structs));
        ast.functions.extend(std::mem::take(&mut m.functions));
        ast.models.extend(std::mem::take(&mut m.models));
        ast.consts.extend(std::mem::take(&mut m.consts));
        ast.type_aliases.extend(std::mem::take(&mut m.type_aliases));
        ast.traits.extend(std::mem::take(&mut m.traits));
        ast.impls.extend(std::mem::take(&mut m.impls));
        ast.effects.extend(std::mem::take(&mut m.effects));
        ast.extern_fns.extend(std::mem::take(&mut m.extern_fns));
    }
}

fn flatten_inline_modules_for(m: &mut AstModuleDef) {
    // Recurse into nested modules first.
    let mut sub_modules = std::mem::take(&mut m.modules);
    for sub in &mut sub_modules {
        flatten_inline_modules_for(sub);
        // Merge sub-module items into this module.
        m.enums.extend(std::mem::take(&mut sub.enums));
        m.structs.extend(std::mem::take(&mut sub.structs));
        m.functions.extend(std::mem::take(&mut sub.functions));
        m.models.extend(std::mem::take(&mut sub.models));
        m.consts.extend(std::mem::take(&mut sub.consts));
        m.type_aliases.extend(std::mem::take(&mut sub.type_aliases));
        m.traits.extend(std::mem::take(&mut sub.traits));
        m.impls.extend(std::mem::take(&mut sub.impls));
        m.effects.extend(std::mem::take(&mut sub.effects));
        m.extern_fns.extend(std::mem::take(&mut sub.extern_fns));
        m.macros.extend(std::mem::take(&mut sub.macros));
    }

    // Build a module-level AstModule from this module's items for mangling.
    let mut mod_ast = AstModule {
        private_items: std::collections::HashSet::new(),
        enums: std::mem::take(&mut m.enums),
        structs: std::mem::take(&mut m.structs),
        functions: std::mem::take(&mut m.functions),
        models: std::mem::take(&mut m.models),
        consts: std::mem::take(&mut m.consts),
        type_aliases: std::mem::take(&mut m.type_aliases),
        traits: std::mem::take(&mut m.traits),
        impls: std::mem::take(&mut m.impls),
        effects: std::mem::take(&mut m.effects),
        brings: vec![],
        extern_fns: std::mem::take(&mut m.extern_fns),
        modules: vec![],
        macros: std::mem::take(&mut m.macros),
        state_vars: vec![],
    };

    mangle_module_symbols(&mut mod_ast, &m.name.name);

    // Move mangled items back.
    m.enums = mod_ast.enums;
    m.structs = mod_ast.structs;
    m.functions = mod_ast.functions;
    m.models = mod_ast.models;
    m.consts = mod_ast.consts;
    m.type_aliases = mod_ast.type_aliases;
    m.traits = mod_ast.traits;
    m.impls = mod_ast.impls;
    m.effects = mod_ast.effects;
    m.extern_fns = mod_ast.extern_fns;
    m.modules = sub_modules;
}

/// Resolve `bring` declarations that reference inline modules.
/// `bring math` → if `mod math { ... }` exists in the AST, flatten and merge it.
fn resolve_inline_module_bring(ast: &mut AstModule) {
    let brings = std::mem::take(&mut ast.brings);
    for bring in brings {
        let mod_name = match &bring.path {
            crate::parser::ast::BringPath::File(p) => {
                p.trim_end_matches(".iris").replace(['.', '-'], "_")
            }
            crate::parser::ast::BringPath::Stdlib(name) => name.replace(['.', '-'], "_"),
        };
        // Check if this bring references an inline module.
        if let Some(pos) = ast.modules.iter().position(|m| m.name.name == mod_name) {
            let mut mod_def = ast.modules.remove(pos);
            // Flatten nested modules inside this one.
            flatten_inline_modules_for(&mut mod_def);
            // Mangle symbols with module name.
            let mut mod_ast = AstModule {
                private_items: std::collections::HashSet::new(),
                enums: std::mem::take(&mut mod_def.enums),
                structs: std::mem::take(&mut mod_def.structs),
                functions: std::mem::take(&mut mod_def.functions),
                models: std::mem::take(&mut mod_def.models),
                consts: std::mem::take(&mut mod_def.consts),
                type_aliases: std::mem::take(&mut mod_def.type_aliases),
                traits: std::mem::take(&mut mod_def.traits),
                impls: std::mem::take(&mut mod_def.impls),
                effects: std::mem::take(&mut mod_def.effects),
                brings: vec![],
                extern_fns: std::mem::take(&mut mod_def.extern_fns),
                modules: vec![],
                macros: std::mem::take(&mut mod_def.macros),
                state_vars: vec![],
            };
            mangle_module_symbols(&mut mod_ast, &mod_name);
            // Merge into parent.
            ast.functions.extend(mod_ast.functions);
            ast.structs.extend(mod_ast.structs);
            ast.enums.extend(mod_ast.enums);
            ast.consts.extend(mod_ast.consts);
            ast.type_aliases.extend(mod_ast.type_aliases);
            ast.traits.extend(mod_ast.traits);
            ast.impls.extend(mod_ast.impls);
            ast.models.extend(mod_ast.models);
            ast.extern_fns.extend(mod_ast.extern_fns);
            ast.macros.extend(mod_ast.macros);
        } else {
            // Not an inline module — put the bring back (handled elsewhere).
            ast.brings.push(bring);
        }
    }
}

impl Default for FileCompiler {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FileCompiler {
    fn drop(&mut self) {
        self.cache.flush();
    }
}
