//! Public API tests: real IRIS programs compiled and evaluated end to end.
use iris::{compile_multi, EmitKind};

#[test]
fn documented_pipeline_example_runs() {
    let source = include_str!("../examples/06_data/stdlib_pipeline.iris");
    let output = compile_multi(&[("main", source)], "main", EmitKind::Eval).unwrap();
    assert!(output.contains("stdlib pipeline: ok"), "{output}");
}

fn check(imports: &str, body: &str) {
    let src = format!("{imports}\ndef main() -> bool {{\n{body}\n}}");
    let output = compile_multi(&[("main", &src)], "main", EmitKind::Eval)
        .unwrap_or_else(|e| panic!("{e}\n{src}"));
    assert_eq!(output.trim(), "true", "{src}\n{output}");
}

#[test]
fn string_affixes_are_optional_and_remove_once() {
    check(
        "bring std.string",
        r#"
        unwrap(strip_prefix("preprefix", "pre")) == "prefix" &&
        unwrap(strip_suffix("name.tar.gz", ".gz")) == "name.tar" &&
        is_none(strip_prefix("value", "missing")) &&
        is_none(strip_suffix("value", "missing")) &&
        unwrap(strip_prefix("éclair", "é")) == "clair" &&
        unwrap(strip_suffix("café", "é")) == "caf" &&
        unwrap(strip_prefix("", "")) == "" &&
        unwrap(strip_suffix("x", "")) == "x"
    "#,
    );
}

#[test]
fn split_once_distinguishes_absent_and_empty_delimiters() {
    check(
        "bring std.string",
        r#"
        val (key, value) = unwrap(split_once("key=a=b", "="))
        val (head, tail) = unwrap(split_once("abc", ""))
        val (empty, trailing) = unwrap(split_once("=", "="))
        key == "key" && value == "a=b" && head == "" && tail == "abc" &&
        empty == "" && trailing == "" && is_none(split_once("abc", ":"))
    "#,
    );
}

#[test]
fn bounded_split_preserves_remainder_and_empty_fields() {
    check(
        "bring std.string",
        r#"
        join(split_n("a::b::c", "::", 2), "|") == "a|b::c" &&
        join(split_n("::a::::", "::", 10), "|") == "|a||" &&
        list_get(split_n("a,b", ",", 1), 0) == "a,b" &&
        len(split_n("", ",", 3)) == 1 &&
        len(split_n("a", ",", 0)) == 0 &&
        len(split_n("a", ",", -1)) == 0 &&
        list_get(split_n("abc", "", 5), 0) == "abc"
    "#,
    );
}

#[test]
fn bounded_replace_scans_original_nonoverlapping_matches() {
    check(
        "bring std.string",
        r#"
        replace_n("a-a-a", "a", "b", 2) == "b-b-a" &&
        replace_n("aaaaa", "aa", "X", 10) == "XXa" &&
        replace_n("aa", "a", "aa", 2) == "aaaa" &&
        replace_n("é-é", "é", "世界", 1) == "世界-é" &&
        replace_n("abc", "b", "", 1) == "ac" &&
        replace_n("abc", "x", "!", 3) == "abc" &&
        replace_n("abc", "", "!", 3) == "abc" &&
        replace_n("abc", "a", "!", 0) == "abc" &&
        replace_n("", "x", "!", 3) == ""
    "#,
    );
}

#[test]
fn whitespace_normalization_and_blank_detection() {
    check(
        "bring std.string",
        r#"
        normalize_whitespace(" \t hello\r\n世界  iris \t") == "hello 世界 iris" &&
        is_blank(" \t\r\n") && is_blank("") && !is_blank(" x ") &&
        !is_blank("é") && normalize_whitespace("") == ""
    "#,
    );
}

#[test]
fn map_filter_fold_support_captures_and_preserve_input() {
    check(
        "bring std.iter",
        r#"
        val xs = range(1, 6)
        val offset = 10
        val mapped = map_i64(xs, |x: i64| x + offset)
        val filtered = filter_i64(mapped, |x: i64| x % 2 == 0)
        val total = fold_i64(filtered, 100, |acc: i64, x: i64| acc + x)
        val order = fold_i64(range(1, 4), 0, |acc: i64, x: i64| acc * 10 + x)
        list_set(mapped, 0, 99);
        total == 126 && order == 123 && list_get(xs, 0) == 1 &&
        len(filtered) == 2 && list_get(filtered, 0) == 12
    "#,
    );
}

#[test]
fn empty_iterator_operations_have_defined_identities() {
    check(
        "bring std.iter",
        r#"
        val xs: list<i64> = list()
        len(map_i64(xs, |x: i64| x + 1)) == 0 &&
        len(filter_i64(xs, |x: i64| true)) == 0 &&
        fold_i64(xs, 42, |acc: i64, x: i64| acc + x) == 42 &&
        !any_i64(xs, |x: i64| true) && all_i64(xs, |x: i64| false) &&
        is_none(find_i64(xs, |x: i64| true)) && len(unique_i64(xs)) == 0
    "#,
    );
}

#[test]
fn predicate_queries_short_circuit() {
    check(
        "bring std.iter",
        r#"
        val xs = range(1, 6)
        val seen: list<i64> = list()
        val any = any_i64(xs, |x: i64| { push(seen, x); x == 2 })
        val all = all_i64(xs, |x: i64| { push(seen, x); x < 3 })
        val found = find_i64(xs, |x: i64| { push(seen, x); x == 4 })
        any && !all && unwrap(found) == 4 && len(seen) == 9 &&
        is_none(find_i64(xs, |x: i64| x > 10))
    "#,
    );
}

#[test]
fn partition_and_unique_are_stable_and_independent() {
    check(
        "bring std.iter",
        r#"
        val xs: list<i64> = list()
        push(xs, 3); push(xs, 2); push(xs, 3); push(xs, 1); push(xs, 2);
        val unique = unique_i64(xs)
        val seen: list<i64> = list()
        val (yes, no) = partition_i64(xs, |x: i64| { push(seen, x); x % 2 == 0 })
        list_set(yes, 0, 100);
        len(unique) == 3 && list_get(unique, 0) == 3 && list_get(unique, 1) == 2 &&
        list_get(unique, 2) == 1 && len(yes) == 2 && len(no) == 3 &&
        list_get(no, 0) == 3 && list_get(no, 2) == 1 && len(seen) == 5 &&
        list_get(xs, 1) == 2
    "#,
    );
}

#[test]
fn chunks_and_windows_copy_values_and_handle_sizes() {
    check(
        "bring std.iter",
        r#"
        val xs = range(1, 6)
        val chunks = chunks_i64(xs, 2)
        val windows = windows_i64(xs, 3)
        list_set(list_get(windows, 0), 1, 99);
        len(chunks) == 3 && len(list_get(chunks, 2)) == 1 &&
        list_get(list_get(chunks, 2), 0) == 5 && len(windows) == 3 &&
        list_get(list_get(windows, 1), 0) == 2 && list_get(xs, 1) == 2 &&
        len(chunks_i64(xs, 0)) == 0 && len(chunks_i64(xs, -2)) == 0 &&
        len(chunks_i64(xs, 20)) == 1 && len(windows_i64(xs, 0)) == 0 &&
        len(windows_i64(xs, -2)) == 0 && len(windows_i64(xs, 20)) == 0 &&
        len(windows_i64(xs, 5)) == 1 && len(windows_i64(xs, 1)) == 5 &&
        len(chunks_i64(range(0, 0), 2)) == 0
    "#,
    );
}

#[test]
fn path_components_and_absolute_detection() {
    check(
        "bring std.path",
        r#"
        path_is_absolute("/a") && path_is_absolute("/") &&
        !path_is_absolute("") && !path_is_absolute("a/b") &&
        !path_is_absolute("C:\\a") &&
        join(path_components("//a/./b/../"), "|") == "a|.|b|.." &&
        len(path_components("///")) == 0 && len(path_components("")) == 0
    "#,
    );
}

#[test]
fn lexical_path_normalization_handles_roots_and_parent_components() {
    check(
        "bring std.path",
        r#"
        normalize_path("//a/./b/../c//") == "/a/c" &&
        normalize_path("/../../a") == "/a" &&
        normalize_path("../../a/../b") == "../../b" &&
        normalize_path("a/../../b") == "../b" &&
        normalize_path("a/..") == "." && normalize_path("") == "." &&
        normalize_path("/a/..") == "/" && normalize_path("///") == "/" &&
        normalize_path("./é/../世界") == "世界" &&
        normalize_path(normalize_path("a//b/../c")) == "a/c"
    "#,
    );
}

#[test]
fn extension_replacement_handles_dotfiles_and_directories() {
    check(
        "bring std.path",
        r#"
        with_extension("dir/file.tar.gz", "zip") == "dir/file.tar.zip" &&
        with_extension("dir/file.txt", "") == "dir/file" &&
        with_extension("dir/.env", "local") == "dir/.env.local" &&
        with_extension("dir/.env.local", "") == "dir/.env" &&
        with_extension("dir.name/file", "txt") == "dir.name/file.txt" &&
        with_extension("file.", "txt") == "file.txt" &&
        with_extension("dir/", "txt") == "dir/" &&
        with_extension("..", "txt") == ".." && with_extension("", "txt") == ""
    "#,
    );
}

#[test]
fn new_apis_compose_across_modules() {
    check(
        "bring std.string\nbring std.iter\nbring std.path",
        r#"
        val (key, value) = unwrap(split_once("path=./out/../data/file.old", "="))
        val output = with_extension(normalize_path(value), "iris")
        val sizes = map_i64(range(1, 4), |x: i64| x * 2)
        key == "path" && output == "data/file.iris" &&
        normalize_whitespace("  data\t ready ") == "data ready" &&
        fold_i64(sizes, 0, |acc: i64, x: i64| acc + x) == 12
    "#,
    );
}
