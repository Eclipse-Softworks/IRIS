//! Suggestions for diagnostics: the name the programmer probably meant, names
//! carried over from other languages, the module that exports a function, and
//! a placeholder value of a type.

/// Edit distance counting a swap of adjacent characters as one edit (optimal
/// string alignment), so `pritnln` is one edit from `println`.
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate().take(m + 1) {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[n][m]
}

/// The closest candidate, if it is close enough to be a typo: one edit for
/// names of up to five characters, two beyond. One- and two-letter names get
/// no suggestion: `z` is one edit from every other one-letter name.
pub(crate) fn closest<'a>(
    name: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let len = name.chars().count();
    let allowed = match len {
        0..=2 => return None,
        3..=5 => 1,
        _ => 2,
    };
    let lower = name.to_ascii_lowercase();
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        if c == name || c.starts_with("__") || c.contains("__") {
            continue;
        }
        // A difference of case alone is the most likely mistake of all.
        let d = if c.to_ascii_lowercase() == lower {
            0
        } else {
            edit_distance(name, c)
        };
        if d <= allowed
            && best
                .map(|(bd, bc)| d < bd || (d == bd && c < bc))
                .unwrap_or(true)
        {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c.to_string())
}

/// A name from another language and its IRIS spelling, with why. `called`:
/// the name is called, so function names from other languages apply too
/// (`count` is a fine variable but `count(xs)` means `len(xs)`).
pub(crate) fn foreign_name(name: &str, called: bool) -> Option<(&'static str, &'static str)> {
    if called {
        if let Some(found) = foreign_function(name) {
            return Some(found);
        }
    }
    Some(match name {
        "True" | "TRUE" => ("true", "booleans are written in lower case"),
        "False" | "FALSE" => ("false", "booleans are written in lower case"),
        "null" | "nil" | "None" | "NULL" | "undefined" | "nullptr" => (
            "none",
            "IRIS has no null: an absent value is `none`, an `option<T>`",
        ),
        _ => return None,
    })
}

fn foreign_function(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "printf" | "puts" | "console" | "echo" | "print_line" => {
            ("println", "IRIS prints a line with `println`")
        }
        "length" | "size" | "count" | "strlen" => {
            ("len", "the length of a string or list is `len(x)`")
        }
        "append" | "push_back" => ("push", "a list grows with `push(xs, value)`"),
        "string" | "str" | "String" | "toString" | "to_string" => {
            ("to_str", "convert to text with `to_str(x)`")
        }
        _ => return None,
    })
}

/// The built-in type a name from another language means.
pub(crate) fn type_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "int" | "integer" | "Int" | "Integer" | "long" | "Long" | "isize" | "i128" => "i64",
        "uint" | "UInt" | "u128" => "u64",
        "float" | "double" | "Float" | "Double" | "number" | "Number" | "real" => "f64",
        "string" | "String" | "Str" | "char" | "Char" | "text" => "str",
        "boolean" | "Boolean" | "Bool" => "bool",
        "void" | "Void" | "None" | "()" | "Unit" => "unit",
        "List" | "Vec" | "vec" | "array" | "Array" => "list",
        "Map" | "HashMap" | "dict" | "Dict" | "hashmap" => "map",
        "Option" | "Optional" | "Maybe" => "option",
        "Result" => "result",
        _ => return None,
    })
}

/// Built-in type names, for suggesting the one meant by a misspelling.
pub(crate) const BUILTIN_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "usize", "f32", "f64", "bool", "str",
    "unit", "list", "map", "option", "result", "chan", "task", "tensor", "array",
];

/// The standard-library module that exports `pub def name`, if one does.
pub(crate) fn module_exporting(name: &str) -> Option<&'static str> {
    let needle = format!("pub def {name}(");
    let generic = format!("pub def {name}[");
    crate::stdlib::STDLIB_MODULE_NAMES
        .iter()
        .copied()
        .find(|module| {
            crate::stdlib::stdlib_source(module)
                .is_some_and(|src| src.contains(&needle) || src.contains(&generic))
        })
}

/// A value of `ty`, written in source, for filling in a missing field.
pub(crate) fn placeholder(ty: &crate::ir::types::IrType) -> Option<String> {
    use crate::ir::types::{DType, IrType};
    Some(match ty {
        IrType::Scalar(DType::F32) | IrType::Scalar(DType::F64) => "0.0".into(),
        IrType::Scalar(DType::Bool) => "false".into(),
        IrType::Scalar(_) => "0".into(),
        IrType::Str => "\"\"".into(),
        IrType::List(_) => "list()".into(),
        IrType::Map(_, _) => "map()".into(),
        IrType::Option(_) => "none".into(),
        _ => return None,
    })
}

/// The broad kind of a type, for deciding whether an argument can be the
/// parameter's type. `None` when the type is not known well enough to judge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    Int,
    Float,
    Bool,
    Str,
    List,
    Map,
    Option,
    Result,
    Named(String),
}

pub(crate) fn kind(ty: &crate::ir::types::IrType) -> Option<Kind> {
    use crate::ir::types::{DType, IrType};
    Some(match ty {
        IrType::Scalar(DType::F32 | DType::F64) => Kind::Float,
        IrType::Scalar(DType::Bool) => Kind::Bool,
        IrType::Scalar(_) => Kind::Int,
        IrType::Str => Kind::Str,
        IrType::List(_) => Kind::List,
        IrType::Map(_, _) => Kind::Map,
        IrType::Option(_) => Kind::Option,
        IrType::ResultType(_, _) => Kind::Result,
        // Monomorphised names (`Box__i64`) are compared elsewhere.
        IrType::Struct { name, .. } | IrType::Enum { name, .. } if !name.contains("__") => {
            Kind::Named(name.clone())
        }
        _ => return None,
    })
}

/// Whether an argument of type `arg` clearly cannot be passed for a parameter
/// of type `param`. Only certain mismatches count: an integer passed for a
/// float is converted, and anything not fully known is left to type inference.
pub(crate) fn incompatible(
    arg: &crate::ir::types::IrType,
    param: &crate::ir::types::IrType,
) -> bool {
    match (kind(arg), kind(param)) {
        (Some(Kind::Int), Some(Kind::Float)) => false,
        (Some(a), Some(p)) => a != p,
        _ => false,
    }
}

/// How a type is written in source, for messages (`list<i64>`, not `List(I64)`).
pub(crate) fn type_name(ty: &crate::ir::types::IrType) -> String {
    let text = ty.to_string();
    text.trim_start_matches('%').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transposition_is_one_edit() {
        assert_eq!(edit_distance("pritnln", "println"), 1);
        assert_eq!(edit_distance("totl", "total"), 1);
        assert_eq!(edit_distance("abc", "abc"), 0);
    }

    #[test]
    fn short_names_get_no_suggestion() {
        assert_eq!(closest("z", ["x", "y"]), None);
        assert_eq!(closest("ab", ["ac"]), None);
        assert_eq!(closest("totl", ["total", "x"]), Some("total".into()));
        assert_eq!(
            closest("pritnln", ["println", "print"]),
            Some("println".into())
        );
        assert_eq!(closest("Total", ["total"]), Some("total".into()));
    }

    #[test]
    fn a_stdlib_export_is_found() {
        assert!(module_exporting("sqrt_f64")
            .or(module_exporting("clamp"))
            .is_some());
        assert_eq!(module_exporting("definitely_not_a_function_xyz"), None);
    }
}
