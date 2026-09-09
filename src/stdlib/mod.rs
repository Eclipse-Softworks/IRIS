//! IRIS standard library registry.
//!
//! Stdlib modules are embedded as source strings via `include_str!`.
//! Use `stdlib_source("name")` to retrieve the IRIS source for a module.

/// Canonical completion/discovery list for every embedded standard-library
/// module. Editor integrations consume this instead of maintaining a second,
/// inevitably stale list.
pub const STDLIB_MODULE_NAMES: &[&str] = &[
    "adaptive",
    "ais",
    "async",
    "bitset",
    "collections",
    "container",
    "crypto",
    "csv",
    "dataframe",
    "dataset",
    "deque",
    "ffi",
    "fmt",
    "fs",
    "heap",
    "http",
    "http_server",
    "iter",
    "json",
    "kv",
    "llm",
    "log",
    "math",
    "meta",
    "meta_learning",
    "ml",
    "net",
    "nn",
    "os",
    "path",
    "queue",
    "reflect",
    "rl",
    "ros2",
    "serial",
    "set",
    "speculation",
    "sql",
    "stochastic",
    "string",
    "svg",
    "table",
    "tensor",
    "tensorx",
    "termplot",
    "testing",
    "time",
    "uncertainty",
    "unicode",
];

/// Returns the IRIS source for the named stdlib module, or `None` if unknown.
pub fn stdlib_source(name: &str) -> Option<&'static str> {
    match name {
        "math" => Some(include_str!("math.iris")),
        "string" => Some(include_str!("string.iris")),
        "collections" => Some(include_str!("collections.iris")),
        "fmt" => Some(include_str!("fmt.iris")),
        "set" => Some(include_str!("set.iris")),
        "heap" => Some(include_str!("heap.iris")),
        "time" => Some(include_str!("time.iris")),
        "stochastic" => Some(include_str!("stochastic.iris")),
        "fs" => Some(include_str!("fs.iris")),
        "json" => Some(include_str!("json.iris")),
        "csv" => Some(include_str!("csv.iris")),
        "http" => Some(include_str!("http.iris")),
        "kv" => Some(include_str!("kv.iris")),
        "sql" => Some(include_str!("sql.iris")),
        // Phase 105: New stdlib modules
        "svg" => Some(include_str!("svg.iris")),
        "termplot" => Some(include_str!("termplot.iris")),
        "iter" => Some(include_str!("iter.iris")),
        "deque" => Some(include_str!("deque.iris")),
        "crypto" => Some(include_str!("crypto.iris")),
        "os" => Some(include_str!("os.iris")),
        "ffi" => Some(include_str!("ffi.iris")),
        "serial" => Some(include_str!("serial.iris")),
        "container" => Some(include_str!("container.iris")),
        "queue" => Some(include_str!("queue.iris")),
        "path" => Some(include_str!("path.iris")),
        "table" => Some(include_str!("table.iris")),
        "dataset" => Some(include_str!("dataset.iris")),
        "dataframe" => Some(include_str!("dataframe.iris")),
        "bitset" => Some(include_str!("bitset.iris")),
        "log" => Some(include_str!("log.iris")),
        "meta_learning" => Some(include_str!("meta_learning.iris")),
        "async" => Some(include_str!("async.iris")),
        "testing" => Some(include_str!("testing.iris")),
        // ML / AI modules
        "ml" => Some(include_str!("ml.iris")),
        "llm" => Some(include_str!("llm.iris")),
        "nn" => Some(include_str!("nn.iris")),
        "tensor" | "tensorx" => Some(include_str!("tensor.iris")),
        // Autonomous Intelligent Systems
        "ais" => Some(include_str!("ais.iris")),
        "rl" => Some(include_str!("rl.iris")),
        "ros2" => Some(include_str!("ros2.iris")),
        // Adaptive AI (v1.0.1)
        "adaptive" => Some(include_str!("adaptive.iris")),
        "uncertainty" => Some(include_str!("uncertainty.iris")),
        // Networking
        "http_server" => Some(include_str!("http_server.iris")),
        "net" => Some(include_str!("net.iris")),
        "unicode" => Some(include_str!("unicode.iris")),
        // Self-Evolving & Reflection modules
        "reflect" => Some(include_str!("reflect.iris")),
        "meta" => Some(include_str!("meta.iris")),
        "speculation" => Some(include_str!("speculation.iris")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_registry_only_advertises_real_modules() {
        for module in STDLIB_MODULE_NAMES {
            assert!(
                stdlib_source(module).is_some(),
                "registered stdlib module '{}' has no source",
                module
            );
        }
    }
}
