//! CLI argument parsing using clap derive.
//!
//! Exports `parse_args` (backward-compatible with the old signature),
//! `version_text`, `help_text`, and the `CliArgs` / `ParseArgsResult` types
//! used by `main.rs`.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

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
    #[arg(long = "error-format", value_enum, default_value_t = ErrorFormatCli::Human)]
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
    #[arg(long = "no-cache")]
    pub no_cache: bool,

    /// Run with sandboxed security (deny fs/network/ffi/process)
    #[arg(long = "sandbox")]
    pub sandbox: bool,

    /// Require every effectful function to declare an `effect` clause that
    /// covers what it does; a violation fails the build
    #[arg(long = "strict-effects")]
    pub strict_effects: bool,

    /// Input file
    pub file: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Build a native binary (same as --emit binary)
    Build {
        /// Input file
        file: Option<PathBuf>,
    },
    /// Build and run the binary
    Run {
        /// Input file
        file: Option<PathBuf>,
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
    /// Evaluate and hot-swap an evolved `(i64) -> i64` policy through seven gates
    Evolve {
        /// Trusted baseline IRIS source
        #[arg(long)]
        baseline: PathBuf,
        /// Candidate IRIS source to validate and promote
        #[arg(long)]
        candidate: PathBuf,
        /// JSON array of `{ "input": i64, "expected": i64 }` canary cases
        #[arg(long)]
        cases: PathBuf,
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
        /// Lowest constitution-approved output
        #[arg(long = "min-output", default_value_t = i64::MIN)]
        min_output: i64,
        /// Highest constitution-approved output
        #[arg(long = "max-output", default_value_t = i64::MAX)]
        max_output: i64,
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
    /// Search and evolve an optimal policy genome using genetic programming
    #[command(name = "evolve-search")]
    EvolveSearch {
        /// Optional trusted baseline IRIS source to seed population
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// JSON array of `{ "input": i64, "expected": i64 }` canary cases
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
        /// Lowest constitution-approved output
        #[arg(long = "min-output", default_value_t = i64::MIN)]
        min_output: i64,
        /// Highest constitution-approved output
        #[arg(long = "max-output", default_value_t = i64::MAX)]
        max_output: i64,
    },
    /// Run the production autonomic microservice daemon or living organism simulation
    #[command(name = "service-daemon")]
    ServiceDaemon {
        /// Number of autonomic simulation ticks (default: 60)
        #[arg(long, default_value_t = 60)]
        ticks: usize,
        /// Target P99 SLA threshold in milliseconds
        #[arg(long = "target-p99", default_value_t = 15.0)]
        target_p99: f64,
        /// Maximum allowed error budget ratio
        #[arg(long = "error-budget", default_value_t = 0.01)]
        error_budget: f64,
        /// Run in headless mode without interactive terminal UI
        #[arg(long)]
        headless: bool,
        /// Optional path to export JSON audit ledger
        #[arg(long)]
        audit: Option<PathBuf>,
        /// Enable embedded HTTP live dashboard and Prometheus metrics server
        #[arg(long)]
        serve: bool,
        /// HTTP server port (default: 9090)
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
    pub error_format: ErrorFormatCli,
}

/// Result of `parse_args` — backward-compatible with the old API.
#[derive(Debug)]
pub enum ParseArgsResult {
    Args(CliArgs),
    Help,
    Version,
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
        cases: PathBuf,
        constitution: PathBuf,
        constitution_sha256: String,
        audit: PathBuf,
        audit_head: PathBuf,
        min_output: i64,
        max_output: i64,
    },
    EvolveUnrestricted {
        candidate: PathBuf,
        manifest: PathBuf,
        acknowledge_unsafe: String,
    },
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
        min_output: i64,
        max_output: i64,
    },
    Meta {
        file: PathBuf,
        emit_ir: bool,
    },
    Pkg {
        /// Raw arguments after `pkg` subcommand
        args: Vec<String>,
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
        ticks: usize,
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
                return Ok(ParseArgsResult::Help);
            }
            if e.kind() == clap::error::ErrorKind::DisplayVersion {
                return Ok(ParseArgsResult::Version);
            }
            return Err(e.to_string());
        }
    };

    match cli.command {
        Some(Command::Build { file }) => {
            let path = file
                .or(cli.file)
                .ok_or_else(|| "no input file specified".to_owned())?;
            Ok(ParseArgsResult::Args(CliArgs {
                emit: EmitKind::Binary,
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
                error_format: cli.error_format,
            }))
        }
        Some(Command::Run { file }) => {
            let path = file
                .or(cli.file)
                .ok_or_else(|| "no input file specified".to_owned())?;
            Ok(ParseArgsResult::Args(CliArgs {
                emit: EmitKind::Binary,
                path,
                output: cli.output,
                run_after_build: true,
                target: cli.target,
                dump_ir_after: cli.dump_ir_after,
                max_steps: cli.max_steps,
                max_depth: cli.max_depth,
                no_cache: cli.no_cache,
                sandbox: cli.sandbox,
                strict_effects: cli.strict_effects,
                error_format: cli.error_format,
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
            constitution,
            constitution_sha256,
            audit,
            audit_head,
            min_output,
            max_output,
        }) => Ok(ParseArgsResult::Evolve {
            baseline,
            candidate,
            cases,
            constitution,
            constitution_sha256,
            audit,
            audit_head,
            min_output,
            max_output,
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
        }),
        Some(Command::Meta { file, emit_ir }) => Ok(ParseArgsResult::Meta { file, emit_ir }),
        Some(Command::Repl) => Ok(ParseArgsResult::Repl),
        Some(Command::Lsp { .. }) => Ok(ParseArgsResult::Lsp),
        Some(Command::Dap { .. }) => Ok(ParseArgsResult::Dap),
        Some(Command::Pkg { args }) => Ok(ParseArgsResult::Pkg { args }),
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
                    error_format: cli.error_format,
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
        Some(Command::Install { url }) => Ok(ParseArgsResult::Install { url }),
        Some(Command::Fmt { file, check }) => Ok(ParseArgsResult::Fmt { file, check }),
        Some(Command::Docs { file, output }) => Ok(ParseArgsResult::Docs { file, output }),
        Some(Command::ServiceDaemon {
            ticks,
            target_p99,
            error_budget,
            headless,
            audit,
            serve,
            port,
        }) => Ok(ParseArgsResult::ServiceDaemon {
            ticks,
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
                error_format: cli.error_format,
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

/// Returns the usage/help text for the CLI.
pub fn help_text() -> &'static str {
    "IRIS compiler\n\
     Usage: iris [subcommand] [options] <file.iris>\n\
     \n\
     Subcommands:\n\
       build                 Build native binary (same as --emit binary)\n\
       run                   Build and run the binary\n\
       embedded             Build an allocation-free bare-metal component bundle\n\
        evolve               Validate and hot-swap a governed `(i64) -> i64` policy\n\
        test [file.iris]      Discover and run test_ functions (--filter <substr> --no-color)\n\
       install [url]         Install dependencies or a package from a Git URL\n\
       fmt [file.iris]       Format source files (--check to verify without modifying)\n\
       repl                  Start an interactive REPL session\n\
       lsp                   Start the LSP server (JSON-RPC on stdin/stdout)\n\
       dap                   Start the DAP debug adapter (JSON-RPC on stdin/stdout)\n\
       pkg <cmd>             Package manager (init, add, remove, install, list, build, run)\n\
       bench <file.iris>     Run performance benchmarks on a file\n\
       explain [code]        Show detailed explanation for an error code (e.g. E0100)\n\
        upgrade               Self-upgrade the IRIS compiler to the latest version\n\
        setup                 Download and configure toolchain dependencies\n\
        docs [file.iris]      Generate HTML documentation from doc comments\n\
     \n\
     Options:\n\
     --emit <kind>         Output kind: ir (default), llvm, llvm-complete, cuda, cuda-ptx, simd,\n\
                              jit, pgo-instrument, pgo-optimize, graph, onnx, onnx-binary,\n\
                              eval, binary, tensorrt\n\
       -o <file>             Write output to <file> instead of stdout\n\
       --target <triple>     Target preset/triple for llvm/binary outputs (e.g. linux-arm64)\n\
       --dump-ir-after <p>   Dump IR to stderr after pass <p> completes\n\
       --max-steps <n>       Legacy interpreter guardrail (ignored for native build/run/eval/jit)\n\
       --max-depth <n>       Legacy call-depth guardrail (ignored for native build/run/eval/jit)\n\
       --no-cache            Disable incremental compilation cache\n\
       --sandbox             Run with sandboxed security (deny fs/network/ffi/process)\n\
       --strict-effects      Require `effect` clauses that cover what each function\n\
                             does; an effect violation fails the build\n\
       --help, -h            Print this help and exit\n\
       --version, -V         Print version and exit\n"
}
