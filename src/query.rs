//! Demand-driven incremental compilation and query engine.
//!
//! Provides a Salsa-style memoized query database (`QueryEngine`) with:
//! - Content-addressed input hashing (`sha256_hex`) so identical inputs never
//!   bump the global revision.
//! - Fine-grained dependency tracking (`MemoEntry` with `verified_at`,
//!   `changed_at`, and upstream `deps`).
//! - Structural **early cutoff**: whitespace/comment-only edits produce the
//!   same AST fingerprint, allowing downstream `IrModule` and `EmitKind`
//!   queries to reuse cached results without re-lowering or re-running passes.
//! - Per-function AST fingerprinting (`changed_functions`) to identify which
//!   functions in a module changed across revisions.
//! - Optional persistent disk backing via [`crate::cache::BuildCache`].

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::cache::{sha256_hex, BuildCache};
use crate::error::Error;
use crate::ir::module::IrModule;
use crate::parser::ast::AstModule;
use crate::EmitKind;

/// Monotonic revision counter for incremental invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Revision(pub u64);

/// Interned identifier for a source input inside [`QueryEngine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(pub u32);

/// Key identifying a memoized query node in the dependency graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QueryKey {
    SourceText(SourceId),
    Parse(SourceId),
    CompileIr(SourceId),
    Emit(SourceId, EmitKind),
}

/// Statistics tracked by [`QueryEngine`] for observability and testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueryStats {
    pub revision: u64,
    pub hits: u64,
    pub misses: u64,
    pub early_cutoffs: u64,
    pub disk_hits: u64,
}

#[derive(Debug, Clone)]
struct MemoEntry<T> {
    value: T,
    fingerprint: u64,
    verified_at: Revision,
    changed_at: Revision,
    deps: Vec<QueryKey>,
}

/// Demand-driven incremental query engine for IRIS compilation.
pub struct QueryEngine {
    revision: Revision,
    next_source_id: u32,
    source_ids: HashMap<String, SourceId>,
    sources: HashMap<SourceId, MemoEntry<String>>,
    source_sha256: HashMap<SourceId, String>,
    parsed: HashMap<SourceId, MemoEntry<AstModule>>,
    fn_fingerprints: HashMap<SourceId, HashMap<String, u64>>,
    last_changed_fns: HashMap<SourceId, Vec<String>>,
    compiled_ir: HashMap<SourceId, MemoEntry<IrModule>>,
    emitted: HashMap<(SourceId, EmitKind), MemoEntry<String>>,
    disk_cache: Option<BuildCache>,
    stats: QueryStats,
}

impl Default for QueryEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryEngine {
    /// Creates a new in-memory incremental [`QueryEngine`].
    pub fn new() -> Self {
        Self {
            revision: Revision(1),
            next_source_id: 0,
            source_ids: HashMap::new(),
            sources: HashMap::new(),
            source_sha256: HashMap::new(),
            parsed: HashMap::new(),
            fn_fingerprints: HashMap::new(),
            last_changed_fns: HashMap::new(),
            compiled_ir: HashMap::new(),
            emitted: HashMap::new(),
            disk_cache: None,
            stats: QueryStats {
                revision: 1,
                ..Default::default()
            },
        }
    }

    /// Creates a [`QueryEngine`] backed by a persistent [`BuildCache`].
    pub fn with_disk_cache(cache: BuildCache) -> Self {
        let mut engine = Self::new();
        engine.disk_cache = Some(cache);
        engine
    }

    /// Returns the current [`Revision`].
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// Returns cumulative cache hit, miss, early-cutoff, and disk-hit stats.
    pub fn stats(&self) -> QueryStats {
        let mut s = self.stats;
        s.revision = self.revision.0;
        s
    }

    /// Registers or updates the source text for `name`.
    ///
    /// If the content hash matches the currently stored text, the revision is
    /// not incremented and all downstream queries remain valid.
    pub fn set_source(&mut self, name: &str, text: impl Into<String>) -> SourceId {
        let text = text.into();
        let sha = sha256_hex(text.as_bytes());
        let fp = fnv1a_hash(text.as_bytes());

        if let Some(&id) = self.source_ids.get(name) {
            if self.source_sha256.get(&id).is_some_and(|prev| prev == &sha) {
                return id;
            }
            self.revision = Revision(self.revision.0 + 1);
            self.source_sha256.insert(id, sha);
            self.sources.insert(
                id,
                MemoEntry {
                    value: text,
                    fingerprint: fp,
                    verified_at: self.revision,
                    changed_at: self.revision,
                    deps: Vec::new(),
                },
            );
            return id;
        }

        let id = SourceId(self.next_source_id);
        self.next_source_id += 1;
        self.source_ids.insert(name.to_owned(), id);
        self.source_sha256.insert(id, sha);
        self.sources.insert(
            id,
            MemoEntry {
                value: text,
                fingerprint: fp,
                verified_at: self.revision,
                changed_at: self.revision,
                deps: Vec::new(),
            },
        );
        id
    }

    /// Returns the names of functions whose AST structure changed on the most
    /// recent parse of `id`.
    pub fn changed_functions(&self, id: SourceId) -> &[String] {
        self.last_changed_fns
            .get(&id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Parses `id` incrementally, applying structural early cutoff when the AST
    /// is unchanged (e.g. whitespace or comment edits).
    pub fn parse(&mut self, id: SourceId) -> Result<&AstModule, Error> {
        if self
            .parsed
            .get(&id)
            .is_some_and(|entry| self.is_entry_fresh(&entry.deps, entry.verified_at))
        {
            self.stats.hits += 1;
            if let Some(entry) = self.parsed.get_mut(&id) {
                entry.verified_at = self.revision;
            }
            return Ok(&self.parsed[&id].value);
        }

        self.stats.misses += 1;
        let src = self
            .sources
            .get(&id)
            .map(|e| e.value.clone())
            .ok_or_else(|| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("unknown QueryEngine SourceId({})", id.0),
                ))
            })?;

        let ast = crate::parse_recovering(&src)?;
        let new_fp = ast_structural_fingerprint(&ast);
        let new_fn_fps = per_function_fingerprints(&ast);

        let changed_fns = match self.fn_fingerprints.get(&id) {
            Some(prev) => {
                let mut diff = Vec::new();
                for (name, &fp) in &new_fn_fps {
                    if prev.get(name) != Some(&fp) {
                        diff.push(name.clone());
                    }
                }
                for name in prev.keys() {
                    if !new_fn_fps.contains_key(name) {
                        diff.push(name.clone());
                    }
                }
                diff.sort();
                diff
            }
            None => {
                let mut all: Vec<String> = new_fn_fps.keys().cloned().collect();
                all.sort();
                all
            }
        };
        self.last_changed_fns.insert(id, changed_fns);
        self.fn_fingerprints.insert(id, new_fn_fps);

        let changed_at = match self.parsed.get(&id) {
            Some(prev) if prev.fingerprint == new_fp => {
                self.stats.early_cutoffs += 1;
                prev.changed_at
            }
            _ => self.revision,
        };

        self.parsed.insert(
            id,
            MemoEntry {
                value: ast,
                fingerprint: new_fp,
                verified_at: self.revision,
                changed_at,
                deps: vec![QueryKey::SourceText(id)],
            },
        );
        Ok(&self.parsed[&id].value)
    }

    /// Compiles `id` to an optimized [`IrModule`] incrementally, checking both
    /// in-memory memoization (with early cutoff) and optional disk cache.
    pub fn compile_ir(&mut self, id: SourceId, module_name: &str) -> Result<&IrModule, Error> {
        let _ = self.parse(id)?;
        if self
            .compiled_ir
            .get(&id)
            .is_some_and(|entry| self.is_entry_fresh(&entry.deps, entry.verified_at))
        {
            self.stats.hits += 1;
            if let Some(entry) = self.compiled_ir.get_mut(&id) {
                entry.verified_at = self.revision;
            }
            return Ok(&self.compiled_ir[&id].value);
        }

        let sha = self.source_sha256.get(&id).cloned().unwrap_or_default();
        if let Some(ref cache) = self.disk_cache {
            if let Some(ir) = cache.load_ir(&sha) {
                self.stats.disk_hits += 1;
                self.compiled_ir.insert(
                    id,
                    MemoEntry {
                        value: ir,
                        fingerprint: self.parsed[&id].fingerprint,
                        verified_at: self.revision,
                        changed_at: self.revision,
                        deps: vec![QueryKey::Parse(id)],
                    },
                );
                return Ok(&self.compiled_ir[&id].value);
            }
        }

        self.stats.misses += 1;
        let src = self.sources[&id].value.clone();
        let ir = crate::compile_to_module(&src, module_name)?;
        if let Some(ref cache) = self.disk_cache {
            cache.store_ir(&sha, &ir);
        }
        self.compiled_ir.insert(
            id,
            MemoEntry {
                value: ir,
                fingerprint: self.parsed[&id].fingerprint,
                verified_at: self.revision,
                changed_at: self.revision,
                deps: vec![QueryKey::Parse(id)],
            },
        );
        Ok(&self.compiled_ir[&id].value)
    }

    /// Runs full compilation for `id` and `emit`, memoized on the parsed AST
    /// fingerprint so unchanged or comment/whitespace-edited sources hit cache.
    pub fn compile(
        &mut self,
        id: SourceId,
        module_name: &str,
        emit: EmitKind,
    ) -> Result<String, Error> {
        let _ = self.parse(id)?;
        let key = (id, emit);
        if self
            .emitted
            .get(&key)
            .is_some_and(|entry| self.is_entry_fresh(&entry.deps, entry.verified_at))
        {
            self.stats.hits += 1;
            if let Some(entry) = self.emitted.get_mut(&key) {
                entry.verified_at = self.revision;
            }
            return Ok(self.emitted[&key].value.clone());
        }

        self.stats.misses += 1;
        let src = self.sources[&id].value.clone();
        let output = crate::compile(&src, module_name, emit)?;
        let out_fp = fnv1a_hash(output.as_bytes());
        self.emitted.insert(
            key,
            MemoEntry {
                value: output.clone(),
                fingerprint: out_fp,
                verified_at: self.revision,
                changed_at: self.revision,
                deps: vec![QueryKey::Parse(id)],
            },
        );
        Ok(output)
    }

    fn is_entry_fresh(&self, deps: &[QueryKey], verified_at: Revision) -> bool {
        if verified_at == self.revision {
            return true;
        }
        deps.iter().all(|dep| match dep {
            QueryKey::SourceText(sid) => self
                .sources
                .get(sid)
                .is_some_and(|e| e.changed_at <= verified_at),
            QueryKey::Parse(sid) => self
                .parsed
                .get(sid)
                .is_some_and(|e| e.changed_at <= verified_at),
            QueryKey::CompileIr(sid) => self
                .compiled_ir
                .get(sid)
                .is_some_and(|e| e.changed_at <= verified_at),
            QueryKey::Emit(sid, kind) => self
                .emitted
                .get(&(*sid, *kind))
                .is_some_and(|e| e.changed_at <= verified_at),
        })
    }
}

fn fnv1a_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Computes a span-insensitive structural fingerprint of an [`AstModule`],
/// so whitespace/comment edits that shift byte offsets still produce the same
/// fingerprint and trigger early cutoff.
fn ast_structural_fingerprint(ast: &AstModule) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ast.brings.len().hash(&mut hasher);
    for b in &ast.brings {
        format!("{:?}", b.path).hash(&mut hasher);
    }
    ast.consts.len().hash(&mut hasher);
    for c in &ast.consts {
        c.name.name.hash(&mut hasher);
        strip_spans_debug(&format!("{:?}", c.value)).hash(&mut hasher);
    }
    ast.structs.len().hash(&mut hasher);
    for s in &ast.structs {
        s.name.name.hash(&mut hasher);
        for f in &s.fields {
            f.name.name.hash(&mut hasher);
            strip_spans_debug(&format!("{:?}", f.ty)).hash(&mut hasher);
        }
    }
    ast.enums.len().hash(&mut hasher);
    for e in &ast.enums {
        e.name.name.hash(&mut hasher);
        for v in &e.variants {
            v.name.name.hash(&mut hasher);
        }
    }
    ast.functions.len().hash(&mut hasher);
    for f in &ast.functions {
        f.name.name.hash(&mut hasher);
        strip_spans_debug(&format!("{:?}", f.params)).hash(&mut hasher);
        strip_spans_debug(&format!("{:?}", f.return_ty)).hash(&mut hasher);
        strip_spans_debug(&format!("{:?}", f.body)).hash(&mut hasher);
    }
    hasher.finish()
}

fn per_function_fingerprints(ast: &AstModule) -> HashMap<String, u64> {
    let mut map = HashMap::with_capacity(ast.functions.len());
    for f in &ast.functions {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        f.name.name.hash(&mut hasher);
        strip_spans_debug(&format!("{:?}", f.params)).hash(&mut hasher);
        strip_spans_debug(&format!("{:?}", f.return_ty)).hash(&mut hasher);
        strip_spans_debug(&format!("{:?}", f.body)).hash(&mut hasher);
        map.insert(f.name.name.clone(), hasher.finish());
    }
    map
}

/// Strips `Span { start: ByteOffset(...), end: ByteOffset(...) }` substrings
/// from `Debug` representations so whitespace/comment shifts don't perturb
/// structural AST fingerprints.
fn strip_spans_debug(debug_str: &str) -> String {
    let mut out = String::with_capacity(debug_str.len());
    let mut rest = debug_str;
    while let Some(pos) = rest.find("Span {") {
        out.push_str(&rest[..pos]);
        if let Some(end) = rest[pos..].find("} }") {
            rest = &rest[pos + end + 3..];
        } else if let Some(end) = rest[pos..].find('}') {
            rest = &rest[pos + end + 1..];
        } else {
            break;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_query_reuses_identical_and_comment_edited_sources() {
        let mut qe = QueryEngine::new();
        let src1 = "def add(a: i64, b: i64) -> i64 { return a + b }\ndef main() -> i64 { return add(1, 2) }";
        let sid = qe.set_source("main.iris", src1);
        let rev1 = qe.revision();

        let ir1 = qe.compile(sid, "m", EmitKind::Ir).unwrap();
        assert!(ir1.contains("add"));
        assert_eq!(qe.stats().misses, 2); // parse + emit

        // Identical source: revision unchanged, full cache hit.
        let sid_same = qe.set_source("main.iris", src1);
        assert_eq!(sid_same, sid);
        assert_eq!(qe.revision(), rev1);
        let ir2 = qe.compile(sid, "m", EmitKind::Ir).unwrap();
        assert_eq!(ir1, ir2);
        assert!(qe.stats().hits >= 2);

        // Comment/whitespace edit: revision bumps, parse runs, early cutoff
        // prevents re-lowering/re-emitting!
        let src_comment = "// comment added\n\ndef add(a: i64, b: i64) -> i64 { return a + b }\ndef main() -> i64 { return add(1, 2) }";
        qe.set_source("main.iris", src_comment);
        assert!(qe.revision() > rev1);
        let misses_before = qe.stats().misses;
        let ir3 = qe.compile(sid, "m", EmitKind::Ir).unwrap();
        assert_eq!(ir1, ir3);
        assert_eq!(qe.stats().early_cutoffs, 1);
        // Only Parse ran (1 miss), Emit reused via early cutoff!
        assert_eq!(qe.stats().misses, misses_before + 1);
    }

    #[test]
    fn changed_functions_tracks_only_modified_fn() {
        let mut qe = QueryEngine::new();
        let sid = qe.set_source(
            "mod.iris",
            "def foo() -> i64 { return 1 }\ndef bar() -> i64 { return 2 }",
        );
        qe.parse(sid).unwrap();
        assert_eq!(
            qe.changed_functions(sid),
            &["bar".to_string(), "foo".to_string()]
        );

        qe.set_source(
            "mod.iris",
            "def foo() -> i64 { return 1 }\ndef bar() -> i64 { return 99 }",
        );
        qe.parse(sid).unwrap();
        assert_eq!(qe.changed_functions(sid), &["bar".to_string()]);
    }
}
