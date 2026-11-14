//! Lazy monomorphisation: instantiation happens on demand at call sites,
//! not eagerly at definition time. The zero-call generic function is never
//! lowered; every instantiation lowered is lowered exactly once.
//!
//! `IRIS_MONO=report` prints every instantiation this compile produced.
//! `IRIS_MONO=check` — like `IRIS_THIR=check` — fails the compile when an
//! instantiation would be lowered twice (a re-entrant mono-name collision).

use std::sync::Mutex;

/// Statistics of the last compile's monomorphisation on this process, if one has run.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct MonoStats {
    pub instantiations: Vec<String>,
    pub cache_hits: usize,
    pub cache_misses: usize,
}

static LAST_STATS: Mutex<Option<MonoStats>> = Mutex::new(None);

thread_local! {
    static ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static INSTANTIATIONS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    static HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MISSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn active() -> bool {
    ACTIVE.with(|a| a.get())
}

/// Begin a fresh monomorphisation audit for this compile.
pub(crate) fn install() {
    ACTIVE.with(|a| a.set(true));
    INSTANTIATIONS.with(|i| i.borrow_mut().clear());
    HITS.with(|h| h.set(0));
    MISSES.with(|m| m.set(0));
}

/// Stop the audit and store its result for `last_stats()`.
pub(crate) fn uninstall() {
    let instantiations = INSTANTIATIONS.with(|i| i.borrow().clone());
    let cache_hits = HITS.with(|h| h.get());
    let cache_misses = MISSES.with(|m| m.get());
    *LAST_STATS.lock().unwrap_or_else(|e| e.into_inner()) = Some(MonoStats {
        instantiations,
        cache_hits,
        cache_misses,
    });
    ACTIVE.with(|a| a.set(false));
}

/// Statistics of the last compile's monomorphisation on this process.
pub fn last_stats() -> Option<MonoStats> {
    LAST_STATS.lock().ok().and_then(|s| s.clone())
}

/// `IRIS_MONO`: `report` prints the instantiations, `check` also fails an incomplete one.
pub(crate) fn mode() -> Option<String> {
    std::env::var("IRIS_MONO")
        .ok()
        .filter(|m| m == "report" || m == "check")
}

pub(crate) fn report() {
    if let Some(s) = last_stats() {
        eprintln!(
            "mono: {} instantiation{} ({} cache hit{}, {} miss{})",
            s.instantiations.len(),
            if s.instantiations.len() == 1 { "" } else { "s" },
            s.cache_hits,
            if s.cache_hits == 1 { "" } else { "s" },
            s.cache_misses,
            if s.cache_misses == 1 { "" } else { "es" },
        );
        for name in &s.instantiations {
            eprintln!("  {name}");
        }
    }
}

/// The `IRIS_MONO=check` error: a cached instantiation re-entered uninitialised.
pub(crate) fn incomplete(problems: Vec<String>) -> crate::error::LowerError {
    let detail = problems.join("; ");
    crate::error::LowerError::rich(
        "E0902",
        crate::diagnostics::Diagnostic::error(format!(
            "internal compiler error: incomplete monomorphisation: {detail}"
        ))
        .with_code("E0902"),
    )
}

pub(crate) fn check() -> Result<(), crate::error::LowerError> {
    let s = last_stats().unwrap_or_default();
    let mut counts = std::collections::HashMap::new();
    for name in &s.instantiations {
        *counts.entry(name).or_insert(0_usize) += 1;
    }
    let mut problems = Vec::new();
    for (name, n) in counts {
        if n > 1 {
            problems.push(format!("{name} lowered {n} times"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(incomplete(problems))
    }
}

/// Record one instantiation. `hit` is true when the mono cache already held this
/// mangled name (no lowering needed); false when it is being lowered now.
pub(crate) fn record_instantiation(mangled: &str, hit: bool) {
    if !active() {
        return;
    }
    INSTANTIATIONS.with(|i| i.borrow_mut().push(mangled.to_string()));
    if hit {
        HITS.with(|h| h.set(h.get() + 1));
    } else {
        MISSES.with(|m| m.set(m.get() + 1));
    }
}
