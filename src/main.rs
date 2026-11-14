#![allow(clippy::collapsible_match)]
#![allow(clippy::useless_conversion)]

use std::path::PathBuf;
use std::process;

use iris::cli::{parse_args, ParseArgsResult};
use iris::diagnostics::{render_error, render_error_colored, render_error_colored_with_file};

/// Returns `true` if stderr is connected to a terminal (for colored output).
fn is_stderr_tty() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let handle = std::io::stderr().as_raw_handle();
        // Enable virtual terminal processing on Windows 10+
        unsafe {
            let mut mode: u32 = 0;
            if winapi_GetConsoleMode(handle, &mut mode) != 0 {
                let _ = winapi_SetConsoleMode(handle, mode | 0x0004);
                return true;
            }
        }
        false
    }
    #[cfg(not(windows))]
    {
        unsafe { libc_isatty(2) != 0 }
    }
}

#[cfg(windows)]
extern "system" {
    fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
}

#[cfg(windows)]
use GetConsoleMode as winapi_GetConsoleMode;
#[cfg(windows)]
use SetConsoleMode as winapi_SetConsoleMode;

#[cfg(not(windows))]
extern "C" {
    fn isatty(fd: i32) -> i32;
}
#[cfg(not(windows))]
use isatty as libc_isatty;

/// 64 MB stack — Windows default is only 1 MB, which overflows on deeply
/// nested IRIS expressions during recursive IR lowering.
const STACK_SIZE: usize = 64 * 1024 * 1024;

fn resolved_target(target: Option<&str>) -> String {
    match target {
        Some(t) => iris::codegen::target_preset_to_triple(t).unwrap_or(t),
        None => iris::codegen::native_target_triple(),
    }
    .to_owned()
}

fn print_compiler_error(
    source: &str,
    err: &iris::error::Error,
    file_path: Option<&str>,
    format: iris::cli::ErrorFormatCli,
) {
    match format {
        iris::cli::ErrorFormatCli::Json => {
            for diag in iris::diagnostics::error_to_diagnostics(err) {
                eprintln!("{}", diag.render_json_in(source, file_path));
            }
        }
        iris::cli::ErrorFormatCli::Human => {
            if is_stderr_tty() {
                if let Some(path) = file_path {
                    eprint!("{}", render_error_colored_with_file(source, err, path));
                } else {
                    eprint!("{}", render_error_colored(source, err));
                }
            } else if let Some(path) = file_path {
                eprint!(
                    "{}",
                    iris::diagnostics::render_error_with_file(source, err, path)
                );
            } else {
                eprint!("{}", render_error(source, err));
            }
        }
    }
}

fn is_native_run_target(target: Option<&str>) -> bool {
    resolved_target(target) == iris::codegen::native_target_triple()
}

fn main() {
    let builder = std::thread::Builder::new().stack_size(STACK_SIZE);
    let handler = builder
        .spawn(run)
        .expect("failed to spawn main thread with enlarged stack");
    if let Err(e) = handler.join() {
        if let Some(msg) = e.downcast_ref::<&str>() {
            eprintln!("error: {}", msg);
        } else if let Some(msg) = e.downcast_ref::<String>() {
            eprintln!("error: {}", msg);
        } else {
            eprintln!("error: thread panicked");
        }
        process::exit(1);
    }
}

fn run() {
    let args: Vec<String> = std::env::args().collect();

    match parse_args(&args) {
        Ok(ParseArgsResult::Help) => {
            print!("{}", iris::cli::help_text());
            process::exit(0);
        }
        Ok(ParseArgsResult::HelpFor(text)) => {
            print!("{text}");
            process::exit(0);
        }
        Ok(ParseArgsResult::Check { paths, json }) => {
            process::exit(iris::devtools::check(&paths, json, is_stderr_tty()));
        }
        Ok(ParseArgsResult::Watch { file, test, check }) => {
            let mode = if test {
                iris::devtools::WatchMode::Test
            } else if check {
                iris::devtools::WatchMode::Check
            } else {
                iris::devtools::WatchMode::Run
            };
            process::exit(iris::devtools::watch(&file, mode));
        }
        Ok(ParseArgsResult::Doctor) => {
            process::exit(iris::devtools::doctor(
                is_stderr_tty() && std::io::IsTerminal::is_terminal(&std::io::stdout()),
            ));
        }
        Ok(ParseArgsResult::New { template, name }) => {
            // `iris new hello` means the default template.
            let (template, name) = match name {
                Some(name) => (template, name),
                None if iris::devtools::TEMPLATES
                    .iter()
                    .any(|(t, _)| *t == template) =>
                {
                    eprintln!("usage: iris new [TEMPLATE] NAME   (templates: app, cli, server, ros2, controller)");
                    process::exit(2);
                }
                None => ("app".to_string(), template),
            };
            process::exit(iris::devtools::new_project(&template, &name));
        }
        Ok(ParseArgsResult::BugReport { file, output, run }) => {
            process::exit(iris::devtools::bug_report(&file, output.as_deref(), run));
        }
        Ok(ParseArgsResult::Version) => {
            print!("{}", iris::cli::version_text());
            process::exit(0);
        }
        Ok(ParseArgsResult::Repl) => {
            run_repl();
        }
        Ok(ParseArgsResult::Lsp) => {
            if let Err(e) = iris::lsp::run_lsp_server() {
                eprintln!("LSP server error: {}", e);
                process::exit(1);
            }
        }
        Ok(ParseArgsResult::Dap) => {
            if let Err(e) = iris::dap::run_dap_server() {
                eprintln!("DAP server error: {}", e);
                process::exit(1);
            }
        }
        Ok(ParseArgsResult::Embedded {
            file,
            target,
            output,
            entry,
        }) => {
            iris::set_strict_effects(true);
            let embedded_target = match iris::codegen::embedded::EmbeddedTarget::parse(&target) {
                Some(target) => target,
                None => {
                    eprintln!(
                        "error: unsupported embedded target '{}'; use arduino-uno, cortex-m4f, cortex-m33, esp32-c3, or esp32",
                        target
                    );
                    process::exit(1);
                }
            };
            let module = match iris::compile_file_to_module(&file) {
                Ok(module) => module,
                Err(error) => {
                    let source = std::fs::read_to_string(&file).unwrap_or_default();
                    eprint!("{}", render_error(&source, &error));
                    process::exit(1);
                }
            };
            let output = output.unwrap_or_else(|| {
                let stem = file
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("iris");
                file.parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join(format!("{}-{}-embedded", stem, embedded_target.preset()))
            });
            match iris::codegen::embedded::build_embedded_bundle(
                &module,
                &output,
                embedded_target,
                &entry,
            ) {
                Ok(bundle) => {
                    eprintln!(
                        "wrote allocation-free embedded bundle: {}",
                        output.display()
                    );
                    eprintln!(
                        "target: {} ({})",
                        bundle.proof.target.preset(),
                        bundle.proof.target.triple()
                    );
                    eprintln!("proof fingerprint: {}", bundle.proof.fingerprint);
                    eprintln!(
                        "fixed-array footprint: {} bytes",
                        bundle.proof.fixed_array_bytes
                    );
                    if let Some(board_support) = &bundle.board_support {
                        eprintln!("board support: {}", board_support.display());
                    }
                    eprintln!("hardware status: not validated until a matching IRIS-HW/1 board report is captured");
                }
                Err(error) => {
                    eprintln!("error: embedded build failed: {}", error);
                    process::exit(1);
                }
            }
        }
        Ok(ParseArgsResult::Evolve {
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
        }) => {
            if let Some(manifest) = manifest {
                let Some(scenarios) = scenarios else {
                    eprintln!("error: --manifest needs --scenarios");
                    process::exit(1);
                };
                if cases.is_some() {
                    eprintln!("error: --cases is for policies; a program is judged by --scenarios");
                    process::exit(1);
                }
                promote_program(ProgramPromotion {
                    baseline: &baseline,
                    candidate: &candidate,
                    manifest: &manifest,
                    scenarios: &scenarios,
                    tests: test,
                    constitution: &constitution,
                    constitution_sha256,
                    audit: &audit,
                    audit_head: &audit_head,
                    audit_trust: &audit_trust,
                    min_output: min_output.as_deref(),
                    max_output: max_output.as_deref(),
                });
                return;
            }
            let Some(cases) = cases else {
                eprintln!(
                    "error: --cases is required (or --manifest with --scenarios for a program)"
                );
                process::exit(1);
            };
            let policy_abi = match parse_policy_abi(&abi) {
                Ok(abi) => abi,
                Err(error) => {
                    eprintln!("error: --abi: {error}");
                    process::exit(1);
                }
            };
            let validation = match validate.as_str() {
                "interpreter" => iris::evolution::ValidationBackend::Interpreter,
                "sandbox" => iris::evolution::ValidationBackend::NativeSandbox {
                    limits: iris::sandbox::ResourceLimits::default(),
                },
                other => {
                    eprintln!(
                        "error: --validate must be 'interpreter' or 'sandbox', found '{other}'"
                    );
                    process::exit(1);
                }
            };
            let bounds = match OutputBounds::parse(
                policy_abi,
                min_output.as_deref(),
                max_output.as_deref(),
                &bound,
            ) {
                Ok(bounds) => bounds,
                Err(error) => {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            };
            let candidate_source = match std::fs::read_to_string(&candidate) {
                Ok(source) => source,
                Err(error) => {
                    eprintln!("error: cannot read '{}': {error}", candidate.display());
                    process::exit(1);
                }
            };
            let candidate_module = match iris::evolution::compile_candidate_file(&candidate) {
                Ok(module) => module,
                Err(error) => {
                    eprintln!("error: candidate: {error}");
                    process::exit(1);
                }
            };
            promote_governed(GovernedPromotion {
                baseline: &baseline,
                candidate_source,
                candidate_module,
                cases: &cases,
                constitution: &constitution,
                constitution_sha256,
                audit: &audit,
                audit_head: &audit_head,
                audit_trust: &audit_trust,
                bounds,
                policy: iris::evolution::EvolutionPolicy {
                    abi: policy_abi,
                    require_allocation_free,
                    validation,
                    ..iris::evolution::EvolutionPolicy::default()
                },
            });
        }
        Ok(ParseArgsResult::EvolveUnrestricted {
            candidate,
            manifest,
            acknowledge_unsafe,
        }) => {
            use iris::evolution::unrestricted::{
                acknowledge_unrestricted, UnrestrictedEvolutionAuthority,
            };

            let token = match acknowledge_unrestricted(&acknowledge_unsafe) {
                Ok(token) => token,
                Err(error) => {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            };
            let source = match std::fs::read_to_string(&candidate) {
                Ok(source) => source,
                Err(error) => {
                    eprintln!("error: cannot read '{}': {error}", candidate.display());
                    process::exit(1);
                }
            };
            let manifest: iris::evolution::manifest::ProgramManifest =
                match std::fs::read_to_string(&manifest)
                    .map_err(|error| error.to_string())
                    .and_then(|json| serde_json::from_str(&json).map_err(|error| error.to_string()))
                {
                    Ok(manifest) => manifest,
                    Err(error) => {
                        eprintln!("error: invalid unrestricted manifest: {error}");
                        process::exit(1);
                    }
                };
            let authority = UnrestrictedEvolutionAuthority::new(token);
            match authority.compile_and_activate(&source, "unrestricted_cli", &manifest) {
                Ok(receipt) => println!(
                    "UNSAFE: activated program {} generation {} from {}",
                    receipt.swap.program_name, receipt.swap.generation, receipt.source_sha256
                ),
                Err(error) => {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
        }
        Ok(ParseArgsResult::EvolveSearch {
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
        }) => {
            let policy_abi = match parse_policy_abi(&abi) {
                Ok(abi) => abi,
                Err(error) => {
                    eprintln!("error: --abi: {error}");
                    process::exit(1);
                }
            };
            if policy_abi == iris::evolution::PolicyAbi::Record {
                eprintln!(
                    "error: evolve-search searches i64 and f64:N policies; a record policy \
                     is written by hand and promoted with `iris evolve --abi record`"
                );
                process::exit(1);
            }
            let bounds = match OutputBounds::parse(
                policy_abi,
                min_output.as_deref(),
                max_output.as_deref(),
                &[],
            ) {
                Ok(bounds) => bounds,
                Err(error) => {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            };
            let cases_json = match std::fs::read_to_string(&cases) {
                Ok(json) => json,
                Err(error) => {
                    eprintln!(
                        "error: cannot read canary cases '{}': {error}",
                        cases.display()
                    );
                    process::exit(1);
                }
            };
            let invalid_cases = |error: serde_json::Error| -> ! {
                eprintln!("error: invalid canary cases '{}': {error}", cases.display());
                process::exit(1);
            };

            let candidate_source = match bounds {
                OutputBounds::I64(min_output, max_output) => {
                    let canary_cases: Vec<iris::evolution::CanaryCase> =
                        serde_json::from_str(&cases_json).unwrap_or_else(|e| invalid_cases(e));
                    let fitness_config = iris::evolution::FitnessConfig {
                        parsimony_weight,
                        min_output,
                        max_output,
                        ..Default::default()
                    };
                    let evaluator = iris::evolution::FitnessEvaluator::from_canary_cases(
                        &canary_cases,
                        fitness_config,
                    );
                    let evo_config = iris::evolution::EvolutionConfig {
                        population_size: pop_size,
                        generations,
                        crossover_rate,
                        mutation_rate,
                        tournament_size,
                        elite_count,
                        max_depth,
                        target_loss,
                        seed,
                        gradient_refinement: true,
                    };
                    let search =
                        iris::evolution::EvolutionarySearch::new(evo_config, "input".to_string());

                    println!("== IRIS Evolutionary Search ==");
                    println!(
                        "Generations: {}, Population: {}, Canary Cases: {}",
                        generations,
                        pop_size,
                        canary_cases.len()
                    );

                    let result = search.run(&evaluator, None, |summary| {
                        println!(
                            "Gen {:02}/{:02} | Best Loss: {:.4} (Exact: {}/{}, Nodes: {}, Depth: {}) | Div: {:.0}% | Expr: {}",
                            summary.generation,
                            generations,
                            summary.best_loss,
                            summary.best_exact_matches,
                            summary.total_cases,
                            summary.best_node_count,
                            summary.best_tree_depth,
                            summary.diversity_ratio * 100.0,
                            summary.best_expression
                        );
                    });

                    println!("\n=== Evolution Outcome ===");
                    println!(
                        "Discovered Genome Expression: {}",
                        result.best_genome.to_iris_expr()
                    );
                    println!(
                        "Fitness: Loss = {:.4}, Exact Matches = {}/{}, Converged = {}",
                        result.best_score.loss,
                        result.best_score.exact_matches,
                        result.best_score.total_cases,
                        result.converged
                    );
                    result.best_genome.to_iris_source("policy", "input")
                }
                OutputBounds::Record { .. } => unreachable!("records were refused above"),
                OutputBounds::F64(..) => {
                    let iris::evolution::PolicyAbi::ControlF64 { inputs } = policy_abi else {
                        unreachable!("f64 bounds come from an f64 ABI")
                    };
                    let canary_cases: Vec<iris::evolution::ControlCanaryCase> =
                        serde_json::from_str(&cases_json).unwrap_or_else(|e| invalid_cases(e));
                    let config = iris::evolution::FloatSearchConfig {
                        inputs,
                        population_size: pop_size,
                        generations,
                        crossover_rate,
                        mutation_rate,
                        tournament_size,
                        elite_count,
                        max_depth,
                        parsimony_weight,
                        target_loss,
                        seed,
                        ..iris::evolution::FloatSearchConfig::default()
                    };
                    println!("== IRIS Evolutionary Search (f64:{inputs}) ==");
                    println!(
                        "Generations: {}, Population: {}, Canary Cases: {}",
                        generations,
                        pop_size,
                        canary_cases.len()
                    );
                    let result = match iris::evolution::float_search::search(
                        &config,
                        &canary_cases,
                        |summary| {
                            println!(
                                "Gen {:02}/{:02} | Loss: {:.6} (mean |error| {:.6}, {} nodes) | {}",
                                summary.generation,
                                generations,
                                summary.best.loss,
                                summary.best.mean_absolute_error,
                                summary.best.nodes,
                                summary.expression
                            );
                        },
                    ) {
                        Ok(result) => result,
                        Err(error) => {
                            eprintln!("error: {error}");
                            process::exit(1);
                        }
                    };
                    println!("\n=== Evolution Outcome ===");
                    println!("Discovered control law: {}", result.best.to_iris_expr());
                    println!(
                        "Fitness: mean |error| = {:.6}, {} nodes, {} generations{}",
                        result.score.mean_absolute_error,
                        result.score.nodes,
                        result.generations_run,
                        if result.reached_target {
                            ", target reached"
                        } else {
                            ""
                        }
                    );
                    result.best.to_iris_source("policy", inputs)
                }
            };

            if let Some(out_path) = &out_candidate {
                if let Err(error) = std::fs::write(out_path, &candidate_source) {
                    eprintln!(
                        "error: cannot write candidate to '{}': {error}",
                        out_path.display()
                    );
                    process::exit(1);
                }
                println!("Saved candidate source to '{}'", out_path.display());
            }

            if promote {
                let required = |value: &Option<std::path::PathBuf>, flag: &str| {
                    value.clone().unwrap_or_else(|| {
                        eprintln!("error: {flag} is required when --promote is set");
                        process::exit(1);
                    })
                };
                let baseline = required(&baseline, "--baseline");
                let constitution = required(&constitution, "--constitution");
                let audit = required(&audit, "--audit");
                let audit_head = required(&audit_head, "--audit-head");
                let constitution_sha256 = constitution_sha256.unwrap_or_else(|| {
                    eprintln!("error: --constitution-sha256 is required when --promote is set");
                    process::exit(1);
                });
                // The search proposes; the seven gates decide, exactly as for
                // a hand-written candidate.
                let candidate_module = match iris::evolution::compile_candidate(
                    &candidate_source,
                    "evolved_candidate",
                ) {
                    Ok(module) => module,
                    Err(error) => {
                        eprintln!("error: candidate compile: {error}");
                        process::exit(1);
                    }
                };
                promote_governed(GovernedPromotion {
                    baseline: &baseline,
                    candidate_source,
                    candidate_module,
                    cases: &cases,
                    constitution: &constitution,
                    constitution_sha256,
                    audit: &audit,
                    audit_head: &audit_head,
                    audit_trust: &audit_trust,
                    bounds,
                    policy: iris::evolution::EvolutionPolicy {
                        abi: policy_abi,
                        ..iris::evolution::EvolutionPolicy::default()
                    },
                });
            }
        }
        Ok(ParseArgsResult::EvolveLive(args)) => run_evolve_live(args),
        Ok(ParseArgsResult::EvolveServe(args)) => run_evolve_serve(args),
        Ok(ParseArgsResult::Meta { file, emit_ir }) => {
            let source = match std::fs::read_to_string(&file) {
                Ok(source) => source,
                Err(error) => {
                    eprintln!("error: cannot read '{}': {error}", file.display());
                    process::exit(1);
                }
            };
            let module_name = file
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("meta");
            if emit_ir {
                match iris::meta::emit_ir(&source, module_name) {
                    Ok(ir) => print!("{ir}"),
                    Err(error) => {
                        eprintln!("error: {error}");
                        process::exit(1);
                    }
                }
            } else {
                match iris::meta::analyze_source_json(&source, module_name) {
                    Ok(json) => println!("{json}"),
                    Err(error) => {
                        eprintln!("error: cannot encode meta analysis: {error}");
                        process::exit(1);
                    }
                }
            }
        }
        Ok(ParseArgsResult::Pkg { args: pkg_args }) => {
            if let Err(e) = iris::pkg::run_pkg_command(&pkg_args) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Ok(ParseArgsResult::Bindgen {
            header,
            output,
            lib,
            cpp,
            include_dirs,
            defines,
            shim_out,
            no_clang,
        }) => {
            let opts = iris::bindgen::BindgenOptions {
                link_lib: lib,
                abi: Some("C".to_owned()),
                include_dirs,
                defines,
                cpp_mode: cpp,
                emit_cpp_shim: shim_out.clone(),
                no_clang,
            };
            match iris::bindgen::generate_bindings_bundle_from_file(&header, &opts) {
                Ok(bundle) => {
                    if let Some(shim_path) = shim_out {
                        if let Some(shim_src) = bundle.cpp_shim {
                            if let Err(e) = std::fs::write(&shim_path, shim_src) {
                                eprintln!("error: cannot write '{}': {e}", shim_path.display());
                                process::exit(1);
                            }
                        }
                    }
                    if let Some(out_path) = output {
                        if let Err(e) = std::fs::write(&out_path, bundle.iris) {
                            eprintln!("error: cannot write '{}': {e}", out_path.display());
                            process::exit(1);
                        }
                    } else {
                        print!("{}", bundle.iris);
                    }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    process::exit(1);
                }
            }
        }
        Ok(ParseArgsResult::Bench { file, iterations }) => {
            if let Err(e) = iris::bench::run_bench_command(&file, iterations) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Ok(ParseArgsResult::Test {
            file,
            filter,
            no_color,
        }) => {
            if let Err(e) = iris::test_runner::run_test_command(&args, file, filter, no_color) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Ok(ParseArgsResult::Explain(code)) => {
            if let Some(c) = code {
                if let Some(explanation) = iris::explain::explain(&c) {
                    println!("{}", explanation);
                } else {
                    eprintln!("error: no explanation found for error code '{}'", c);
                    process::exit(1);
                }
            } else {
                eprintln!("error: explain requires an error code (e.g. `iris explain E0100`)");
                process::exit(1);
            }
        }
        Ok(ParseArgsResult::Profile) => {
            if let Err(e) = iris::profiler::run_profile_command(&args) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Ok(ParseArgsResult::Upgrade { check, yes, force }) => {
            if let Err(e) = iris::upgrade::run_upgrade_command(check, yes, force) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Ok(ParseArgsResult::Install { url }) => {
            if let Err(e) = iris::package_manager::run_install(&url.into_iter().collect::<Vec<_>>())
            {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Ok(ParseArgsResult::Setup) => {
            if let Err(e) = iris::setup::run_setup_command() {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Ok(ParseArgsResult::Docs { file, output }) => {
            let path = match file {
                Some(p) => p,
                None => {
                    eprintln!("error: docs requires an input file (e.g. `iris docs file.iris`)");
                    process::exit(1);
                }
            };
            let source = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: cannot read '{}': {}", path.display(), e);
                    process::exit(1);
                }
            };
            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("module.iris");
            match iris::docs::generate_docs(&source, filename) {
                Ok(html) => {
                    if let Some(out_path) = output {
                        if let Err(e) = std::fs::write(&out_path, &html) {
                            eprintln!("error: cannot write '{}': {}", out_path.display(), e);
                            process::exit(1);
                        }
                        eprintln!("wrote documentation: {}", out_path.display());
                    } else {
                        print!("{}", html);
                    }
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    process::exit(1);
                }
            }
        }
        Ok(ParseArgsResult::Fmt { file, check }) => {
            let options = iris::formatter::FormatOptions::default();
            match file {
                Some(path) => match iris::formatter::format_file(&path, &options, check) {
                    Ok(changed) => {
                        if check && changed {
                            eprintln!("would reformat: {}", path.display());
                            process::exit(1);
                        } else if !check && changed {
                            eprintln!("formatted {}", path.display());
                        } else if check {
                            eprintln!("all files already formatted");
                        }
                    }
                    Err(e) => {
                        eprintln!("error: {}", e);
                        process::exit(1);
                    }
                },
                None => {
                    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    match iris::formatter::format_directory(&dir, &options, check) {
                        Ok((total, changed)) => {
                            if check && changed > 0 {
                                eprintln!(
                                    "{} file(s) would be reformatted (of {} checked)",
                                    changed, total
                                );
                                process::exit(1);
                            } else if check {
                                eprintln!("{} file(s) already formatted", total);
                            } else if changed > 0 {
                                eprintln!("formatted {} of {} file(s)", changed, total);
                            } else {
                                eprintln!("{} file(s) already formatted", total);
                            }
                        }
                        Err(e) => {
                            eprintln!("error: {}", e);
                            process::exit(1);
                        }
                    }
                }
            }
        }
        Ok(ParseArgsResult::ServiceDaemon {
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
        }) => {
            if let Err(e) = iris::evolution::run_service_daemon(iris::evolution::DaemonConfig {
                upstream,
                listen,
                ticks,
                tick_ms,
                target_p99,
                error_budget,
                max_inflight,
                headless,
                audit,
                serve: serve.then_some(port),
                ready: None,
                tls: None,
            }) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Ok(ParseArgsResult::Args(cli)) => {
            iris::set_program_args(&cli.path.to_string_lossy(), &cli.program_args);
            if cli.sandbox {
                iris::security::set_security_policy(iris::security::SecurityPolicy::sandboxed());
            }
            if cli.strict_effects {
                iris::set_strict_effects(true);
            }
            if cli.simd {
                iris::codegen::simd::set_enabled(true);
            }
            if cli.pgo_generate {
                iris::codegen::pgo::set_instrument(true);
            }
            match cli.gpu.as_deref() {
                None => {}
                Some("cuda") => iris::codegen::cuda::set_offload(true),
                Some(other) => {
                    eprintln!("error: --gpu {other}: the GPU API IRIS targets is `cuda`");
                    process::exit(1);
                }
            }
            if let Some(path) = &cli.weights {
                match std::fs::read_to_string(path) {
                    Ok(text) => iris::codegen::onnx_binary::set_weights(Some(text)),
                    Err(e) => {
                        eprintln!("error: cannot read the weights {}: {}", path.display(), e);
                        process::exit(1);
                    }
                }
            }
            if let Some(path) = &cli.pgo_use {
                match std::fs::read_to_string(path) {
                    Ok(text) => iris::codegen::pgo::set_profile(Some(text)),
                    Err(e) => {
                        eprintln!("error: cannot read the profile {}: {}", path.display(), e);
                        process::exit(1);
                    }
                }
            }
            if cli.target.is_some()
                && !matches!(
                    cli.emit,
                    iris::EmitKind::Binary | iris::EmitKind::Llvm | iris::EmitKind::LlvmComplete
                )
            {
                eprintln!(
                    "error: --target is currently supported only for native builds and LLVM IR emission"
                );
                process::exit(1);
            }
            if cli.debug && (cli.emit != iris::EmitKind::Binary || cli.target.is_some()) {
                eprintln!(
                    "error: --debug applies to host native builds (`iris build` / `iris run`) only"
                );
                process::exit(1);
            }
            // `--emit python-ext -o name.pyd` (`.so`, `.dylib`): build the
            // module Python imports, not just its C source.
            let ext_out = cli.output.as_ref().filter(|o| {
                matches!(
                    o.extension().and_then(|e| e.to_str()),
                    Some("pyd" | "so" | "dylib")
                )
            });
            if let (iris::EmitKind::PythonExt, Some(out)) = (cli.emit, ext_out) {
                let source = std::fs::read_to_string(&cli.path).unwrap_or_default();
                let module = match iris::compile_file_to_module_with_opts(
                    &cli.path,
                    cli.dump_ir_after.as_deref(),
                ) {
                    Ok(m) => m,
                    Err(e) => {
                        print_compiler_error(
                            &source,
                            &e,
                            Some(&cli.path.display().to_string()),
                            cli.error_format,
                        );
                        process::exit(1);
                    }
                };
                match iris::codegen::python_ext::build_python_ext(&module, out) {
                    Ok(path) => {
                        eprintln!("wrote Python extension: {}", path.display());
                        process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        process::exit(1);
                    }
                }
            }
            if cli.emit == iris::EmitKind::Binary {
                let source = std::fs::read_to_string(&cli.path).unwrap_or_default();
                // Debug builds use the debugger's pipeline: no inlining, DCE
                // or unrolling, so every source function keeps its frame and
                // its line table stays faithful to the source.
                let mut source_map = None;
                let compiled = if cli.debug {
                    iris::compile_file_to_module_debug(&cli.path).map(|(module, map)| {
                        source_map = Some(map);
                        module
                    })
                } else {
                    iris::compile_file_to_module_with_opts(&cli.path, cli.dump_ir_after.as_deref())
                };
                let module = match compiled {
                    Ok(m) => m,
                    Err(e) => {
                        print_compiler_error(
                            &source,
                            &e,
                            Some(&cli.path.display().to_string()),
                            cli.error_format,
                        );
                        process::exit(1);
                    }
                };
                if let Err(err) = iris::security::validate_module_ffi(&module.extern_fns) {
                    let e = iris::error::Error::Interp(
                        iris::error::InterpError::SecurityViolation(err),
                    );
                    print_compiler_error(
                        &source,
                        &e,
                        Some(&cli.path.display().to_string()),
                        cli.error_format,
                    );
                    process::exit(1);
                }
                // `iris run` without -o builds into the temp directory and
                // deletes the binary afterwards; it used to leave `name.exe`
                // in whatever directory it was run from.
                let run_scratch = (cli.run_after_build && cli.output.is_none())
                    .then(|| std::env::temp_dir().join(format!("iris-run-{}", std::process::id())));
                let output_path = cli.output.clone().unwrap_or_else(|| {
                    let stem = cli
                        .path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("iris_out");
                    let is_wasm = cli
                        .target
                        .as_deref()
                        .is_some_and(|t| resolved_target(Some(t)).contains("wasm32"));
                    let ext = if is_wasm {
                        ".wasm"
                    } else {
                        std::env::consts::EXE_SUFFIX
                    };
                    let name = format!("{}{}", stem, ext);
                    match &run_scratch {
                        Some(dir) => {
                            let _ = std::fs::create_dir_all(dir);
                            dir.join(name)
                        }
                        None => PathBuf::from(name),
                    }
                });
                let built = if let Some(map) = &source_map {
                    iris::codegen::build::build_binary_debug(&module, map, &output_path)
                } else {
                    iris::codegen::build_binary_with_target(
                        &module,
                        &output_path,
                        cli.target.as_deref(),
                    )
                };
                match built {
                    Ok(path) => {
                        if run_scratch.is_none() {
                            eprintln!(
                                "wrote binary: {}{}",
                                path.display(),
                                cli.target
                                    .as_deref()
                                    .map(|t| format!(" (target: {})", resolved_target(Some(t))))
                                    .unwrap_or_default()
                            );
                        }
                        if cli.run_after_build {
                            if !is_native_run_target(cli.target.as_deref()) {
                                eprintln!(
                                    "error: cannot run cross-target binary locally (requested target: {})",
                                    resolved_target(cli.target.as_deref())
                                );
                                process::exit(1);
                            }
                            // Canonicalize so Command finds the binary in the
                            // current directory on Windows (relative paths
                            // without ".\" are not searched).
                            let run_path =
                                std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                if let Ok(metadata) = std::fs::metadata(&run_path) {
                                    let mut perms = metadata.permissions();
                                    perms.set_mode(0o755);
                                    let _ = std::fs::set_permissions(&run_path, perms);
                                }
                            }
                            // A runtime error is written here rather than to
                            // stderr, then shown with the source line.
                            let error_file = std::env::temp_dir()
                                .join(format!("iris-error-{}.txt", std::process::id()));
                            let _ = std::fs::remove_file(&error_file);
                            let mut cmd = std::process::Command::new(&run_path);
                            cmd.args(&cli.program_args)
                                .env("IRIS_ERROR_FILE", &error_file);
                            let active_policy = iris::security::get_security_policy();
                            if !active_policy.is_default_permissive() {
                                cmd.env("IRIS_SANDBOX", "1")
                                    .env("IRIS_SECURITY_POLICY", active_policy.to_runtime_spec());
                            }
                            let status = cmd.status().unwrap_or_else(|e| {
                                eprintln!("error: could not run binary: {}", e);
                                process::exit(1);
                            });
                            if let Ok(text) = std::fs::read_to_string(&error_file) {
                                let _ = std::fs::remove_file(&error_file);
                                if let Some((_, report)) =
                                    iris::codegen::build::split_runtime_report(&text)
                                {
                                    use std::io::Write;
                                    let _ = std::io::stdout().flush();
                                    let err = iris::error::Error::Codegen(report.into_error(
                                        module.source.as_deref(),
                                        String::new(),
                                        String::new(),
                                    ));
                                    print_compiler_error(
                                        &source,
                                        &err,
                                        Some(&cli.path.display().to_string()),
                                        cli.error_format,
                                    );
                                }
                            }
                            #[cfg(unix)]
                            if !status.success() {
                                use std::os::unix::process::ExitStatusExt;
                                if let Some(signal) = status.signal() {
                                    eprintln!(
                                        "error: native program terminated by signal {signal}"
                                    );
                                }
                            }
                            #[cfg(windows)]
                            if !status.success() {
                                if let Some(code) = status.code() {
                                    if (code as u32) >= 0xC0000000 {
                                        eprintln!(
                                            "error: native program crashed ({})",
                                            iris::codegen::build::describe_exit(&status)
                                        );
                                    }
                                }
                            }
                            if let Some(dir) = &run_scratch {
                                let _ = std::fs::remove_dir_all(dir);
                            }
                            use std::io::Write;
                            let _ = std::io::stdout().flush();
                            let _ = std::io::stderr().flush();
                            process::exit(status.code().unwrap_or(1));
                        }
                    }
                    Err(e) => {
                        eprintln!("\x1b[1;31merror\x1b[0m: {}", e);
                        if !matches!(
                            e,
                            iris::error::CodegenError::ToolchainMissing { .. }
                                | iris::error::CodegenError::TargetLacks { .. }
                        ) {
                            eprintln!(
                                "note: this is a failure in IRIS's native code generation, not in \
                                 your program; `iris bug-report {}` collects what is needed to report it",
                                cli.path.display()
                            );
                        }
                        if let Some(dir) = &run_scratch {
                            let _ = std::fs::remove_dir_all(dir);
                        }
                        use std::io::Write;
                        let _ = std::io::stderr().flush();
                        process::exit(1);
                    }
                }
                return;
            }

            let source = std::fs::read_to_string(&cli.path).unwrap_or_default();
            let result = if matches!(
                cli.emit,
                iris::EmitKind::Llvm | iris::EmitKind::LlvmComplete
            ) && cli.target.is_some()
            {
                let module =
                    iris::compile_file_to_module_with_opts(&cli.path, cli.dump_ir_after.as_deref());
                module.and_then(|m| {
                    iris::codegen::emit_llvm_ir_with_target(&m, cli.target.as_deref())
                        .map_err(iris::Error::Codegen)
                })
            } else {
                iris::compile_file_with_full_opts(
                    &cli.path,
                    cli.emit,
                    cli.max_steps,
                    cli.max_depth,
                    cli.dump_ir_after.as_deref(),
                )
            };
            match result {
                Ok(output) => {
                    if let Some(out_path) = cli.output {
                        // An ONNX model or TensorRT engine is bytes, carried as hex.
                        let bytes: Vec<u8> = if matches!(
                            cli.emit,
                            iris::EmitKind::OnnxBinary | iris::EmitKind::TensorRt
                        ) {
                            (0..output.len() / 2)
                                .filter_map(|i| {
                                    u8::from_str_radix(&output[2 * i..2 * i + 2], 16).ok()
                                })
                                .collect()
                        } else {
                            output.clone().into_bytes()
                        };
                        if let Err(e) = std::fs::write(&out_path, &bytes) {
                            eprintln!("error: cannot write '{}': {}", out_path.display(), e);
                            process::exit(1);
                        }
                    } else {
                        print!("{}", output);

                        // Propagate `main`'s return value as the process exit
                        // code, which is what a built binary already does.
                        //
                        // `--emit eval` used to print the value and exit 0
                        // regardless, so the documented test idiom -- "return a
                        // non-zero code on mismatch" -- silently passed: a test
                        // that detected a failure and returned 1 still exited 0.
                        // 34 corpus files used exactly that idiom. See
                        // known-issues #53.
                        //
                        // Both eval paths document the same output shape --
                        // printed lines first, then the returned value last --
                        // so the final line is the value. A non-integer return
                        // (f64, str) leaves the exit code at 0, since there is
                        // no meaningful code to report.
                        if cli.emit == iris::EmitKind::Eval {
                            if let Some(last) = output.lines().last() {
                                if let Ok(code) = last.trim().parse::<i64>() {
                                    if code != 0 {
                                        // Exit codes are a byte; 256 would
                                        // otherwise be reported as success.
                                        let code = (code.rem_euclid(256)) as i32;
                                        process::exit(if code == 0 { 1 } else { code });
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    // A program that failed still printed what it printed.
                    {
                        use iris::error::{CodegenError, Error};
                        use std::io::Write;
                        match &e {
                            Error::Codegen(CodegenError::NativeRun { stdout, .. }) => {
                                print!("{stdout}")
                            }
                            Error::Codegen(CodegenError::Runtime { stdout, stderr, .. }) => {
                                print!("{stdout}");
                                eprint!("{stderr}");
                            }
                            _ => {}
                        }
                        let _ = std::io::stdout().flush();
                    }
                    print_compiler_error(
                        &source,
                        &e,
                        Some(&cli.path.display().to_string()),
                        cli.error_format,
                    );
                    process::exit(if e.is_runtime() { 101 } else { 1 });
                }
            }
        }
        Err(msg) => {
            // clap's own errors already name the problem and how to get help.
            if msg.starts_with("error:") {
                eprint!("{msg}");
            } else {
                eprintln!("error: {msg}");
                eprintln!("`iris --help` lists the commands");
            }
            process::exit(2);
        }
    }
}

fn run_repl() {
    use rustyline::error::ReadlineError;
    use rustyline::DefaultEditor;

    let mut repl = iris::ReplState::new();
    let version = env!("CARGO_PKG_VERSION");
    eprintln!("\x1b[1;36mIRIS {}\x1b[0m REPL  (type \x1b[1m:help\x1b[0m for commands, \x1b[1m:quit\x1b[0m to exit)", version);
    eprintln!();

    let mut rl = DefaultEditor::new().expect("failed to initialize REPL editor");
    let history_path = ".iris_history";
    let _ = rl.load_history(history_path);

    let mut accumulator = String::new();
    let mut brace_depth: i32 = 0;

    loop {
        let prompt = if brace_depth > 0 { "... " } else { ">> " };

        match rl.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line.as_str());

                // Track brace depth for multiline input.
                for ch in line.chars() {
                    if ch == '{' {
                        brace_depth += 1;
                    }
                    if ch == '}' {
                        brace_depth -= 1;
                    }
                }
                accumulator.push_str(&line);
                accumulator.push('\n');

                // Only evaluate when braces are balanced.
                if brace_depth <= 0 {
                    brace_depth = 0;
                    let input = accumulator.trim().to_owned();
                    accumulator.clear();
                    if !input.is_empty() {
                        run_repl_input(&mut repl, &input);
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                accumulator.clear();
                brace_depth = 0;
                println!("(cancelled)");
            }
            Err(ReadlineError::Eof) => {
                if !accumulator.trim().is_empty() {
                    run_repl_input(&mut repl, accumulator.trim());
                }
                println!();
                break;
            }
            Err(err) => {
                eprintln!("REPL error: {:?}", err);
                break;
            }
        }
    }
    let _ = rl.save_history(history_path);
}

fn run_repl_input(repl: &mut iris::ReplState, input: &str) {
    match repl.eval(input) {
        Ok(s) if !s.is_empty() => {
            println!("{}", s);
            // Show timing for expressions (not for meta-commands which start with :).
            if !input.trim_start().starts_with(':') {
                if let Some(d) = repl.last_elapsed() {
                    eprintln!("\x1b[90m  ({:.3}ms)\x1b[0m", d.as_secs_f64() * 1000.0);
                }
            }
        }
        Ok(_) => {}
        Err(e) => {
            // Use the rich diagnostic renderer when possible.
            // In the REPL the "source" is the input line itself.
            let rendered = render_error_colored(input, &e);
            if rendered.trim().is_empty() {
                eprintln!("\x1b[1;31merror\x1b[0m: {}", e);
            } else {
                eprint!("{}", rendered);
            }
        }
    }
}

fn resolve_audit_trust_policy(
    cli: &iris::cli::AuditTrustArgs,
) -> Result<iris::evolution::audit::AuditTrustPolicy, String> {
    if cli.audit_hmac_key.is_some() && cli.audit_hmac_key_file.is_some() {
        return Err("pass either --audit-hmac-key or --audit-hmac-key-file, not both".to_owned());
    }
    let mut trust = iris::evolution::audit::AuditTrustPolicy::from_env();
    if let Some(secret) = &cli.audit_hmac_key {
        if secret.is_empty() {
            return Err("--audit-hmac-key must not be empty".to_owned());
        }
        trust.hmac_key = Some(secret.as_bytes().to_vec());
    } else if let Some(path) = &cli.audit_hmac_key_file {
        let key = iris::evolution::audit::load_hmac_key_file(path).map_err(|e| {
            format!(
                "cannot read --audit-hmac-key-file '{}': {e}",
                path.display()
            )
        })?;
        trust.hmac_key = Some(key);
    }
    if !cli.audit_head_mirror.is_empty() {
        trust
            .checkpoint_mirrors
            .extend(cli.audit_head_mirror.iter().cloned());
    }
    trust.require_separate_domain |= cli.require_separate_audit_domain;
    Ok(trust)
}

fn open_audit_with_trust(
    audit: &std::path::Path,
    audit_head: &std::path::Path,
    trust_args: &iris::cli::AuditTrustArgs,
) -> Result<iris::evolution::audit::EvolutionAuditLog, String> {
    let trust = resolve_audit_trust_policy(trust_args)?;
    iris::evolution::audit::EvolutionAuditLog::open_with_trusted_checkpoint(
        audit, audit_head, trust,
    )
    .map_err(|e| e.to_string())
}

fn resolve_hot_swap_config(
    max_generations: Option<&str>,
    archive: Option<&std::path::Path>,
) -> Result<iris::codegen::hot_swap::HotSwapConfig, String> {
    let mut config = iris::codegen::hot_swap::HotSwapConfig::from_env();
    if let Some(raw) = max_generations {
        config.history_limit = iris::codegen::hot_swap::parse_retention_spec(raw)?;
    }
    if let Some(dir) = archive {
        config.archive_dir = Some(dir.to_path_buf());
    }
    Ok(config)
}

/// Parse `--abi`: `i64`, or `f64:N` with `1 <= N <= 8`.
/// A parsed `iris evolve --manifest` request.
struct ProgramPromotion<'a> {
    baseline: &'a std::path::Path,
    candidate: &'a std::path::Path,
    manifest: &'a std::path::Path,
    scenarios: &'a std::path::Path,
    tests: Vec<String>,
    constitution: &'a std::path::Path,
    constitution_sha256: String,
    audit: &'a std::path::Path,
    audit_head: &'a std::path::Path,
    audit_trust: &'a iris::cli::AuditTrustArgs,
    min_output: Option<&'a str>,
    max_output: Option<&'a str>,
}

/// Promote a whole program through the seven gates, handing over the
/// baseline generation's state. Exits the process on any failure.
fn promote_program(request: ProgramPromotion) {
    use iris::evolution::program::{ProgramCoordinator, ProgramPolicy, Scenario};
    let fail = |message: String| -> ! {
        eprintln!("error: {message}");
        process::exit(1);
    };
    let read = |path: &std::path::Path, what: &str| {
        std::fs::read_to_string(path).unwrap_or_else(|error| {
            fail(format!("cannot read {what} '{}': {error}", path.display()))
        })
    };
    let manifest: iris::evolution::manifest::ProgramManifest =
        serde_json::from_str(&read(request.manifest, "manifest"))
            .unwrap_or_else(|error| fail(format!("invalid manifest: {error}")));
    let scenarios: Vec<Scenario> = serde_json::from_str(&read(request.scenarios, "scenarios"))
        .unwrap_or_else(|error| fail(format!("invalid scenarios: {error}")));
    let bound = |flag: &str, text: Option<&str>, default: f64| match text {
        None => default,
        Some(text) => text
            .parse::<f64>()
            .ok()
            .filter(|value| !value.is_nan())
            .unwrap_or_else(|| fail(format!("{flag} '{text}' is not a number"))),
    };
    let low = bound("--min-output", request.min_output, f64::NEG_INFINITY);
    let high = bound("--max-output", request.max_output, f64::INFINITY);
    if low > high {
        fail("--min-output cannot exceed --max-output".into());
    }
    let baseline = iris::evolution::compile_candidate_file(request.baseline)
        .unwrap_or_else(|error| fail(format!("baseline: {error}")));
    let source = read(request.candidate, "candidate");
    let candidate = iris::evolution::compile_candidate_file(request.candidate)
        .unwrap_or_else(|error| fail(format!("candidate: {error}")));
    let mut policy = ProgramPolicy::default();
    if !request.tests.is_empty() {
        policy.required_tests = request.tests;
    }
    let coordinator =
        ProgramCoordinator::new(policy, manifest).unwrap_or_else(|error| fail(error.to_string()));
    coordinator
        .install_baseline(baseline)
        .unwrap_or_else(|error| fail(format!("baseline activation: {error}")));
    let constitution = iris::evolution::TrustedConstitution {
        source: std::fs::read(request.constitution).unwrap_or_else(|error| {
            fail(format!(
                "cannot read constitution '{}': {error}",
                request.constitution.display()
            ))
        }),
        expected_sha256: request.constitution_sha256,
    };
    let mut audit_log =
        open_audit_with_trust(request.audit, request.audit_head, request.audit_trust)
            .unwrap_or_else(|error| fail(format!("cannot open audit: {error}")));
    // Every number in every result stays within the bounds.
    fn within(value: &serde_json::Value, low: f64, high: f64) -> bool {
        match value {
            serde_json::Value::Number(number) => {
                number.as_f64().is_some_and(|v| v >= low && v <= high)
            }
            serde_json::Value::Array(items) => items.iter().all(|item| within(item, low, high)),
            serde_json::Value::Object(map) => map.values().all(|item| within(item, low, high)),
            _ => true,
        }
    }
    match coordinator.evaluate_and_promote(
        &source,
        candidate,
        &scenarios,
        &constitution,
        |_, output| within(output, low, high),
        Some(&mut audit_log),
    ) {
        Ok(receipt) => {
            println!(
                "promoted program {} generation {} after {}/7 gates",
                receipt.swap.program_name,
                receipt.swap.generation,
                receipt.gates.len()
            );
            for evidence in &receipt.gates {
                println!("  {}: {}", evidence.gate.name(), evidence.detail);
            }
            println!(
                "scenarios: candidate {}/{} exact; baseline {}/{}",
                receipt.metrics.candidate_exact,
                receipt.metrics.compared_steps,
                receipt.metrics.baseline_exact,
                receipt.metrics.compared_steps
            );
            println!(
                "audit head: {}:{}",
                audit_log.head().sequence,
                audit_log.head().hash
            );
        }
        Err(error) => fail(error.to_string()),
    }
}

/// A parsed governed promotion request: `iris evolve`, or a search winner
/// from `iris evolve-search --promote`.
struct GovernedPromotion<'a> {
    baseline: &'a std::path::Path,
    candidate_source: String,
    candidate_module: iris::IrModule,
    cases: &'a std::path::Path,
    constitution: &'a std::path::Path,
    constitution_sha256: String,
    audit: &'a std::path::Path,
    audit_head: &'a std::path::Path,
    audit_trust: &'a iris::cli::AuditTrustArgs,
    bounds: OutputBounds,
    policy: iris::evolution::EvolutionPolicy,
}

/// Install the baseline, run the candidate through the seven gates, and
/// report the receipt. Exits the process on any failure.
fn promote_governed(request: GovernedPromotion) {
    let GovernedPromotion {
        baseline,
        candidate_source,
        candidate_module,
        cases,
        constitution,
        constitution_sha256,
        audit,
        audit_head,
        audit_trust,
        bounds,
        policy,
    } = request;
    let baseline_module = match iris::evolution::compile_candidate_file(baseline) {
        Ok(module) => module,
        Err(error) => {
            eprintln!("error: baseline: {error}");
            process::exit(1);
        }
    };
    let cases_json = match std::fs::read_to_string(cases) {
        Ok(json) => json,
        Err(error) => {
            eprintln!(
                "error: cannot read canary cases '{}': {error}",
                cases.display()
            );
            process::exit(1);
        }
    };
    let constitution_source = match std::fs::read(constitution) {
        Ok(source) => source,
        Err(error) => {
            eprintln!(
                "error: cannot read constitution '{}': {error}",
                constitution.display()
            );
            process::exit(1);
        }
    };
    let trusted_constitution = iris::evolution::TrustedConstitution {
        source: constitution_source,
        expected_sha256: constitution_sha256,
    };
    let coordinator = match iris::evolution::EvolutionCoordinator::new(policy) {
        Ok(coordinator) => coordinator,
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    };
    if let Err(error) = coordinator.install_baseline(&baseline_module) {
        eprintln!("error: baseline activation: {error}");
        process::exit(1);
    }
    let mut audit_log = match open_audit_with_trust(audit, audit_head, audit_trust) {
        Ok(log) => log,
        Err(error) => {
            eprintln!(
                "error: cannot open audit '{}' with head '{}': {error}",
                audit.display(),
                audit_head.display()
            );
            process::exit(1);
        }
    };
    let invalid_cases = |error: serde_json::Error| -> ! {
        eprintln!("error: invalid canary cases '{}': {error}", cases.display());
        process::exit(1);
    };
    let outcome = match bounds {
        OutputBounds::I64(low, high) => {
            let canary_cases: Vec<iris::evolution::CanaryCase> =
                serde_json::from_str(&cases_json).unwrap_or_else(|e| invalid_cases(e));
            coordinator.evaluate_and_promote(
                &candidate_source,
                &candidate_module,
                &canary_cases,
                &trusted_constitution,
                |_, output| output >= low && output <= high,
                Some(&mut audit_log),
            )
        }
        OutputBounds::F64(low, high) => {
            let canary_cases: Vec<iris::evolution::ControlCanaryCase> =
                serde_json::from_str(&cases_json).unwrap_or_else(|e| invalid_cases(e));
            coordinator.evaluate_and_promote_control(
                &candidate_source,
                &candidate_module,
                &canary_cases,
                &trusted_constitution,
                |_, output| output >= low && output <= high,
                Some(&mut audit_log),
            )
        }
        OutputBounds::Record { low, high, fields } => {
            let canary_cases: Vec<iris::evolution::RecordCanaryCase> =
                serde_json::from_str(&cases_json).unwrap_or_else(|e| invalid_cases(e));
            coordinator.evaluate_and_promote_record(
                &candidate_source,
                &candidate_module,
                &canary_cases,
                &trusted_constitution,
                |_, output| record_within_bounds(output, low, high, &fields),
                Some(&mut audit_log),
            )
        }
    };
    match outcome {
        Ok(receipt) => {
            println!(
                "promoted {} generation {} after {}/7 gates",
                receipt.swap.function_name,
                receipt.swap.generation,
                receipt.gates.len()
            );
            for evidence in &receipt.gates {
                println!("  {}: {}", evidence.gate.name(), evidence.detail);
            }
            println!(
                "canary exact: candidate {}/{}; baseline {}/{}",
                receipt.canary.candidate_exact,
                receipt.canary.cases,
                receipt.canary.baseline_exact,
                receipt.canary.cases
            );
            println!(
                "audit head: {}:{}",
                audit_log.head().sequence,
                audit_log.head().hash
            );
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

/// `iris evolve-serve`: serve the baseline until `POST /shutdown`, printing
/// each decision as it happens and a summary at the end.
fn run_evolve_serve(args: iris::cli::EvolveServeArgs) {
    use iris::evolution::program::Scenario;
    use iris::evolution::serve::{serve_with_hot_swap, ServeConfig};
    let fail = |what: &str, error: &dyn std::fmt::Display| -> ! {
        eprintln!("error: {what}: {error}");
        process::exit(1);
    };
    if !iris::codegen::llvm_orc::is_orc_jit_available() {
        fail(
            "evolve-serve",
            &"LLVM ORC is unavailable; the service runs native code",
        );
    }
    let read = |path: &std::path::Path, what: &str| {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| fail(&format!("{what} '{}'", path.display()), &e))
    };
    let manifest: iris::evolution::manifest::ProgramManifest =
        serde_json::from_str(&read(&args.manifest, "manifest"))
            .unwrap_or_else(|e| fail("manifest", &e));
    let scenarios: Vec<Scenario> = serde_json::from_str(&read(&args.scenarios, "scenarios"))
        .unwrap_or_else(|e| fail("scenarios", &e));
    let min_output = args.min_output.unwrap_or(f64::NEG_INFINITY);
    let max_output = args.max_output.unwrap_or(f64::INFINITY);
    if min_output > max_output {
        fail("evolve-serve", &"--min-output cannot exceed --max-output");
    }
    let baseline = iris::evolution::compile_candidate_file(&args.baseline)
        .unwrap_or_else(|e| fail(&format!("baseline '{}'", args.baseline.display()), &e));
    let constitution = iris::evolution::TrustedConstitution {
        source: std::fs::read(&args.constitution).unwrap_or_else(|e| {
            fail(
                &format!("constitution '{}'", args.constitution.display()),
                &e,
            )
        }),
        expected_sha256: args.constitution_sha256.clone(),
    };
    let hot_swap = resolve_hot_swap_config(
        args.max_generations.as_deref(),
        args.generation_archive.as_deref(),
    )
    .unwrap_or_else(|e| fail("hot-swap", &e));
    let audit = open_audit_with_trust(&args.audit, &args.audit_head, &args.audit_trust)
        .unwrap_or_else(|e| fail("audit", &e));
    let config = ServeConfig {
        manifest,
        baseline,
        scenarios,
        constitution,
        min_output,
        max_output,
        tests: args.tests.clone(),
        port: args.port,
        max_body_bytes: args.max_body,
    };
    let port_file = args.port_file.clone();
    let report = serve_with_hot_swap(
        config,
        audit,
        hot_swap,
        move |address| {
            if let Some(path) = &port_file {
                if let Err(e) = std::fs::write(path, address.port().to_string()) {
                    eprintln!("error: port file '{}': {e}", path.display());
                    process::exit(1);
                }
            }
        },
        |event| {
            println!(
                "[{:>7.3}s  {:>5} requests] {:<12} {}",
                event.at_ms as f64 / 1000.0,
                event.requests,
                event.kind.label(),
                event.detail
            );
        },
    )
    .unwrap_or_else(|e| fail("evolve-serve", &e));
    let served: Vec<String> = report
        .served_by
        .iter()
        .map(|(generation, count)| format!("generation {generation}: {count}"))
        .collect();
    println!();
    println!("{} requests ({})", report.requests, served.join(", "));
    println!(
        "final generation {}; state {}",
        report.generation,
        serde_json::Value::Object(report.state)
    );
    match iris::evolution::audit::verify_file(&args.audit) {
        Ok(head) => println!("audit chain verified: head {}:{}", head.sequence, head.hash),
        Err(e) => fail("audit verification", &e),
    }
}

/// `iris evolve-live`: run the loop, printing each evolution event as it
/// happens and a summary at the end. Exits non-zero on any failure.
fn run_evolve_live(args: iris::cli::EvolveLiveArgs) {
    use iris::evolution::live::{run_live_with_hot_swap, LiveConfig, LiveEventKind};
    let fail = |what: &str, error: &dyn std::fmt::Display| -> ! {
        eprintln!("error: {what}: {error}");
        process::exit(1);
    };
    if !iris::codegen::llvm_orc::is_orc_jit_available() {
        fail(
            "evolve-live",
            &"LLVM ORC is unavailable; the loop runs native code",
        );
    }
    if args.min_output > args.max_output {
        fail("evolve-live", &"--min-output cannot exceed --max-output");
    }
    let plant = iris::compile_file_to_module(&args.plant)
        .unwrap_or_else(|e| fail(&format!("plant '{}'", args.plant.display()), &e));
    let baseline = iris::evolution::compile_candidate_file(&args.baseline)
        .unwrap_or_else(|e| fail(&format!("baseline '{}'", args.baseline.display()), &e));
    let candidates = args
        .candidates
        .iter()
        .map(|path| {
            let source = std::fs::read_to_string(path)
                .unwrap_or_else(|e| fail(&format!("candidate '{}'", path.display()), &e));
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "candidate".into());
            (name, source)
        })
        .collect();
    let cases_json = std::fs::read_to_string(&args.cases)
        .unwrap_or_else(|e| fail(&format!("cases '{}'", args.cases.display()), &e));
    let cases: Vec<iris::evolution::ControlCanaryCase> = serde_json::from_str(&cases_json)
        .unwrap_or_else(|e| fail(&format!("cases '{}'", args.cases.display()), &e));
    let constitution = iris::evolution::TrustedConstitution {
        source: std::fs::read(&args.constitution).unwrap_or_else(|e| {
            fail(
                &format!("constitution '{}'", args.constitution.display()),
                &e,
            )
        }),
        expected_sha256: args.constitution_sha256.clone(),
    };
    let hot_swap = resolve_hot_swap_config(
        args.max_generations.as_deref(),
        args.generation_archive.as_deref(),
    )
    .unwrap_or_else(|e| fail("hot-swap", &e));
    let mut audit = open_audit_with_trust(&args.audit, &args.audit_head, &args.audit_trust)
        .unwrap_or_else(|e| fail(&format!("audit '{}'", args.audit.display()), &e));
    let search = args.search.then(|| iris::evolution::FloatSearchConfig {
        inputs: 2,
        population_size: args.pop_size,
        generations: args.generations,
        seed: args.seed,
        ..iris::evolution::FloatSearchConfig::default()
    });
    let config = LiveConfig {
        plant,
        baseline,
        candidates,
        cases,
        constitution,
        output_low: args.min_output,
        output_high: args.max_output,
        setpoints: args.setpoints.clone(),
        hold_seconds: args.hold,
        dt: args.dt,
        offer_every: args.offer_every,
        search,
        policy: iris::evolution::EvolutionPolicy {
            abi: iris::evolution::PolicyAbi::ControlF64 { inputs: 2 },
            require_allocation_free: args.require_allocation_free,
            ..iris::evolution::EvolutionPolicy::default()
        },
        realtime: args.realtime,
    };
    println!(
        "evolve-live: {} setpoints held {} s each, dt {} s, actuator rating [{}, {}]",
        args.setpoints.len(),
        args.hold,
        args.dt,
        args.min_output,
        args.max_output
    );
    let report = run_live_with_hot_swap(config, &mut audit, hot_swap, |event| {
        println!(
            "[t={:7.2}s] {:<15} {}: {}",
            event.time,
            event.kind.label(),
            event.subject,
            event.detail
        );
    })
    .unwrap_or_else(|e| fail("evolve-live", &e));
    println!("\nsetpoint  final error  settled        peak force  generation");
    for segment in &report.segments {
        let settled = segment
            .settled_after
            .map_or("no".to_owned(), |s| format!("after {s:.2} s"));
        println!(
            "{:>8.3}  {:>+11.4}  {:<13}  {:>10.3}  {}",
            segment.target, segment.final_error, settled, segment.peak_force, segment.generation
        );
    }
    if let Err(e) = audit.verify() {
        fail("audit verification", &e);
    }
    println!(
        "\n{} steps; controller call mean {} ns, worst {} ns",
        report.steps, report.mean_call_ns, report.worst_call_ns
    );
    println!(
        "{} promoted, {} rejected, {} rolled back; active generation {}",
        report.count(LiveEventKind::Promoted),
        report.count(LiveEventKind::Rejected),
        report.count(LiveEventKind::RolledBack),
        report.active_generation
    );
    println!(
        "audit chain verified: head {}:{}",
        audit.head().sequence,
        audit.head().hash
    );
}

fn parse_policy_abi(text: &str) -> Result<iris::evolution::PolicyAbi, String> {
    if text == "i64" {
        return Ok(iris::evolution::PolicyAbi::ScalarI64);
    }
    if text == "record" {
        return Ok(iris::evolution::PolicyAbi::Record);
    }
    let inputs = text
        .strip_prefix("f64:")
        .and_then(|count| count.parse::<usize>().ok())
        .ok_or_else(|| format!("expected 'i64', 'f64:N' or 'record', found '{text}'"))?;
    if !(1..=iris::evolution::MAX_CONTROL_INPUTS).contains(&inputs) {
        return Err(format!(
            "f64 policies take 1..={} inputs, found {inputs}",
            iris::evolution::MAX_CONTROL_INPUTS
        ));
    }
    Ok(iris::evolution::PolicyAbi::ControlF64 { inputs })
}

/// The constitution's output range, typed by the policy ABI.
#[derive(Clone)]
enum OutputBounds {
    I64(i64, i64),
    F64(f64, f64),
    /// Every numeric field within `low..=high`, and each named field within
    /// its own range too.
    Record {
        low: f64,
        high: f64,
        fields: Vec<(String, f64, f64)>,
    },
}

/// Whether a record output satisfies the CLI constitution.
fn record_within_bounds(
    output: &iris::evolution::RecordData,
    low: f64,
    high: f64,
    fields: &[(String, f64, f64)],
) -> bool {
    use iris::evolution::RecordField;
    let numeric = |value: RecordField| match value {
        RecordField::F64(v) => Some(v),
        RecordField::I64(v) => Some(v as f64),
        RecordField::Bool(_) => None,
    };
    output
        .fields
        .iter()
        .filter_map(|(_, value)| numeric(*value))
        .all(|v| v >= low && v <= high)
        && fields.iter().all(|(name, field_low, field_high)| {
            output
                .get(name)
                .and_then(numeric)
                .is_some_and(|v| v >= *field_low && v <= *field_high)
        })
}

impl OutputBounds {
    fn parse(
        abi: iris::evolution::PolicyAbi,
        min: Option<&str>,
        max: Option<&str>,
        field_bounds: &[String],
    ) -> Result<Self, String> {
        if !field_bounds.is_empty() && abi != iris::evolution::PolicyAbi::Record {
            return Err("--bound applies to record policies only".into());
        }
        fn bound<T: std::str::FromStr>(
            flag: &str,
            text: Option<&str>,
            default: T,
        ) -> Result<T, String> {
            match text {
                None => Ok(default),
                Some(text) => text
                    .parse()
                    .map_err(|_| format!("{flag} '{text}' is not a valid number for this ABI")),
            }
        }
        match abi {
            iris::evolution::PolicyAbi::ScalarI64 => {
                let low = bound("--min-output", min, i64::MIN)?;
                let high = bound("--max-output", max, i64::MAX)?;
                if low > high {
                    return Err("--min-output cannot exceed --max-output".into());
                }
                Ok(Self::I64(low, high))
            }
            iris::evolution::PolicyAbi::ControlF64 { .. } => {
                let low = bound("--min-output", min, f64::NEG_INFINITY)?;
                let high = bound("--max-output", max, f64::INFINITY)?;
                if low.is_nan() || high.is_nan() || low > high {
                    return Err(
                        "--min-output must not exceed --max-output, and neither may be NaN".into(),
                    );
                }
                Ok(Self::F64(low, high))
            }
            iris::evolution::PolicyAbi::Record => {
                let low = bound("--min-output", min, f64::NEG_INFINITY)?;
                let high = bound("--max-output", max, f64::INFINITY)?;
                if low.is_nan() || high.is_nan() || low > high {
                    return Err(
                        "--min-output must not exceed --max-output, and neither may be NaN".into(),
                    );
                }
                let mut fields = Vec::with_capacity(field_bounds.len());
                for spec in field_bounds {
                    let parsed = spec.split_once('=').and_then(|(name, range)| {
                        let (lo, hi) = range.split_once(':')?;
                        let lo = if lo.is_empty() {
                            f64::NEG_INFINITY
                        } else {
                            lo.parse().ok()?
                        };
                        let hi = if hi.is_empty() {
                            f64::INFINITY
                        } else {
                            hi.parse().ok()?
                        };
                        (!name.is_empty() && lo <= hi).then(|| (name.to_owned(), lo, hi))
                    });
                    match parsed {
                        Some(field) => fields.push(field),
                        None => {
                            return Err(format!(
                                "--bound '{spec}' must be FIELD=MIN:MAX with MIN <= MAX"
                            ))
                        }
                    }
                }
                Ok(Self::Record { low, high, fields })
            }
        }
    }
}
