//! CLI argument parsing using clap derive.
//!
//! Exports `parse_args` (backward-compatible with the old signature),
//! `version_text`, `help_text`, and the `CliArgs` / `ParseArgsResult` types
//! used by `main.rs`.

use std::path::PathBuf;

use clap::CommandFactory;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::EmitKind;

// ---------------------------------------------------------------------------
// EmitKind as a clap ValueEnum (so --emit values are auto-validated)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EmitKindCli {
    Ir,
    Llvm,
    #[clap(name = "llvm-complete")]
    LlvmComplete,
    Cuda,
    #[clap(name = "cuda-ptx")]
    CudaPtx,
    Simd,
    Jit,
    #[clap(name = "pgo-instrument")]
    PgoInstrument,
    #[clap(name = "pgo-optimize")]
    PgoOptimize,
    Graph,
    Onnx,
    #[clap(name = "onnx-binary")]
    OnnxBinary,
    Eval,
    Binary,
    #[clap(name = "tensorrt")]
    TensorRt,
    #[clap(name = "python-ext")]
    PythonExt,
}

impl From<EmitKindCli> for EmitKind {
    fn from(val: EmitKindCli) -> Self {
        match val {
            EmitKindCli::Ir => EmitKind::Ir,
            EmitKindCli::Llvm => EmitKind::Llvm,
            EmitKindCli::LlvmComplete => EmitKind::LlvmComplete,
            EmitKindCli::Cuda => EmitKind::Cuda,
            EmitKindCli::CudaPtx => EmitKind::CudaPtx,
            EmitKindCli::Simd => EmitKind::Simd,
            EmitKindCli::Jit => EmitKind::Jit,
            EmitKindCli::PgoInstrument => EmitKind::PgoInstrument,
            EmitKindCli::PgoOptimize => EmitKind::PgoOptimize,
            EmitKindCli::Graph => EmitKind::Graph,
            EmitKindCli::Onnx => EmitKind::Onnx,
            EmitKindCli::OnnxBinary => EmitKind::OnnxBinary,
            EmitKindCli::Eval => EmitKind::Eval,
            EmitKindCli::Binary => EmitKind::Binary,
            EmitKindCli::TensorRt => EmitKind::TensorRt,
            EmitKindCli::PythonExt => EmitKind::PythonExt,
        }
    }
}

/// Output format for diagnostics (rustc-style human text vs. machine-readable json).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ErrorFormatCli {
    #[default]
    Human,
    Json,
}

// ---------------------------------------------------------------------------
// Clap CLI definition
// ---------------------------------------------------------------------------

/// IRIS — Intermediate Representation for Intelligent Systems compiler.
#[derive(Parser)]
#[command(name = "iris", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Output kind
    #[arg(long, value_enum, default_value_t = EmitKindCli::Ir)]
    pub emit: EmitKindCli,

    /// Write output to <file> instead of stdout
    #[arg(short = 'o', long = "output")]
    pub output: Option<PathBuf>,

    /// Diagnostic error format (human or json)
    #[arg(long = "error-format", value_enum, default_value_t = ErrorFormatCli::Human, global = true)]
    pub error_format: ErrorFormatCli,

    /// Target preset/triple for LLVM and native builds
    #[arg(long = "target")]
    pub target: Option<String>,

    /// Dump IR to stderr after this pass
    #[arg(long = "dump-ir-after")]
    pub dump_ir_after: Option<String>,

    /// Legacy interpreter guardrail (max steps, default: 1 000 000)
    #[arg(long = "max-steps", default_value_t = 1_000_000)]
    pub max_steps: usize,

    /// Interpreter call-depth guard.
    ///
    /// Default is well under the depth at which the interpreter exhausts its
    /// stack (~350 frames on a 64 MiB stack), so exceeding it produces a
    /// diagnostic instead of killing the process. Raise it if you need deeper
    /// recursion and accept the risk.
    #[arg(long = "max-depth", default_value_t = 250)]
    pub max_depth: usize,

    /// Disable incremental compilation cache
    #[arg(long = "no-cache", global = true)]
    pub no_cache: bool,

    /// Run with sandboxed security (deny fs/network/ffi/process)
    #[arg(long = "sandbox", global = true)]
    pub sandbox: bool,

    /// Require every effectful function to declare an `effect` clause that
    /// covers what it does; a violation fails the build
    #[arg(long = "strict-effects", global = true)]
    pub strict_effects: bool,

    /// Native code: run element-wise `par for` bodies as explicit vector
    /// kernels (AVX2 on x86-64, NEON on AArch64) and enable LLVM's loop
    /// vectorizer on every other loop
    #[arg(long = "simd", global = true)]
    pub simd: bool,

    /// Native code: count how often each block runs; the program adds the
    /// counts to IRIS_PROFILE_FILE (default iris.profile) when it exits
    #[arg(long = "pgo-generate", global = true)]
    pub pgo_generate: bool,

    /// Native code: optimise with the profile a `--pgo-generate` build
    /// recorded (branch weights, entry counts, hot and cold code)
    #[arg(long = "pgo-use", global = true, value_name = "FILE")]
    pub pgo_use: Option<PathBuf>,

    /// ONNX and TensorRT: the model's trained parameters, a JSON object of
    /// `"layer.weight": nested lists` (else freshly initialised)
    #[arg(long = "weights", global = true, value_name = "FILE")]
    pub weights: Option<PathBuf>,

    /// Native code: run `par for` loops whose bodies work on numbers and
    /// tensors on the GPU (`cuda`: NVIDIA, through the CUDA driver)
    #[arg(long = "gpu", global = true, value_name = "API")]
    pub gpu: Option<String>,

    /// Native builds: emit DWARF debug info mapping machine code to IRIS
    /// source lines, compiled at -O0, for gdb/lldb/VS Code debugging
    #[arg(short = 'g', long = "debug", global = true)]
    pub debug: bool,

    /// Define a preprocessor symbol for `#ifdef`/`#if`, or with a value a
    /// macro the source's uses of NAME become (repeatable)
    #[arg(
        short = 'D',
        long = "define",
        global = true,
        value_name = "NAME[=VALUE]"
    )]
    pub defines: Vec<String>,

    /// Input file
    pub file: Option<PathBuf>,

    /// Arguments for the program, after `--` (`iris file.iris --emit eval
    /// -- a b`); the program sees them as `args()[1..]`
    #[arg(last = true)]
    pub program_args: Vec<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Build a native binary (same as --emit binary)
    Build {
        /// Input file
        file: Option<PathBuf>,
        /// Output file path
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// Target triple (or native/wasm32/cortex-m4f/...)
        #[arg(long)]
        target: Option<String>,
    },
    /// Build and run the binary
    Run {
        /// Input file
        file: Option<PathBuf>,
        /// Output file path
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// Target triple (or native/wasm32/cortex-m4f/...)
        #[arg(long)]
        target: Option<String>,
        /// Arguments for the program, after `--` (`iris run tool.iris -- a b`)
        #[arg(last = true)]
        program_args: Vec<String>,
    },
    /// Build an allocation-free bare-metal component bundle
    Embedded {
        /// Input file
        file: PathBuf,
        /// Hardware profile: arduino-uno, cortex-m4f, cortex-m33, esp32-c3, or esp32
        #[arg(long)]
        target: String,
        /// Output directory (defaults beside the input file)
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// Zero-argument i64 entry function used by the board harness
        #[arg(long, default_value = "main")]
        entry: String,
    },
    /// Evaluate and hot-swap an evolved policy, or a whole program, through
    /// seven gates
    Evolve {
        /// Trusted baseline IRIS source
        #[arg(long)]
        baseline: PathBuf,
        /// Candidate IRIS source to validate and promote
        #[arg(long)]
        candidate: PathBuf,
        /// JSON canary cases for a policy: `[{ "input": i64, "expected": i64 }]`
        /// for the `i64` ABI, `[{ "inputs": [f64, ...], "expected": f64 }]` for
        /// `f64:N`, `[{ "input": {field: value, ...}, "expected": {...} }]` for
        /// `record`
        #[arg(long)]
        cases: Option<PathBuf>,
        /// Evolve a whole program instead: its `iris-evolution-program/2`
        /// manifest (gateways), judged by --scenarios; program state is handed
        /// over from the live generation
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// JSON scenarios for --manifest: `[{ "name": ..., "state": {...},
        /// "steps": [{ "gateway": ..., "input": ..., "expected": ... }] }]`
        #[arg(long)]
        scenarios: Option<PathBuf>,
        /// Required test of a program (`() -> i64`, must return 0);
        /// repeatable, default `test_program`
        #[arg(long = "test")]
        test: Vec<String>,
        /// Trusted constitution text whose hash is pinned below
        #[arg(long)]
        constitution: PathBuf,
        /// Expected lowercase SHA-256 of the constitution file
        #[arg(long = "constitution-sha256")]
        constitution_sha256: String,
        /// Append-only JSONL decision log
        #[arg(long)]
        audit: PathBuf,
        /// Independently protected audit-head checkpoint (required)
        #[arg(long = "audit-head")]
        audit_head: PathBuf,
        /// Lowest constitution-approved output (an integer for `i64`, a
        /// number for `f64:N`); unbounded when omitted
        #[arg(long = "min-output", allow_hyphen_values = true)]
        min_output: Option<String>,
        /// Highest constitution-approved output; unbounded when omitted
        #[arg(long = "max-output", allow_hyphen_values = true)]
        max_output: Option<String>,
        /// Policy ABI: `i64` for `(i64) -> i64`, `f64:N` for `N` f64 inputs
        /// returning f64 (1 <= N <= 8), or `record` for a record of f64, i64
        /// and bool fields in and out
        #[arg(long, default_value = "i64")]
        abi: String,
        /// Constitution bound on one output field of a `record` policy,
        /// `FIELD=MIN:MAX` (either side may be empty); repeatable. The
        /// --min-output/--max-output range applies to every numeric field.
        #[arg(long = "bound", allow_hyphen_values = true)]
        bound: Vec<String>,
        /// Require the IR-level allocation-free proof for the policy's call graph
        #[arg(long = "require-allocation-free")]
        require_allocation_free: bool,
        /// Where candidate code runs during validation: `sandbox` (native
        /// build inside the worker sandbox; fails closed where no sandbox is
        /// enforced) or `interpreter`
        #[arg(long = "validate", default_value = "sandbox")]
        validate: String,
        #[command(flatten)]
        audit_trust: AuditTrustArgs,
    },
    /// Compile and atomically activate a whole program without behavioral gates
    #[command(name = "evolve-unrestricted")]
    EvolveUnrestricted {
        /// Candidate IRIS source
        #[arg(long)]
        candidate: PathBuf,
        /// `iris-evolution-program/2` manifest JSON
        #[arg(long)]
        manifest: PathBuf,
        /// Exact acknowledgement phrase printed in the command help
        #[arg(long = "acknowledge-unsafe")]
        acknowledge_unsafe: String,
    },
    /// Search for a policy by genetic programming, then optionally promote the
    /// winner through the seven gates
    #[command(name = "evolve-search")]
    EvolveSearch {
        /// Trusted baseline IRIS source (required with --promote)
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// JSON canary cases: `[{ "input": i64, "expected": i64 }]` for the
        /// `i64` ABI, `[{ "inputs": [f64, ...], "expected": f64 }]` for `f64:N`
        #[arg(long)]
        cases: PathBuf,
        /// Number of generations to evolve
        #[arg(long, default_value_t = 30)]
        generations: usize,
        /// Size of population
        #[arg(long = "pop-size", default_value_t = 50)]
        pop_size: usize,
        /// Mutation probability (0.0 to 1.0)
        #[arg(long = "mutation-rate", default_value_t = 0.3)]
        mutation_rate: f64,
        /// Crossover probability (0.0 to 1.0)
        #[arg(long = "crossover-rate", default_value_t = 0.7)]
        crossover_rate: f64,
        /// Tournament selection size
        #[arg(long = "tournament-size", default_value_t = 4)]
        tournament_size: usize,
        /// Elites preserved per generation
        #[arg(long = "elite-count", default_value_t = 2)]
        elite_count: usize,
        /// Maximum AST depth
        #[arg(long = "max-depth", default_value_t = 6)]
        max_depth: usize,
        /// Parsimony pressure weight per AST node
        #[arg(long = "parsimony-weight", default_value_t = 0.001)]
        parsimony_weight: f64,
        /// Optional target loss threshold to stop early
        #[arg(long = "target-loss")]
        target_loss: Option<f64>,
        /// Random number seed for reproducible evolution
        #[arg(long)]
        seed: Option<u64>,
        /// Output path for the evolved candidate IRIS source file
        #[arg(long = "out-candidate")]
        out_candidate: Option<PathBuf>,
        /// Automatically promote the winning candidate through the 7 gates
        #[arg(long)]
        promote: bool,
        /// Trusted constitution text (required if --promote)
        #[arg(long)]
        constitution: Option<PathBuf>,
        /// Expected SHA-256 of constitution (required if --promote)
        #[arg(long = "constitution-sha256")]
        constitution_sha256: Option<String>,
        /// Append-only JSONL decision log (required if --promote)
        #[arg(long)]
        audit: Option<PathBuf>,
        /// Independently protected audit-head checkpoint (required if --promote)
        #[arg(long = "audit-head")]
        audit_head: Option<PathBuf>,
        /// Lowest constitution-approved output (an integer for `i64`, a
        /// number for `f64:N`); unbounded when omitted
        #[arg(long = "min-output", allow_hyphen_values = true)]
        min_output: Option<String>,
        /// Highest constitution-approved output; unbounded when omitted
        #[arg(long = "max-output", allow_hyphen_values = true)]
        max_output: Option<String>,
        /// Policy ABI: `i64` searches integer genomes for `(i64) -> i64`;
        /// `f64:N` searches control laws over N f64 inputs (1 <= N <= 8)
        #[arg(long, default_value = "i64")]
        abi: String,
        #[command(flatten)]
        audit_trust: AuditTrustArgs,
    },
    /// Run a control loop whose controller evolves while it runs: candidates
    /// (and a background search's winner) are promoted through the seven
    /// gates between steps and rolled back on a field violation
    #[command(name = "evolve-live")]
    EvolveLive(EvolveLiveArgs),
    /// Serve a stateful program over HTTP and promote new versions of it
    /// while it runs, carrying its state over
    #[command(name = "evolve-serve")]
    EvolveServe(EvolveServeArgs),
    /// Run a service behind the autonomic daemon: a reverse proxy that
    /// measures it and applies the policy that best holds its SLA
    #[command(name = "service-daemon")]
    ServiceDaemon {
        /// The service, as an http:// URL (e.g. http://127.0.0.1:8080)
        #[arg(long)]
        upstream: String,
        /// The port clients reach the service through
        #[arg(long, default_value_t = 8088)]
        listen: u16,
        /// Monitoring intervals to run; 0 runs until stopped
        #[arg(long, default_value_t = 0)]
        ticks: usize,
        /// Length of a monitoring interval, in milliseconds
        #[arg(long = "tick-ms", default_value_t = 1000)]
        tick_ms: u64,
        /// In-flight requests from which the shedding policy answers 429
        #[arg(long = "max-inflight", default_value_t = 64)]
        max_inflight: usize,
        /// Target P99 SLA threshold in milliseconds
        #[arg(long = "target-p99", default_value_t = 15.0)]
        target_p99: f64,
        /// Maximum allowed error budget ratio
        #[arg(long = "error-budget", default_value_t = 0.01)]
        error_budget: f64,
        /// Print nothing while running
        #[arg(long)]
        headless: bool,
        /// Write the JSON audit ledger here (after every interval when
        /// running until stopped)
        #[arg(long)]
        audit: Option<PathBuf>,
        /// Serve the live dashboard and Prometheus metrics
        #[arg(long)]
        serve: bool,
        /// The dashboard and metrics port
        #[arg(long, default_value_t = 9090)]
        port: u16,
    },
    /// Inspect source through the compiler-hosted typed metaprogramming API
    Meta {
        /// Input IRIS source
        file: PathBuf,
        /// Emit optimized IR instead of the typed JSON analysis
        #[arg(long = "emit-ir")]
        emit_ir: bool,
    },
    /// Start an interactive REPL session
    Repl,
    /// Start the LSP server (JSON-RPC on stdin/stdout)
    Lsp {
        /// Accepted and ignored: stdio is the only transport this server has.
        ///
        /// `vscode-languageclient` appends `--stdio` to the server's argv
        /// whenever `TransportKind.stdio` is configured, so a server that
        /// rejects the flag exits 1 on spawn and the client reports
        /// `write EPIPE`. See known-issues #59.
        #[arg(long)]
        stdio: bool,
    },
    /// Start the DAP debug adapter (JSON-RPC on stdin/stdout)
    Dap {
        /// Accepted and ignored, for the same reason as `lsp --stdio`.
        #[arg(long)]
        stdio: bool,
    },
    /// Package manager commands (all remaining args passthrough)
    #[command(trailing_var_arg = true)]
    Pkg {
        /// Subcommand and arguments for the package manager
        args: Vec<String>,
    },
    /// Generate IRIS FFI bindings from a C/C++ header (.h/.hpp)
    Bindgen {
        /// Input C/C++ header path
        header: PathBuf,
        /// Optional output .iris file path (defaults to stdout)
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// Library name to emit in @link(lib = "...")
        #[arg(short = 'l', long = "lib")]
        lib: Option<String>,
        /// Treat header as C++ (namespaces, classes, templates, overloads)
        #[arg(long = "cpp")]
        cpp: bool,
        /// Add header include search directory
        #[arg(short = 'I', long = "include")]
        include_dirs: Vec<PathBuf>,
        /// Define preprocessor macro NAME or NAME=VALUE
        #[arg(short = 'D', long = "define")]
        defines: Vec<String>,
        /// Write an extern "C" .cpp wrapper shim for C++ APIs
        #[arg(long = "shim-out")]
        shim_out: Option<PathBuf>,
        /// Disable external clang/clang++ AST driver and use built-in parser
        #[arg(long = "no-clang")]
        no_clang: bool,
    },
    /// Run performance benchmarks
    Bench {
        /// Input file
        file: PathBuf,
        /// Number of measured iterations
        #[arg(
            short = 'n',
            long = "iterations",
            default_value_t = crate::bench::DEFAULT_ITERS
        )]
        iterations: usize,
    },
    /// Run the profiler
    Profile {
        /// Input file
        file: Option<PathBuf>,
    },
    /// Discover and run test_ functions
    Test {
        /// Input file (optional — scans current directory)
        file: Option<PathBuf>,
        /// Filter tests by substring
        #[arg(long = "filter")]
        filter: Option<String>,
        /// Disable colored output
        #[arg(long = "no-color")]
        no_color: bool,
    },
    /// Show detailed explanation for an error code
    Explain {
        /// Error code (e.g. E0100)
        code: Option<String>,
    },
    /// Self-upgrade the IRIS compiler
    Upgrade {
        /// Check for available updates without installing
        #[arg(short = 'c', long = "check")]
        check: bool,
        /// Skip confirmation prompts
        #[arg(short = 'y', long = "yes")]
        yes: bool,
        /// Force reinstall even if up-to-date
        #[arg(short = 'f', long = "force")]
        force: bool,
    },
    /// Install a package or all dependencies from iris.toml
    Install {
        /// Git URL of the package to install (omit to install all from iris.toml)
        url: Option<String>,
    },
    /// Download and configure toolchain dependencies
    Setup,
    /// Generate HTML documentation from doc comments
    Docs {
        /// Input file
        file: Option<PathBuf>,
        /// Write output to <file> instead of stdout
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
    },
    /// Find errors without building: every diagnostic, in every file given
    /// (a directory checks each .iris file under it; default: this directory)
    Check {
        /// Files or directories
        paths: Vec<PathBuf>,
    },
    /// Run, test or check again on every save
    Watch {
        /// The program (or directory) to watch
        file: PathBuf,
        /// Run `iris test` instead of `iris run`
        #[arg(long)]
        test: bool,
        /// Run `iris check` instead of `iris run`
        #[arg(long)]
        check: bool,
    },
    /// Check the toolchain IRIS needs, end to end, and say how to fix what is missing
    Doctor,
    /// Create a project from a template: app, cli, server, ros2, controller
    New {
        /// Template (app, cli, server, ros2, controller); default app
        #[arg(default_value = "app")]
        template: String,
        /// Project name: the directory to create
        name: Option<String>,
    },
    /// Collect what is needed to report a compiler bug (nothing is sent)
    #[command(name = "bug-report")]
    BugReport {
        /// The program that shows the bug
        file: PathBuf,
        /// Where to write the report (default: iris-bug-report-<time>)
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// Also run the program, natively and in the interpreter
        #[arg(long)]
        run: bool,
    },
    /// Format IRIS source files
    Fmt {
        /// Input file (optional — formats all *.iris in current directory)
        file: Option<PathBuf>,
        /// Check if formatting is needed without modifying files (exit 1 if changes needed)
        #[arg(short = 'c', long = "check")]
        check: bool,
    },
}

// ---------------------------------------------------------------------------
// Public API — backward-compatible with old parse_args signature
// ---------------------------------------------------------------------------

/// Fully-parsed CLI arguments for a compilation request.
#[derive(Debug)]
pub struct CliArgs {
    pub path: PathBuf,
    pub emit: EmitKind,
    pub output: Option<PathBuf>,
    pub run_after_build: bool,
    pub target: Option<String>,
    pub dump_ir_after: Option<String>,
    pub max_steps: usize,
    pub max_depth: usize,
    pub no_cache: bool,
    pub sandbox: bool,
    pub strict_effects: bool,
    pub simd: bool,
    pub pgo_generate: bool,
    pub pgo_use: Option<PathBuf>,
    pub weights: Option<PathBuf>,
    pub gpu: Option<String>,
    pub error_format: ErrorFormatCli,
    pub debug: bool,
    /// Arguments passed to the program itself (after `--`).
    pub program_args: Vec<String>,
}

/// Hardening options for evolution audit checkpoints (`--audit-head`).
#[derive(Args, Debug, Clone, Default)]
pub struct AuditTrustArgs {
    /// Shared secret for HMAC-SHA256 authenticated audit checkpoints
    /// (`iris-evolution-audit-head/2`); or set `IRIS_AUDIT_HMAC_KEY`
    #[arg(long = "audit-hmac-key")]
    pub audit_hmac_key: Option<String>,
    /// File containing the HMAC-SHA256 secret key; or set
    /// `IRIS_AUDIT_HMAC_KEY_FILE`
    #[arg(long = "audit-hmac-key-file")]
    pub audit_hmac_key_file: Option<PathBuf>,
    /// Secondary checkpoint file kept atomically in sync (repeatable); or set
    /// `IRIS_AUDIT_HEAD_MIRROR`
    #[arg(long = "audit-head-mirror")]
    pub audit_head_mirror: Vec<PathBuf>,
    /// Fail closed if `--audit-head` (or any mirror) resides in the same
    /// directory as `--audit`; or set `IRIS_AUDIT_REQUIRE_SEPARATE_DOMAIN=1`
    #[arg(long = "require-separate-audit-domain")]
    pub require_separate_audit_domain: bool,
}

/// Result of `parse_args` — backward-compatible with the old API.
/// `iris evolve-live`: a plant and controllers in IRIS, a loop on the host.
#[derive(Args, Debug, Clone)]
pub struct EvolveLiveArgs {
    /// The plant: `def plant(s: PlantInput) -> PlantState`, PlantInput with
    /// f64 fields position, velocity, force, dt and PlantState position, velocity
    #[arg(long)]
    pub plant: PathBuf,
    /// The controller installed first: `def policy(error: f64, velocity: f64) -> f64`
    #[arg(long)]
    pub baseline: PathBuf,
    /// A candidate controller, offered while the loop runs (repeatable, in order)
    #[arg(long = "candidate")]
    pub candidates: Vec<PathBuf>,
    /// Canary cases: `[{ "inputs": [error, velocity], "expected": force }]`
    #[arg(long)]
    pub cases: PathBuf,
    /// Trusted constitution text
    #[arg(long)]
    pub constitution: PathBuf,
    /// Expected lowercase SHA-256 of the constitution
    #[arg(long = "constitution-sha256")]
    pub constitution_sha256: String,
    /// Append-only JSONL decision log
    #[arg(long)]
    pub audit: PathBuf,
    /// Independently protected audit-head checkpoint
    #[arg(long = "audit-head")]
    pub audit_head: PathBuf,
    /// The actuator rating's lower bound (constitution)
    #[arg(long = "min-output", allow_hyphen_values = true)]
    pub min_output: f64,
    /// The actuator rating's upper bound (constitution)
    #[arg(long = "max-output", allow_hyphen_values = true)]
    pub max_output: f64,
    /// Comma-separated setpoints, each held for --hold seconds
    #[arg(
        long,
        value_delimiter = ',',
        allow_hyphen_values = true,
        default_value = "1.0"
    )]
    pub setpoints: Vec<f64>,
    /// Simulated seconds each setpoint is held
    #[arg(long, default_value_t = 8.0)]
    pub hold: f64,
    /// Control period, seconds
    #[arg(long, default_value_t = 0.02)]
    pub dt: f64,
    /// Simulated seconds between candidate offers
    #[arg(long = "offer-every", default_value_t = 4.0)]
    pub offer_every: f64,
    /// Also run the float search beside the loop and offer its winner
    #[arg(long)]
    pub search: bool,
    /// Search generations
    #[arg(long, default_value_t = 20)]
    pub generations: usize,
    /// Search population
    #[arg(long = "pop-size", default_value_t = 60)]
    pub pop_size: usize,
    /// Search seed, for a reproducible run
    #[arg(long)]
    pub seed: Option<u64>,
    /// Require the IR-level allocation-free proof of every promoted controller
    #[arg(long = "require-allocation-free")]
    pub require_allocation_free: bool,
    /// Pace the loop to the wall clock
    #[arg(long)]
    pub realtime: bool,
    /// Maximum in-memory generations per slot (`N >= 1` or `unlimited`;
    /// default 8, or `IRIS_HOT_SWAP_RETENTION`)
    #[arg(long = "max-generations")]
    pub max_generations: Option<String>,
    /// External directory to durably archive generation records; or set
    /// `IRIS_HOT_SWAP_ARCHIVE_DIR`
    #[arg(long = "generation-archive")]
    pub generation_archive: Option<PathBuf>,
    #[command(flatten)]
    pub audit_trust: AuditTrustArgs,
}

/// `iris evolve-serve`: a program's gateways over HTTP, upgradable in place.
#[derive(Args, Debug, Clone)]
pub struct EvolveServeArgs {
    /// Program manifest (`iris-evolution-program/2`): the gateways served
    #[arg(long)]
    pub manifest: PathBuf,
    /// The version served first
    #[arg(long)]
    pub baseline: PathBuf,
    /// Scenarios every upgrade is judged by
    #[arg(long)]
    pub scenarios: PathBuf,
    /// Required test function (repeatable); `test_program` when omitted
    #[arg(long = "test")]
    pub tests: Vec<String>,
    /// Trusted constitution text
    #[arg(long)]
    pub constitution: PathBuf,
    /// Expected lowercase SHA-256 of the constitution
    #[arg(long = "constitution-sha256")]
    pub constitution_sha256: String,
    /// Append-only JSONL decision log
    #[arg(long)]
    pub audit: PathBuf,
    /// Independently protected audit-head checkpoint
    #[arg(long = "audit-head")]
    pub audit_head: PathBuf,
    /// Lowest number allowed in any scenario result (constitution)
    #[arg(long = "min-output", allow_hyphen_values = true)]
    pub min_output: Option<f64>,
    /// Highest number allowed in any scenario result (constitution)
    #[arg(long = "max-output", allow_hyphen_values = true)]
    pub max_output: Option<f64>,
    /// Port on 127.0.0.1; 0 picks a free one
    #[arg(long, default_value_t = 8787)]
    pub port: u16,
    /// Write the bound port here once listening
    #[arg(long = "port-file")]
    pub port_file: Option<PathBuf>,
    /// Largest accepted request body, in bytes
    #[arg(long = "max-body", default_value_t = 1 << 20)]
    pub max_body: usize,
    /// Maximum in-memory generations per slot (`N >= 1` or `unlimited`;
    /// default 8, or `IRIS_HOT_SWAP_RETENTION`)
    #[arg(long = "max-generations")]
    pub max_generations: Option<String>,
    /// External directory to durably archive generation records; or set
    /// `IRIS_HOT_SWAP_ARCHIVE_DIR`
    #[arg(long = "generation-archive")]
    pub generation_archive: Option<PathBuf>,
    #[command(flatten)]
    pub audit_trust: AuditTrustArgs,
}

#[derive(Debug)]
pub enum ParseArgsResult {
    Args(CliArgs),
    Help,
    /// `iris <command> --help`: that command's own help.
    HelpFor(String),
    Version,
    Check {
        paths: Vec<PathBuf>,
        json: bool,
    },
    Watch {
        file: PathBuf,
        test: bool,
        check: bool,
    },
    Doctor,
    New {
        template: String,
        name: Option<String>,
    },
    BugReport {
        file: PathBuf,
        output: Option<PathBuf>,
        run: bool,
    },
    Repl,
    Lsp,
    Dap,
    Embedded {
        file: PathBuf,
        target: String,
        output: Option<PathBuf>,
        entry: String,
    },
    Evolve {
        baseline: PathBuf,
        candidate: PathBuf,
        cases: Option<PathBuf>,
        manifest: Option<PathBuf>,
        scenarios: Option<PathBuf>,
        test: Vec<String>,
        constitution: PathBuf,
        constitution_sha256: String,
        audit: PathBuf,
        audit_head: PathBuf,
        min_output: Option<String>,
        max_output: Option<String>,
        abi: String,
        bound: Vec<String>,
        require_allocation_free: bool,
        validate: String,
        audit_trust: AuditTrustArgs,
    },
    EvolveUnrestricted {
        candidate: PathBuf,
        manifest: PathBuf,
        acknowledge_unsafe: String,
    },
    EvolveLive(EvolveLiveArgs),
    EvolveServe(EvolveServeArgs),
    EvolveSearch {
        baseline: Option<PathBuf>,
        cases: PathBuf,
        generations: usize,
        pop_size: usize,
        mutation_rate: f64,
        crossover_rate: f64,
        tournament_size: usize,
        elite_count: usize,
        max_depth: usize,
        parsimony_weight: f64,
        target_loss: Option<f64>,
        seed: Option<u64>,
        out_candidate: Option<PathBuf>,
        promote: bool,
        constitution: Option<PathBuf>,
        constitution_sha256: Option<String>,
        audit: Option<PathBuf>,
        audit_head: Option<PathBuf>,
        min_output: Option<String>,
        max_output: Option<String>,
        abi: String,
        audit_trust: AuditTrustArgs,
    },
    Meta {
        file: PathBuf,
        emit_ir: bool,
    },
    Pkg {
        /// Raw arguments after `pkg` subcommand
        args: Vec<String>,
    },
    Bindgen {
        header: PathBuf,
        output: Option<PathBuf>,
        lib: Option<String>,
        cpp: bool,
        include_dirs: Vec<PathBuf>,
        defines: Vec<String>,
        shim_out: Option<PathBuf>,
        no_clang: bool,
    },
    Bench {
        file: PathBuf,
        iterations: usize,
    },
    Profile,
    Test {
        /// Input file (optional — scans current directory)
        file: Option<PathBuf>,
        /// Filter tests by substring
        filter: Option<String>,
        /// Disable colored output
        no_color: bool,
    },
    Explain(Option<String>),
    Upgrade {
        check: bool,
        yes: bool,
        force: bool,
    },
    Install {
        url: Option<String>,
    },
    Setup,
    Fmt {
        file: Option<PathBuf>,
        check: bool,
    },
    Docs {
        file: Option<PathBuf>,
        output: Option<PathBuf>,
    },
    ServiceDaemon {
        upstream: String,
        listen: u16,
        ticks: usize,
        tick_ms: u64,
        max_inflight: usize,
        target_p99: f64,
        error_budget: f64,
        headless: bool,
        audit: Option<PathBuf>,
        serve: bool,
        port: u16,
    },
}

/// Parses command-line arguments.
///
/// Uses `clap` internally, but returns the same `ParseArgsResult` enum
/// that `main.rs` already matches on.
pub fn parse_args(args: &[String]) -> Result<ParseArgsResult, String> {
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            if e.kind() == clap::error::ErrorKind::DisplayHelp {
                // `iris <command> --help` (or `iris help <command>`) shows that
                // command's options; `iris --help` the overview.
                let named = args.iter().skip(1).any(|a| {
                    !a.starts_with('-')
                        && a != "help"
                        && Cli::command().find_subcommand(a).is_some()
                });
                if named {
                    return Ok(ParseArgsResult::HelpFor(e.render().to_string()));
                }
                return Ok(ParseArgsResult::Help);
            }
            if e.kind() == clap::error::ErrorKind::DisplayVersion {
                return Ok(ParseArgsResult::Version);
            }
            return Err(e.to_string());
        }
    };
    // `#ifdef WINDOWS`, `DEBUG` and `-D` symbols describe this build.
    crate::preprocessor::configure(cli.target.as_deref(), cli.debug, &cli.defines);

    match cli.command {
        Some(Command::Build {
            file,
            output,
            target,
        }) => {
            let path = file
                .or(cli.file)
                .ok_or_else(|| "no input file specified".to_owned())?;
            Ok(ParseArgsResult::Args(CliArgs {
                emit: EmitKind::Binary,
                path,
                output: output.or(cli.output),
                run_after_build: false,
                target: target.or(cli.target),
                dump_ir_after: cli.dump_ir_after,
                max_steps: cli.max_steps,
                max_depth: cli.max_depth,
                no_cache: cli.no_cache,
                sandbox: cli.sandbox,
                strict_effects: cli.strict_effects,
                simd: cli.simd,
                pgo_generate: cli.pgo_generate,
                pgo_use: cli.pgo_use.clone(),
                weights: cli.weights.clone(),
                gpu: cli.gpu.clone(),
                error_format: cli.error_format,
                debug: cli.debug,
                program_args: cli.program_args.clone(),
            }))
        }
        Some(Command::Run {
            file,
            output,
            target,
            program_args,
        }) => {
            let path = file
                .or(cli.file)
                .ok_or_else(|| "no input file specified".to_owned())?;
            let program_args = if program_args.is_empty() {
                cli.program_args
            } else {
                program_args
            };
            Ok(ParseArgsResult::Args(CliArgs {
                emit: EmitKind::Binary,
                path,
                output: output.or(cli.output),
                run_after_build: true,
                target: target.or(cli.target),
                dump_ir_after: cli.dump_ir_after,
                max_steps: cli.max_steps,
                max_depth: cli.max_depth,
                no_cache: cli.no_cache,
                sandbox: cli.sandbox,
                strict_effects: cli.strict_effects,
                simd: cli.simd,
                pgo_generate: cli.pgo_generate,
                pgo_use: cli.pgo_use.clone(),
                weights: cli.weights.clone(),
                gpu: cli.gpu.clone(),
                error_format: cli.error_format,
                debug: cli.debug,
                program_args,
            }))
        }
        Some(Command::Embedded {
            file,
            target,
            output,
            entry,
        }) => Ok(ParseArgsResult::Embedded {
            file,
            target,
            output,
            entry,
        }),
        Some(Command::Evolve {
            baseline,
            candidate,
            cases,
            manifest,
            scenarios,
            test,
            constitution,
            constitution_sha256,
            audit,
            audit_head,
            min_output,
            max_output,
            abi,
            bound,
            require_allocation_free,
            validate,
            audit_trust,
        }) => Ok(ParseArgsResult::Evolve {
            baseline,
            candidate,
            cases,
            manifest,
            scenarios,
            test,
            constitution,
            constitution_sha256,
            audit,
            audit_head,
            min_output,
            max_output,
            abi,
            bound,
            require_allocation_free,
            validate,
            audit_trust,
        }),
        Some(Command::EvolveUnrestricted {
            candidate,
            manifest,
            acknowledge_unsafe,
        }) => Ok(ParseArgsResult::EvolveUnrestricted {
            candidate,
            manifest,
            acknowledge_unsafe,
        }),
        Some(Command::EvolveSearch {
            baseline,
            cases,
            generations,
            pop_size,
            mutation_rate,
            crossover_rate,
            tournament_size,
            elite_count,
            max_depth,
            parsimony_weight,
            target_loss,
            seed,
            out_candidate,
            promote,
            constitution,
            constitution_sha256,
            audit,
            audit_head,
            min_output,
            max_output,
            abi,
            audit_trust,
        }) => Ok(ParseArgsResult::EvolveSearch {
            baseline,
            cases,
            generations,
            pop_size,
            mutation_rate,
            crossover_rate,
            tournament_size,
            elite_count,
            max_depth,
            parsimony_weight,
            target_loss,
            seed,
            out_candidate,
            promote,
            constitution,
            constitution_sha256,
            audit,
            audit_head,
            min_output,
            max_output,
            abi,
            audit_trust,
        }),
        Some(Command::EvolveLive(args)) => Ok(ParseArgsResult::EvolveLive(args)),
        Some(Command::EvolveServe(args)) => Ok(ParseArgsResult::EvolveServe(args)),
        Some(Command::Meta { file, emit_ir }) => Ok(ParseArgsResult::Meta { file, emit_ir }),
        Some(Command::Repl) => Ok(ParseArgsResult::Repl),
        Some(Command::Lsp { .. }) => Ok(ParseArgsResult::Lsp),
        Some(Command::Dap { .. }) => Ok(ParseArgsResult::Dap),
        Some(Command::Pkg { args }) => Ok(ParseArgsResult::Pkg { args }),
        Some(Command::Bindgen {
            header,
            output,
            lib,
            cpp,
            include_dirs,
            defines,
            shim_out,
            no_clang,
        }) => Ok(ParseArgsResult::Bindgen {
            header,
            output,
            lib,
            cpp,
            include_dirs,
            defines,
            shim_out,
            no_clang,
        }),
        Some(Command::Bench { file, iterations }) => {
            Ok(ParseArgsResult::Bench { file, iterations })
        }
        Some(Command::Profile { file }) => {
            if let Some(path) = file {
                Ok(ParseArgsResult::Args(CliArgs {
                    emit: EmitKind::Eval,
                    path,
                    output: cli.output,
                    run_after_build: false,
                    target: cli.target,
                    dump_ir_after: cli.dump_ir_after,
                    max_steps: cli.max_steps,
                    max_depth: cli.max_depth,
                    no_cache: cli.no_cache,
                    sandbox: cli.sandbox,
                    strict_effects: cli.strict_effects,
                    simd: cli.simd,
                    pgo_generate: cli.pgo_generate,
                    pgo_use: cli.pgo_use.clone(),
                    weights: cli.weights.clone(),
                    gpu: cli.gpu.clone(),
                    error_format: cli.error_format,
                    debug: cli.debug,
                    program_args: cli.program_args.clone(),
                }))
            } else {
                Ok(ParseArgsResult::Profile)
            }
        }
        Some(Command::Test {
            file,
            filter,
            no_color,
        }) => Ok(ParseArgsResult::Test {
            file,
            filter,
            no_color,
        }),
        Some(Command::Explain { code }) => Ok(ParseArgsResult::Explain(code)),
        Some(Command::Upgrade { check, yes, force }) => {
            Ok(ParseArgsResult::Upgrade { check, yes, force })
        }
        Some(Command::Setup) => Ok(ParseArgsResult::Setup),
        Some(Command::Check { paths }) => Ok(ParseArgsResult::Check {
            paths: if paths.is_empty() {
                cli.file.into_iter().collect()
            } else {
                paths
            },
            json: matches!(cli.error_format, ErrorFormatCli::Json),
        }),
        Some(Command::Watch { file, test, check }) => {
            Ok(ParseArgsResult::Watch { file, test, check })
        }
        Some(Command::Doctor) => Ok(ParseArgsResult::Doctor),
        Some(Command::New { template, name }) => Ok(ParseArgsResult::New { template, name }),
        Some(Command::BugReport { file, output, run }) => {
            Ok(ParseArgsResult::BugReport { file, output, run })
        }
        Some(Command::Install { url }) => Ok(ParseArgsResult::Install { url }),
        Some(Command::Fmt { file, check }) => Ok(ParseArgsResult::Fmt { file, check }),
        Some(Command::Docs { file, output }) => Ok(ParseArgsResult::Docs { file, output }),
        Some(Command::ServiceDaemon {
            upstream,
            listen,
            ticks,
            tick_ms,
            max_inflight,
            target_p99,
            error_budget,
            headless,
            audit,
            serve,
            port,
        }) => Ok(ParseArgsResult::ServiceDaemon {
            upstream,
            listen,
            ticks,
            tick_ms,
            max_inflight,
            target_p99,
            error_budget,
            headless,
            audit,
            serve,
            port,
        }),
        None => {
            // No subcommand — treat as direct compilation request
            let path = cli
                .file
                .ok_or_else(|| "no input file specified".to_owned())?;
            Ok(ParseArgsResult::Args(CliArgs {
                path,
                emit: cli.emit.into(),
                output: cli.output,
                run_after_build: false,
                target: cli.target,
                dump_ir_after: cli.dump_ir_after,
                max_steps: cli.max_steps,
                max_depth: cli.max_depth,
                no_cache: cli.no_cache,
                sandbox: cli.sandbox,
                strict_effects: cli.strict_effects,
                simd: cli.simd,
                pgo_generate: cli.pgo_generate,
                pgo_use: cli.pgo_use.clone(),
                weights: cli.weights.clone(),
                gpu: cli.gpu.clone(),
                error_format: cli.error_format,
                debug: cli.debug,
                program_args: cli.program_args.clone(),
            }))
        }
    }
}

/// Returns the version string for the CLI (GCC-style verbose output).
pub fn version_text() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let build_date = option_env!("IRIS_BUILD_DATE").unwrap_or("unknown");
    let target = option_env!("IRIS_TARGET").unwrap_or("unknown");
    let host = option_env!("IRIS_HOST").unwrap_or("unknown");
    let profile = option_env!("IRIS_PROFILE").unwrap_or("unknown");
    let opt_level = option_env!("IRIS_OPT_LEVEL").unwrap_or("unknown");
    let git_hash = option_env!("IRIS_GIT_HASH").unwrap_or("unknown");
    let git_hash_short = option_env!("IRIS_GIT_HASH_SHORT").unwrap_or("unknown");
    let git_branch = option_env!("IRIS_GIT_BRANCH").unwrap_or("unknown");
    let git_dirty = option_env!("IRIS_GIT_DIRTY").unwrap_or("false");
    let rustc_ver = option_env!("IRIS_RUSTC_VERSION").unwrap_or("unknown");

    // Detect thread model.
    let thread_model = if cfg!(target_family = "windows") {
        "win32"
    } else {
        "posix"
    };

    let dirty_flag = if git_dirty == "true" {
        " (modified)"
    } else {
        ""
    };

    format!(
        "iris {version} ({git_hash_short} {build_date}){dirty}\n\
         IRIS — Intermediate Representation for Intelligent Systems\n\
         Copyright (C) 2024-2026 Moon & IRIS Project Contributors\n\
         License: GPL-2.0-or-later <https://www.gnu.org/licenses/old-licenses/gpl-2.0.html>\n\
         This is free software; you can redistribute it and/or modify it under\n\
         the terms of the GNU General Public License v2 (or later).\n\
         There is NO WARRANTY, to the extent permitted by law.\n\
         \n\
         Compiler:\n\
           Version:       {version}\n\
           Git commit:    {git_hash}\n\
           Git branch:    {git_branch}\n\
           Build date:    {build_date}\n\
         \n\
         Platform:\n\
           Target:        {target}\n\
           Host:          {host}\n\
           Thread model:  {thread_model}\n\
         \n\
         Build:\n\
           Profile:       {profile}\n\
           Opt level:     {opt_level}\n\
           Rust edition:  2021\n\
           Built with:    {rustc_ver}\n",
        version = version,
        git_hash_short = git_hash_short,
        git_hash = git_hash,
        git_branch = git_branch,
        build_date = build_date,
        dirty = dirty_flag,
        target = target,
        host = host,
        profile = profile,
        opt_level = opt_level,
        thread_model = thread_model,
        rustc_ver = rustc_ver,
    )
}

/// The overview `iris --help` prints, generated from the command definitions
/// above so it cannot fall out of date (it used to be a hand-written copy that
/// lacked `evolve-live` and `evolve-serve`).
pub fn help_text() -> String {
    let mut cmd = Cli::command()
        .about("IRIS compiler: build, run, test and check IRIS programs")
        .after_help(
            "Examples:\n  \
             iris new app hello         start a project (templates: app, cli, server, ros2, controller)\n  \
             iris run main.iris         build and run\n  \
             iris test                  run the test_ functions in this directory\n  \
             iris check                 find errors without building\n  \
             iris watch main.iris       run again on every save\n  \
             iris doctor                check the toolchain\n  \
             iris explain E0109         what an error code means\n\n\
             `iris <command> --help` shows a command's options.",
        );
    cmd.render_help().to_string()
}
