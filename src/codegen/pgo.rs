//! Profile-guided optimisation for IRIS.
//!
//! Two transforms over the LLVM IR the native backend emits, so either
//! applies to any build (`--pgo-generate`, `--pgo-use FILE`):
//!
//! `instrument` (`--pgo-generate`, `--emit pgo-instrument`)
//! ──────────────────────────────────────────────────────────
//! Every block of every function counts its executions (`atomicrmw add` on
//! a per-function counter array, `@__profc_{fn}`); a module constructor
//! registers the arrays with the runtime, which at exit -- and when a runtime
//! error stops the program -- adds them to the profile file:
//! `IRIS_PROFILE_FILE`, else `iris.profile`. Several runs accumulate.
//!
//! `apply` (`--pgo-use FILE`, `--emit pgo-optimize`)
//! ──────────────────────────────────────────────────
//! From the counts:
//! - `!prof !{!"branch_weights", ...}` on each conditional branch and switch,
//!   from the counts of the blocks it leads to;
//! - `!prof !{!"function_entry_count", i64 N}` on each profiled function;
//! - `cold` on functions the profile saw but that never ran;
//! - the module's `ProfileSummary`, with which LLVM's own analyses tell hot
//!   code from cold (inlining, block placement, splitting).
//!
//! Profile file format
//! ────────────────────
//! One counter per line, `function:block:count`, `#` comments:
//! ```text
//! factorial:entry0:1000
//! factorial:then1:5
//! factorial:else2:995
//! ```
//! Blocks are named as the LLVM IR names them, so a profile describes the
//! program it was recorded from; counters of blocks the program no longer
//! has are ignored, and the output says how many functions matched.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;
use std::sync::Mutex;

use crate::codegen::llvm_ir::{emit_llvm_ir, USER_SYMBOL_PREFIX};
use crate::error::CodegenError;
use crate::ir::block::BlockId;
use crate::ir::module::IrModule;

// ---------------------------------------------------------------------------
// Process-wide settings (the CLI's `--pgo-generate` / `--pgo-use`)
// ---------------------------------------------------------------------------

static INSTRUMENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PROFILE: Mutex<Option<String>> = Mutex::new(None);

/// `--pgo-generate`: every native build of this process counts its blocks.
pub fn set_instrument(on: bool) {
    INSTRUMENT.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn instrumenting() -> bool {
    INSTRUMENT.load(std::sync::atomic::Ordering::Relaxed)
}

/// `--pgo-use`: the profile every native build of this process applies.
pub fn set_profile(text: Option<String>) {
    *PROFILE.lock().unwrap() = text;
}

pub fn profile() -> Option<String> {
    PROFILE.lock().unwrap().clone()
}

/// The settings applied to LLVM IR `ir` emitted for `module`.
pub(crate) fn apply_settings(module: &IrModule, ir: String) -> Result<String, CodegenError> {
    let ir = if instrumenting() {
        instrument(module, &ir)?
    } else {
        ir
    };
    match profile() {
        Some(p) => apply(module, &ir, &ProfileData::parse(&p)),
        None => Ok(ir),
    }
}

// ---------------------------------------------------------------------------
// Profile data
// ---------------------------------------------------------------------------

/// Parsed profile data: maps (function_name, block_label) → execution count.
#[derive(Debug, Default, Clone)]
pub struct ProfileData {
    pub counts: HashMap<(String, String), u64>,
}

impl ProfileData {
    /// Parse profile data from the IRIS text format. A function name may
    /// contain `:` (a module path); the block label and count may not.
    pub fn parse(text: &str) -> Self {
        let mut counts = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.rsplitn(3, ':');
            let (Some(count), Some(label), Some(func)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if let Ok(count) = count.parse::<u64>() {
                *counts
                    .entry((func.to_owned(), label.to_owned()))
                    .or_insert(0) += count;
            }
        }
        Self { counts }
    }

    /// Return the execution count for a block, or `None` if not profiled.
    pub fn block_count(&self, fn_name: &str, block_label: &str) -> Option<u64> {
        self.counts
            .get(&(fn_name.to_owned(), block_label.to_owned()))
            .copied()
    }

    /// The count of a function's entry block.
    pub fn entry_count(&self, fn_name: &str) -> Option<u64> {
        for label in ["entry0", "entry", "bb0"] {
            if let Some(c) = self.block_count(fn_name, label) {
                return Some(c);
            }
        }
        None
    }

    fn has_function(&self, fn_name: &str) -> bool {
        self.counts.keys().any(|(f, _)| f == fn_name)
    }
}

// ---------------------------------------------------------------------------
// The functions of a module's LLVM IR
// ---------------------------------------------------------------------------

/// A function definition in LLVM IR text: its line range and blocks.
struct Function {
    /// The IRIS name: the LLVM name without the user-symbol prefix.
    name: String,
    define: usize,
    end: usize,
    /// (line, label) of each block, in order.
    blocks: Vec<(usize, String)>,
}

/// The functions of `lines`. A native binary's entry is `iris_main` (the C
/// `main` calls it): named `main` here, as in every other build, so a
/// profile recorded from a binary applies to IR and back.
fn functions(module: &IrModule, lines: &[&str]) -> Vec<Function> {
    let entry_alias = module.function_by_name("iris_main").is_none();
    let mut out = Vec::new();
    let mut current: Option<Function> = None;
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("define ") && line.trim_end().ends_with('{') {
            let mut name = defined_name(line).unwrap_or_default();
            if entry_alias && name == "iris_main" {
                name = "main".to_owned();
            }
            current = Some(Function {
                name,
                define: i,
                end: i,
                blocks: Vec::new(),
            });
        } else if *line == "}" {
            if let Some(mut f) = current.take() {
                f.end = i;
                out.push(f);
            }
        } else if let Some(f) = current.as_mut() {
            if let Some(label) = line.strip_suffix(':') {
                if !label.starts_with(' ') && !label.starts_with(';') {
                    f.blocks.push((i, label.to_owned()));
                }
            }
        }
    }
    out
}

fn defined_name(line: &str) -> Option<String> {
    let rest = &line[line.find('@')? + 1..];
    let rest = rest.strip_prefix('"').unwrap_or(rest);
    let name = rest.split(['(', '"']).next()?;
    Some(
        name.strip_prefix(USER_SYMBOL_PREFIX)
            .unwrap_or(name)
            .to_owned(),
    )
}

/// A name usable inside an LLVM global's name.
fn symbol(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn c_string(s: &str) -> (String, usize) {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'"' | b'\\' => write!(out, "\\{b:02X}").unwrap(),
            0x20..=0x7E => out.push(b as char),
            other => write!(out, "\\{other:02X}").unwrap(),
        }
    }
    (out, s.len() + 1)
}

// ---------------------------------------------------------------------------
// Instrumentation
// ---------------------------------------------------------------------------

/// `--emit pgo-instrument`: the module's LLVM IR, counting its blocks.
pub fn emit_pgo_instrument(module: &IrModule) -> Result<String, CodegenError> {
    let ir = emit_llvm_ir(module)?;
    if instrumenting() {
        // `emit_llvm_ir` applied it already.
        return Ok(ir);
    }
    instrument(module, &ir)
}

/// Count every block of every function defined in `base`.
pub fn instrument(module: &IrModule, base: &str) -> Result<String, CodegenError> {
    let lines: Vec<&str> = base.lines().collect();
    // The compiler's own helpers (`__iris_chk_*`) are not the program.
    let funcs: Vec<Function> = functions(module, &lines)
        .into_iter()
        .filter(|f| !f.name.starts_with("__iris_") && !f.blocks.is_empty())
        .collect();

    // Line → counter to bump after it (and its phis).
    let mut bump: HashMap<usize, (String, usize, usize)> = HashMap::new();
    for (fi, f) in funcs.iter().enumerate() {
        let counters = format!("@__profc_{fi}_{}", symbol(&f.name));
        for (bi, (line, _)) in f.blocks.iter().enumerate() {
            bump.insert(*line, (counters.clone(), f.blocks.len(), bi));
        }
    }

    let mut out = String::new();
    writeln!(
        out,
        "; IRIS PGO-instrumented IR: each block counts its executions; the counts are"
    )?;
    writeln!(
        out,
        "; added to IRIS_PROFILE_FILE (default iris.profile) when the program exits."
    )?;
    let mut pending: Option<(String, usize, usize)> = None;
    let mut ctors: Option<String> = None;
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("@llvm.global_ctors = appending global") {
            ctors = Some((*line).to_owned());
            continue;
        }
        if let Some((counters, n, bi)) = pending.take() {
            if line.trim_start().contains("= phi ") {
                writeln!(out, "{line}")?;
                pending = Some((counters, n, bi));
                continue;
            }
            writeln!(
                out,
                "  atomicrmw add ptr getelementptr inbounds ([{n} x i64], ptr {counters}, i64 0, i64 {bi}), i64 1 monotonic"
            )?;
        }
        writeln!(out, "{line}")?;
        if let Some(b) = bump.get(&i) {
            pending = Some(b.clone());
        }
    }

    // Counter arrays, names, and the constructor that registers them.
    writeln!(out)?;
    writeln!(
        out,
        "; ── PGO counters ─────────────────────────────────────────────────────"
    )?;
    for (fi, f) in funcs.iter().enumerate() {
        let sym = format!("{fi}_{}", symbol(&f.name));
        writeln!(
            out,
            "@__profc_{sym} = internal global [{} x i64] zeroinitializer, align 8",
            f.blocks.len()
        )?;
        let (name, len) = c_string(&f.name);
        writeln!(
            out,
            "@__profn_{sym} = private unnamed_addr constant [{len} x i8] c\"{name}\\00\""
        )?;
        let mut label_refs = Vec::new();
        for (bi, (_, label)) in f.blocks.iter().enumerate() {
            let (l, len) = c_string(label);
            writeln!(
                out,
                "@__profb_{sym}_{bi} = private unnamed_addr constant [{len} x i8] c\"{l}\\00\""
            )?;
            label_refs.push(format!("ptr @__profb_{sym}_{bi}"));
        }
        writeln!(
            out,
            "@__profl_{sym} = private unnamed_addr constant [{} x ptr] [{}]",
            f.blocks.len(),
            label_refs.join(", ")
        )?;
    }
    writeln!(
        out,
        "declare void @iris_profile_register(ptr, ptr, ptr, i64)"
    )?;
    writeln!(out, "define internal void @iris_profile_init() {{")?;
    writeln!(out, "entry:")?;
    for (fi, f) in funcs.iter().enumerate() {
        let sym = format!("{fi}_{}", symbol(&f.name));
        writeln!(
            out,
            "  call void @iris_profile_register(ptr @__profn_{sym}, ptr @__profl_{sym}, ptr @__profc_{sym}, i64 {})",
            f.blocks.len()
        )?;
    }
    writeln!(out, "  ret void")?;
    writeln!(out, "}}")?;

    // One `llvm.global_ctors`: the module's own entries, then ours.
    let mut entries = Vec::new();
    if let Some(existing) = &ctors {
        if let (Some(a), Some(b)) = (existing.find("] [{"), existing.rfind("}]")) {
            entries.push(existing[a + 3..b + 1].to_owned());
        }
    }
    entries.push("{ i32, ptr, ptr } { i32 65535, ptr @iris_profile_init, ptr null }".to_owned());
    let count = entries
        .iter()
        .map(|e| e.matches("{ i32, ptr, ptr } {").count())
        .sum::<usize>();
    writeln!(
        out,
        "@llvm.global_ctors = appending global [{count} x {{ i32, ptr, ptr }}] [{}]",
        entries.join(", ")
    )?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Applying a profile
// ---------------------------------------------------------------------------

/// `--emit pgo-optimize`: the module's LLVM IR, annotated from `profile`.
pub fn emit_pgo_optimize(module: &IrModule, profile: &str) -> Result<String, CodegenError> {
    let ir = emit_llvm_ir(module)?;
    if self::profile().is_some() {
        // `emit_llvm_ir` applied it already.
        return Ok(ir);
    }
    apply(module, &ir, &ProfileData::parse(profile))
}

/// LLVM's branch weights are 32-bit: scale a set of counts to fit.
fn weights(counts: &[u64]) -> Vec<u32> {
    let max = counts.iter().copied().max().unwrap_or(0);
    let scale = if max > u32::MAX as u64 {
        max.div_ceil(u32::MAX as u64)
    } else {
        1
    };
    counts.iter().map(|c| (c / scale) as u32).collect()
}

/// The `branch_weights` of a branch or switch line: its targets' counts.
fn branch_targets(line: &str) -> Vec<String> {
    line.split("label %")
        .skip(1)
        .map(|t| t.split([',', ' ', ']']).next().unwrap_or("").to_owned())
        .collect()
}

/// Annotate `base`, LLVM IR emitted for `module`, with `pdata`.
pub fn apply(module: &IrModule, base: &str, pdata: &ProfileData) -> Result<String, CodegenError> {
    let lines: Vec<&str> = base.lines().collect();
    let funcs = functions(module, &lines);

    // Unnamed metadata numbers already taken.
    let mut next_md = lines
        .iter()
        .filter_map(|l| l.strip_prefix('!'))
        .filter_map(|l| l.split(|c: char| !c.is_ascii_digit()).next())
        .filter_map(|n| n.parse::<u32>().ok())
        .max()
        .map_or(0, |m| m + 1);
    let mut metadata: Vec<String> = Vec::new();
    let mut md = |text: String, metadata: &mut Vec<String>| {
        let id = next_md;
        next_md += 1;
        metadata.push(format!("!{id} = {text}"));
        id
    };

    // Line edits: a define line gains attributes and `!prof`; a branch gains
    // `!prof`.
    let mut define_edit: HashMap<usize, String> = HashMap::new();
    let mut branch_edit: HashMap<usize, u32> = HashMap::new();
    let mut matched = 0usize;
    let mut heat: BTreeMap<String, Option<u64>> = BTreeMap::new();
    let mut all_counts: Vec<u64> = Vec::new();
    let mut max_function = 0u64;

    for f in &funcs {
        if f.name.starts_with("__iris_") {
            continue;
        }
        if !pdata.has_function(&f.name) {
            heat.insert(f.name.clone(), None);
            continue;
        }
        let count = |label: &str| pdata.block_count(&f.name, label);
        let known: Vec<u64> = f.blocks.iter().filter_map(|(_, l)| count(l)).collect();
        if known.is_empty() {
            // Recorded from a different version of this function.
            heat.insert(f.name.clone(), None);
            continue;
        }
        matched += 1;
        all_counts.extend(&known);
        let entry = f.blocks.first().and_then(|(_, l)| count(l)).unwrap_or(0);
        max_function = max_function.max(entry);
        heat.insert(f.name.clone(), Some(entry));

        let id = md(
            format!("!{{!\"function_entry_count\", i64 {entry}}}"),
            &mut metadata,
        );
        let head = lines[f.define].trim_end().trim_end_matches('{').trim_end();
        let cold = if entry == 0 && !head.contains(" cold") {
            " cold"
        } else {
            ""
        };
        define_edit.insert(f.define, format!("{head}{cold} !prof !{id} {{"));

        for (i, line) in lines.iter().enumerate().take(f.end).skip(f.define + 1) {
            let t = line.trim_start();
            if !(t.starts_with("br i1 ") || t.starts_with("switch ")) || line.contains("!prof") {
                continue;
            }
            let targets = branch_targets(line);
            if targets.len() < 2 {
                continue;
            }
            let counts: Vec<u64> = targets.iter().map(|t| count(t).unwrap_or(0)).collect();
            if counts.iter().all(|c| *c == 0) {
                continue;
            }
            let w: Vec<String> = weights(&counts)
                .iter()
                .map(|w| format!("i32 {w}"))
                .collect();
            let id = md(
                format!("!{{!\"branch_weights\", {}}}", w.join(", ")),
                &mut metadata,
            );
            branch_edit.insert(i, id);
        }
    }

    let mut out = String::new();
    writeln!(
        out,
        "; IRIS PGO-optimized IR: profile matched {matched} of {} functions",
        heat.len()
    )?;
    for (i, line) in lines.iter().enumerate() {
        if let Some(d) = define_edit.get(&i) {
            writeln!(out, "{d}")?;
        } else if let Some(id) = branch_edit.get(&i) {
            // After the operands; a `switch`'s, after its `[ ... ]`.
            writeln!(out, "{line}, !prof !{id}")?;
        } else {
            writeln!(out, "{line}")?;
        }
    }

    // The profile summary, as LLVM's ProfileSummaryBuilder computes it: the
    // counts sorted, and for each cutoff (per million of the total) the
    // smallest count reached and how many counts that takes.
    let summary = (!all_counts.is_empty()).then(|| {
        let mut sorted = all_counts.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        let total: u128 = sorted.iter().map(|c| *c as u128).sum();
        let cutoffs = [
            10000u32, 100000, 200000, 300000, 400000, 500000, 600000, 700000, 800000, 900000,
            950000, 990000, 999000, 999900, 999990, 999999,
        ];
        let mut detailed = Vec::new();
        let (mut acc, mut idx) = (0u128, 0usize);
        for cutoff in cutoffs {
            let want = total * cutoff as u128 / 1_000_000;
            while idx < sorted.len() && acc < want.max(1) {
                acc += sorted[idx] as u128;
                idx += 1;
            }
            let min = sorted.get(idx.saturating_sub(1)).copied().unwrap_or(0);
            detailed.push((cutoff, min, idx));
        }
        (total, sorted[0], sorted.len(), detailed)
    });
    if let Some((total, max, n, detailed)) = &summary {
        let mut ids = Vec::new();
        for (cutoff, min, num) in detailed {
            ids.push(md(
                format!("!{{i32 {cutoff}, i64 {min}, i64 {num}}}"),
                &mut metadata,
            ));
        }
        let det = md(
            format!(
                "!{{{}}}",
                ids.iter()
                    .map(|i| format!("!{i}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            &mut metadata,
        );
        let fields = [
            "!{!\"ProfileFormat\", !\"InstrProf\"}".to_owned(),
            format!("!{{!\"TotalCount\", i64 {total}}}"),
            format!("!{{!\"MaxCount\", i64 {max}}}"),
            format!("!{{!\"MaxInternalCount\", i64 {max}}}"),
            format!("!{{!\"MaxFunctionCount\", i64 {max_function}}}"),
            format!("!{{!\"NumCounts\", i64 {n}}}"),
            format!("!{{!\"NumFunctions\", i64 {matched}}}"),
            format!("!{{!\"DetailedSummary\", !{det}}}"),
        ];
        let field_ids: Vec<u32> = fields
            .iter()
            .map(|f| md(f.clone(), &mut metadata))
            .collect();
        let ps = md(
            format!(
                "!{{{}}}",
                field_ids
                    .iter()
                    .map(|i| format!("!{i}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            &mut metadata,
        );
        let flag = md(
            format!("!{{i32 1, !\"ProfileSummary\", !{ps}}}"),
            &mut metadata,
        );
        // One `!llvm.module.flags`: the module's own, then the summary.
        let mut text = out.clone();
        if let Some(at) = text.find("!llvm.module.flags = !{") {
            let close = at + text[at..].find('}').unwrap_or(0);
            text.insert_str(close, &format!(", !{flag}"));
            out = text;
        } else {
            writeln!(out, "!llvm.module.flags = !{{!{flag}}}")?;
        }
    }

    writeln!(out)?;
    writeln!(
        out,
        "; ── PGO metadata ─────────────────────────────────────────────────────"
    )?;
    for m in &metadata {
        writeln!(out, "{m}")?;
    }

    // Each function's heat, against the summary's hot and cold thresholds
    // (the counts reaching 99% and 99.9999% of the total).
    let (hot, cold) = summary
        .as_ref()
        .map(|(_, _, _, d)| {
            let at = |c: u32| {
                d.iter()
                    .find(|(cut, _, _)| *cut == c)
                    .map_or(0, |(_, m, _)| *m)
            };
            (at(990000), at(999999))
        })
        .unwrap_or((0, 0));
    writeln!(out)?;
    writeln!(
        out,
        "; ── PGO function heat ───────────────────────────────────────────────"
    )?;
    for (name, entry) in &heat {
        let label = match entry {
            None => "UNPROFILED",
            Some(0) => "COLD",
            Some(c) if *c >= hot && hot > 0 => "HOT",
            Some(c) if *c < cold => "COLD",
            Some(_) => "WARM",
        };
        writeln!(
            out,
            "; {label} {name} (entry count: {})",
            entry.map_or("-".to_owned(), |c| c.to_string())
        )?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Profile generation helper (for testing)
// ---------------------------------------------------------------------------

fn block_label_str(name: Option<&str>, id: BlockId) -> String {
    format!("{}{}", name.unwrap_or("bb"), id.0)
}

/// A synthetic profile for a module: every entry block `entry_count`, every
/// other block half of it. For tests that need a profile without a run.
pub fn generate_synthetic_profile(module: &IrModule, entry_count: u64) -> String {
    let mut out = String::new();
    for func in module.functions() {
        for (bi, block) in func.blocks().iter().enumerate() {
            let label = block_label_str(block.name.as_deref(), block.id);
            let count = if bi == 0 {
                entry_count
            } else {
                entry_count / 2
            };
            out.push_str(&format!("{}:{}:{}\n", func.name, label, count));
        }
    }
    out
}
