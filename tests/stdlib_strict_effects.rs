//! Regression tests ensuring all embedded stdlib modules pass `--strict-effects`
//! with zero errors and zero unused-effect warnings, and that strict mode still
//! rejects unannotated or mis-declared effects.

use iris::ir::module::IrModule;
use iris::pass::effect_checker::EffectChecker;
use iris::stdlib::STDLIB_MODULE_NAMES;

fn run_strict_check(
    source: &str,
    module_name: &str,
) -> (Vec<String>, Result<IrModule, iris::Error>) {
    let mut ast = iris::compile_multi_to_ast(&[(module_name, source)], module_name)
        .unwrap_or_else(|e| panic!("failed to parse module '{module_name}': {e}"));
    let mut checker = EffectChecker::new(true);
    checker.run(&ast);
    let compiled = iris::compile_ast_to_module_with_effect_mode(&mut ast, module_name, None, true);
    (checker.errors, compiled)
}

#[test]
fn all_stdlib_modules_compile_cleanly_under_strict_effects() {
    for module in STDLIB_MODULE_NAMES {
        let source = format!("bring std.{module}\ndef main() -> i64 {{ 0 }}\n");
        let mod_name = format!("stdlib_strict_{module}");
        let (diagnostics, compiled) = run_strict_check(&source, &mod_name);
        assert!(
            diagnostics.is_empty(),
            "std.{module} produced strict-effect diagnostics (errors or warnings):\n{}",
            diagnostics.join("\n")
        );
        if let Err(err) = compiled {
            panic!("std.{module} failed strict compilation: {err}");
        }
    }
}

#[test]
fn strict_effects_rejects_unannotated_and_mismatched_effects() {
    let cases = [
        (
            "unannotated_os_shell",
            "bring std.os\ndef main() -> i64 {\n    val _s = shell(\"echo hi\")\n    0\n}\n",
            &["E0301", "sys", "alloc"][..],
        ),
        (
            "pure_fs_read",
            "bring std.fs\ndef main() -> i64 effect pure {\n    val _r = read_text(\"missing.txt\")\n    0\n}\n",
            &["E0303", "fs", "alloc"][..],
        ),
        (
            "unannotated_spawn",
            "def main() -> i64 {\n    spawn { val x = 1; };\n    0\n}\n",
            &["E0301", "thread", "alloc"][..],
        ),
        (
            "pure_spawn",
            "def main() -> i64 effect pure {\n    spawn { val x = 1; };\n    0\n}\n",
            &["E0303", "thread", "alloc"][..],
        ),
    ];

    for (name, source, expected_fragments) in cases {
        let (diagnostics, compiled) = run_strict_check(source, name);
        let joined = diagnostics.join("\n");
        for fragment in expected_fragments {
            assert!(
                joined.contains(fragment),
                "case '{name}' expected diagnostic containing '{fragment}', got:\n{joined}"
            );
        }
        assert!(
            compiled.is_err(),
            "case '{name}' unexpectedly passed strict compilation"
        );
    }

    let pure_src = "bring std.math\ndef main() -> i64 effect pure {\n    val _discard = 1\n    clamp_i64(5, 0, 10)\n}\n";
    let (pure_diags, pure_compiled) = run_strict_check(pure_src, "pure_math_and_wildcard");
    assert!(
        pure_diags.is_empty(),
        "pure math + wildcard val _ should have zero diagnostics, got:\n{}",
        pure_diags.join("\n")
    );
    assert!(
        pure_compiled.is_ok(),
        "pure math + wildcard val _ failed strict compilation: {:?}",
        pure_compiled.err()
    );
}
