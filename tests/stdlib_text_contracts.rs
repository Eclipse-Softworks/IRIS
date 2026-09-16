//! Behavioral contracts for the core text standard library, executed natively.
use iris::{compile_multi, EmitKind};

fn check(module: &str, body: &str) {
    let src = format!("bring std.{module}\ndef main() -> bool {{\n{body}\n}}");
    let output = compile_multi(&[("main", &src)], "main", EmitKind::Eval)
        .unwrap_or_else(|e| panic!("{e}\n{src}"));
    assert_eq!(output.trim(), "true", "{src}\n{output}");
}

#[test]
fn directional_trim_preserves_opposite_end_and_utf8() {
    check(
        "string",
        r#"
        trim_start(" \t\r\néclair  ") == "éclair  " &&
        trim_end("  éclair\r\n\t ") == "  éclair" &&
        trim_start("") == "" && trim_end(" \t\n") == "" &&
        trim_start("éclair") == "éclair"
    "#,
    );
}

#[test]
fn words_collapse_ascii_whitespace() {
    check(
        "string",
        r#"
        join(words(" \thello  世界\r\niris\t"), "|") == "hello|世界|iris" &&
        len(words("")) == 0 && len(words(" \r\n\t")) == 0
    "#,
    );
}

#[test]
fn lines_handle_empty_crlf_and_trailing_terminators() {
    check(
        "string",
        r#"
        len(lines("")) == 0 && len(lines("\n")) == 1 &&
        join(lines("a\r\n\r\nb\n"), "|") == "a||b" &&
        join(lines("a\rb\nlast\r"), "|") == "a\rb|last\r" &&
        len(lines("a\n\n")) == 2
    "#,
    );
}

#[test]
fn padding_uses_whole_tokens_and_byte_widths() {
    check(
        "string",
        r#"
        pad_left("x", 4, "ab") == "ababx" &&
        pad_right("x", 4, "ab") == "xabab" &&
        pad_left("x", 100, "") == "x" &&
        pad_right("long", 2, ".") == "long" &&
        pad_left("x", -1, ".") == "x" &&
        pad_right("é", 4, ".") == "é.." &&
        pad_left("x", 4, "é") == "ééx"
    "#,
    );
}

#[test]
fn numeric_zero_padding_preserves_sign() {
    check(
        "fmt",
        r#"
        zero_pad_int(-42, 6) == "-00042" &&
        zero_pad_int(-42, 2) == "-42" &&
        zero_pad_int(0, 3) == "000" &&
        sprintf("%06d", split("-42", ",")) == "-00042" &&
        sprintf("%06d", split("+42", ",")) == "+00042" &&
        sprintf("%-6d", split("-42", ",")) == "-42   "
    "#,
    );
}

#[test]
fn table_formats_headers_rows_and_minimum_widths() {
    check(
        "fmt",
        r#"
        val headers = split("Name,N", ",")
        val rows: list<list<str>> = list()
        list_push(rows, split("Ada,12", ","));
        list_push(rows, split("Grace,7", ","));
        val widths: list<i64> = list()
        list_push(widths, 7);
        list_push(widths, 1);
        format_table(headers, rows, none) == "Name  | N\nAda   | 12\nGrace | 7" &&
        format_table(headers, rows, some(widths)) == "Name    | N\nAda     | 12\nGrace   | 7"
    "#,
    );
}

#[test]
fn table_handles_empty_and_ragged_inputs() {
    check(
        "fmt",
        r#"
        val empty: list<str> = list()
        val no_rows: list<list<str>> = list()
        val rows: list<list<str>> = list()
        list_push(rows, split("a", ","));
        list_push(rows, split("long,b", ","));
        val widths: list<i64> = list()
        list_push(widths, -1);
        format_table(empty, no_rows, none) == "" &&
        format_table(empty, rows, some(widths)) == "a    | \nlong | b" &&
        format_table(split("H", ","), no_rows, none) == "H"
    "#,
    );
}
