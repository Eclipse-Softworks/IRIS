//! CPython extension modules from IRIS.
//!
//! `emit_python_ext` writes the C source of a module that exposes the
//! program's functions to Python; `build_python_ext` (`--emit python-ext
//! -o name.pyd|.so`) compiles it with the program and the runtime into a
//! module Python imports.
//!
//! Values cross by type, converted both ways:
//!
//! | IRIS                         | Python                                      |
//! |------------------------------|---------------------------------------------|
//! | integers, `f32`/`f64`, `bool`| `int`, `float`, `bool` (range-checked)      |
//! | `str`                        | `str`                                       |
//! | `list<T>`, `[T; N]`, slices  | `list`                                      |
//! | `map<K, V>`                  | `dict`                                      |
//! | tuples                       | `tuple`                                     |
//! | `option<T>`                  | the value, or `None`                        |
//! | `result<T, E>`               | the `ok` value; an `err` raises `IrisError(e)` |
//! | records                      | `dict` by field name                        |
//! | choices                      | `"Variant"`, or `("Variant", fields...)`    |
//! | tensors                      | nested lists by shape (from anything with `tolist()`) |
//! | `grad`                       | `(value, tangent)`                          |
//! | channels, closures, tasks... | a capsule, which can be passed back         |
//!
//! A runtime error inside IRIS code raises `<module>.IrisError` with the
//! error's report (`error[E0403]: ... --> file:line:col`) instead of ending
//! the Python process, also from `par for` workers. Calls release the GIL.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::CodegenError;
use crate::ir::function::IrFunction;
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};

/// Emits complete C source for a CPython C-extension module wrapping the IRIS module.
pub fn emit_python_ext(module: &IrModule) -> Result<String, CodegenError> {
    let name = if module.name.is_empty() {
        "iris_ext"
    } else {
        &module.name
    };
    emit_python_ext_named(module, &c_ident(name), cfg!(target_os = "macos"))
}

/// The functions a module exports: the program's own, not the standard
/// library's (`module__fn`), helpers (`_x`) or `main`.
fn exported(module: &IrModule) -> Vec<&IrFunction> {
    module
        .functions()
        .iter()
        .filter(|f| {
            !f.name.starts_with('_')
                && f.name != "main"
                && !f.name.contains("__")
                && !f.name.is_empty()
        })
        .collect()
}

fn c_ident(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// The C type of a value as IRIS functions pass it.
fn ctype(ty: &IrType) -> &'static str {
    match ty {
        IrType::Scalar(DType::I64) => "int64_t",
        IrType::Scalar(DType::U64 | DType::USize) => "uint64_t",
        IrType::Scalar(DType::I32) => "int32_t",
        IrType::Scalar(DType::U32) => "uint32_t",
        IrType::Scalar(DType::I8) => "int8_t",
        IrType::Scalar(DType::U8) => "uint8_t",
        IrType::Scalar(DType::F64) => "double",
        IrType::Scalar(DType::F32) => "float",
        // An `i1`: only its low bit is defined.
        IrType::Scalar(DType::Bool) => "uint8_t",
        IrType::Str => "char*",
        _ => "void*",
    }
}

/// `unit`: the empty record `%unit` (or an empty tuple). It is returned as
/// a pointer nothing reads, and is `None` in Python.
fn is_unit(ty: &IrType) -> bool {
    match ty {
        IrType::Tuple(t) => t.is_empty(),
        IrType::Struct { name, fields } => name == "unit" && fields.is_empty(),
        _ => false,
    }
}

fn opaque_kind(ty: &IrType) -> Option<&'static str> {
    Some(match ty {
        IrType::Chan(_) => "chan",
        IrType::Atomic(_) => "atomic",
        IrType::Mutex(_) => "mutex",
        IrType::Task(_) => "task",
        IrType::TaskGroup => "task_group",
        IrType::WeakRef(_) => "weak_ref",
        IrType::Fn { .. } => "closure",
        IrType::TraitObject { .. } => "dyn",
        IrType::TapeRef => "tape",
        _ => return None,
    })
}

/// The runtime's box and unbox helpers for heap values that have them.
fn heap_box(ty: &IrType) -> Option<(&'static str, &'static str)> {
    Some(match ty {
        IrType::List(_) => ("iris_box_list", "iris_unbox_list"),
        IrType::Map(_, _) => ("iris_box_map", "iris_unbox_map"),
        IrType::Option(_) => ("iris_box_option", "iris_unbox_option"),
        IrType::ResultType(_, _) => ("iris_box_result", "iris_unbox_result"),
        IrType::Chan(_) => ("iris_box_chan", "iris_unbox_chan"),
        IrType::Atomic(_) => ("iris_box_atomic", "iris_unbox_atomic"),
        IrType::Mutex(_) => ("iris_box_mutex", "iris_unbox_mutex"),
        IrType::Grad(_) => ("iris_box_grad", "iris_unbox_grad"),
        IrType::Sparse(_) => ("iris_box_sparse", "iris_unbox_sparse"),
        IrType::Task(_) => ("iris_box_task", "iris_unbox_task"),
        IrType::TaskGroup => ("iris_box_task_group", "iris_unbox_task_group"),
        IrType::Struct { .. } => ("iris_box_native_object", "iris_unbox_native_object"),
        _ => return None,
    })
}

/// Generates, once per type, the four converters:
/// `iris_to_py_N(native)`, `iris_from_py_N(obj, &native)` (1, or 0 with an
/// exception set), `iris_unbox_py_N(IrisVal*)` and `iris_box_py_N(obj)` for a
/// value inside a collection, boxed as the compiler boxes it.
struct Gen<'m> {
    module: &'m IrModule,
    types: Vec<IrType>,
    decls: String,
    defs: String,
}

impl<'m> Gen<'m> {
    fn id(&mut self, ty: &IrType) -> Result<usize, CodegenError> {
        if let Some(i) = self.types.iter().position(|t| t == ty) {
            return Ok(i);
        }
        let i = self.types.len();
        self.types.push(ty.clone());
        let ct = ctype(ty);
        writeln!(self.decls, "static PyObject* iris_to_py_{i}({ct} v);")?;
        writeln!(
            self.decls,
            "static int iris_from_py_{i}(PyObject* o, {ct}* out);"
        )?;
        writeln!(
            self.decls,
            "static PyObject* iris_unbox_py_{i}(IrisVal* v);"
        )?;
        writeln!(self.decls, "static IrisVal* iris_box_py_{i}(PyObject* o);")?;
        self.define(i, ty)?;
        Ok(i)
    }

    fn define(&mut self, i: usize, ty: &IrType) -> Result<(), CodegenError> {
        let ct = ctype(ty);
        let tname = ty.to_string().replace('"', "'");
        let mut to = String::new();
        let mut from = String::new();
        let mut unbox = String::new();
        let mut boxed = String::new();
        let err = |what: &str| {
            format!("    PyErr_Format(PyExc_TypeError, \"expected {what} for `{tname}`, not %s\", Py_TYPE(o)->tp_name);\n    return 0;\n")
        };

        match ty {
            IrType::Scalar(d) => {
                let (lo, hi, unsigned) = match d {
                    DType::I64 => ("INT64_MIN", "INT64_MAX", false),
                    DType::I32 => ("INT32_MIN", "INT32_MAX", false),
                    DType::I8 => ("INT8_MIN", "INT8_MAX", false),
                    DType::U64 | DType::USize => ("0", "UINT64_MAX", true),
                    DType::U32 => ("0", "UINT32_MAX", true),
                    DType::U8 => ("0", "UINT8_MAX", true),
                    _ => ("", "", false),
                };
                match d {
                    DType::F64 | DType::F32 => {
                        writeln!(to, "    return PyFloat_FromDouble((double)v);")?;
                        writeln!(
                            from,
                            "    if (!PyFloat_Check(o) && !PyLong_Check(o)) {{\n{}    }}",
                            err("a number")
                        )?;
                        writeln!(from, "    double d = PyFloat_AsDouble(o);\n    if (d == -1.0 && PyErr_Occurred()) return 0;\n    *out = ({ct})d;\n    return 1;")?;
                        let field = if *d == DType::F32 { "f32" } else { "f64" };
                        writeln!(unbox, "    if (!v) Py_RETURN_NONE;\n    return PyFloat_FromDouble(v->tag == IRIS_TAG_F32 ? (double)v->f32 : v->f64);")?;
                        writeln!(boxed, "    {ct} x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    return iris_box_{field}(x);")?;
                    }
                    DType::Bool => {
                        writeln!(to, "    return PyBool_FromLong(v & 1);")?;
                        writeln!(from, "    if (!PyBool_Check(o)) {{\n{}    }}\n    *out = (uint8_t)(o == Py_True);\n    return 1;", err("a bool"))?;
                        writeln!(unbox, "    if (!v) Py_RETURN_NONE;\n    return PyBool_FromLong(v->boolean & 1);")?;
                        writeln!(boxed, "    uint8_t x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    return iris_box_bool((int)x);")?;
                    }
                    _ => {
                        if unsigned {
                            writeln!(
                                to,
                                "    return PyLong_FromUnsignedLongLong((unsigned long long)v);"
                            )?;
                            writeln!(
                                from,
                                "    if (!PyLong_Check(o)) {{\n{}    }}",
                                err("an int")
                            )?;
                            writeln!(from, "    unsigned long long x = PyLong_AsUnsignedLongLong(o);\n    if (x == (unsigned long long)-1 && PyErr_Occurred()) {{\n        PyErr_Format(PyExc_OverflowError, \"out of range for `{tname}`\");\n        return 0;\n    }}")?;
                            writeln!(from, "    if (x > (unsigned long long){hi}) {{\n        PyErr_Format(PyExc_OverflowError, \"%llu is out of range for `{tname}`\", x);\n        return 0;\n    }}\n    *out = ({ct})x;\n    return 1;")?;
                        } else {
                            writeln!(to, "    return PyLong_FromLongLong((long long)v);")?;
                            writeln!(
                                from,
                                "    if (!PyLong_Check(o)) {{\n{}    }}",
                                err("an int")
                            )?;
                            writeln!(from, "    int overflow = 0;\n    long long x = PyLong_AsLongLongAndOverflow(o, &overflow);\n    if (x == -1 && PyErr_Occurred()) return 0;")?;
                            writeln!(from, "    if (overflow || x < (long long){lo} || x > (long long){hi}) {{\n        PyErr_Format(PyExc_OverflowError, \"%S is out of range for `{tname}`\", o);\n        return 0;\n    }}\n    *out = ({ct})x;\n    return 1;")?;
                        }
                        let read = if *d == DType::I32 {
                            "(v->tag == IRIS_TAG_I32 ? (int64_t)v->i32 : v->i64)"
                        } else {
                            "v->i64"
                        };
                        writeln!(
                            unbox,
                            "    if (!v) Py_RETURN_NONE;\n    return iris_to_py_{i}(({ct}){read});"
                        )?;
                        let b = if *d == DType::I32 {
                            "iris_box_i32(x)"
                        } else {
                            "iris_box_i64((int64_t)x)"
                        };
                        writeln!(boxed, "    {ct} x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    return {b};")?;
                    }
                }
            }
            IrType::Str => {
                writeln!(to, "    return PyUnicode_FromString(v ? v : \"\");")?;
                writeln!(
                    from,
                    "    if (!PyUnicode_Check(o)) {{\n{}    }}",
                    err("a str")
                )?;
                writeln!(from, "    Py_ssize_t n;\n    const char* s = PyUnicode_AsUTF8AndSize(o, &n);\n    if (!s) return 0;")?;
                writeln!(from, "    if ((Py_ssize_t)strlen(s) != n) {{\n        PyErr_SetString(PyExc_ValueError, \"an IRIS str cannot hold a NUL character\");\n        return 0;\n    }}")?;
                writeln!(from, "    char* copy = (char*)iris_alloc_bytes((uint64_t)n + 1);\n    memcpy(copy, s, (size_t)n + 1);\n    *out = copy;\n    return 1;")?;
                writeln!(
                    unbox,
                    "    if (!v) Py_RETURN_NONE;\n    return iris_to_py_{i}(v->str);"
                )?;
                writeln!(boxed, "    char* x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    return iris_box_str(x);")?;
            }
            IrType::List(elem) => {
                let e = self.id(elem)?;
                writeln!(to, "    IrisList* l = (IrisList*)v;\n    int64_t n = l ? iris_list_len(l) : 0;\n    PyObject* r = PyList_New((Py_ssize_t)n);\n    if (!r) return NULL;")?;
                writeln!(to, "    for (int64_t k = 0; k < n; k++) {{\n        PyObject* x = iris_unbox_py_{e}(iris_list_get(l, k));\n        if (!x) {{ Py_DECREF(r); return NULL; }}\n        PyList_SET_ITEM(r, (Py_ssize_t)k, x);\n    }}\n    return r;")?;
                writeln!(
                    from,
                    "    if (!PyList_Check(o) && !PyTuple_Check(o)) {{\n{}    }}",
                    err("a list")
                )?;
                writeln!(from, "    IrisList* l = iris_list_new();\n    Py_ssize_t n = PySequence_Fast_GET_SIZE(o);\n    PyObject** items = PySequence_Fast_ITEMS(o);")?;
                writeln!(from, "    for (Py_ssize_t k = 0; k < n; k++) {{\n        IrisVal* x = iris_box_py_{e}(items[k]);\n        if (!x) return 0;\n        iris_list_push(l, x);\n    }}\n    *out = l;\n    return 1;")?;
                self.boxed_heap(ty, i, &mut unbox, &mut boxed)?;
            }
            IrType::Array { elem, len } => {
                let e = self.id(elem)?;
                let ect = ctype(elem);
                let scalar = matches!(**elem, IrType::Scalar(_));
                if scalar {
                    writeln!(
                        to,
                        "    PyObject* r = PyList_New({len});\n    if (!r) return NULL;"
                    )?;
                    writeln!(to, "    for (Py_ssize_t k = 0; k < {len}; k++) {{\n        PyObject* x = iris_to_py_{e}((({ect}*)v)[k]);\n        if (!x) {{ Py_DECREF(r); return NULL; }}\n        PyList_SET_ITEM(r, k, x);\n    }}\n    return r;")?;
                } else {
                    writeln!(to, "    IrisList* l = (IrisList*)v;\n    PyObject* r = PyList_New({len});\n    if (!r) return NULL;")?;
                    writeln!(to, "    for (Py_ssize_t k = 0; k < {len}; k++) {{\n        PyObject* x = iris_unbox_py_{e}(iris_list_get(l, k));\n        if (!x) {{ Py_DECREF(r); return NULL; }}\n        PyList_SET_ITEM(r, k, x);\n    }}\n    return r;")?;
                }
                writeln!(from, "    if ((!PyList_Check(o) && !PyTuple_Check(o)) || PySequence_Fast_GET_SIZE(o) != {len}) {{\n        PyErr_Format(PyExc_TypeError, \"expected a list of {len} for `{tname}`\");\n        return 0;\n    }}\n    PyObject** items = PySequence_Fast_ITEMS(o);")?;
                if scalar {
                    writeln!(from, "    {ect}* a = ({ect}*)iris_alloc_bytes(sizeof({ect}) * {len});\n    for (Py_ssize_t k = 0; k < {len}; k++)\n        if (!iris_from_py_{e}(items[k], &a[k])) return 0;\n    *out = a;\n    return 1;")?;
                    writeln!(unbox, "    if (!v) Py_RETURN_NONE;\n    return iris_to_py_{i}(iris_unbox_native_object(v));")?;
                    writeln!(boxed, "    void* x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    return iris_box_scalar_array(x, (int64_t)(sizeof({ect}) * {len}));")?;
                } else {
                    writeln!(from, "    IrisList* l = iris_list_new();\n    for (Py_ssize_t k = 0; k < {len}; k++) {{\n        IrisVal* x = iris_box_py_{e}(items[k]);\n        if (!x) return 0;\n        iris_list_push(l, x);\n    }}\n    *out = l;\n    return 1;")?;
                    writeln!(unbox, "    if (!v) Py_RETURN_NONE;\n    return iris_to_py_{i}(iris_unbox_list(v));")?;
                    writeln!(boxed, "    void* x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    return iris_box_array_list((IrisList*)x);")?;
                }
            }
            IrType::Slice { elem, .. } => {
                let e = self.id(elem)?;
                let ect = ctype(elem);
                writeln!(to, "    struct {{ {ect}* data; int64_t len; }}* s = v;\n    PyObject* r = PyList_New((Py_ssize_t)s->len);\n    if (!r) return NULL;")?;
                writeln!(to, "    for (int64_t k = 0; k < s->len; k++) {{\n        PyObject* x = iris_to_py_{e}(s->data[k]);\n        if (!x) {{ Py_DECREF(r); return NULL; }}\n        PyList_SET_ITEM(r, (Py_ssize_t)k, x);\n    }}\n    return r;")?;
                writeln!(
                    from,
                    "    if (!PyList_Check(o) && !PyTuple_Check(o)) {{\n{}    }}",
                    err("a list")
                )?;
                writeln!(from, "    Py_ssize_t n = PySequence_Fast_GET_SIZE(o);\n    PyObject** items = PySequence_Fast_ITEMS(o);")?;
                writeln!(from, "    struct {{ {ect}* data; int64_t len; }}* s = iris_alloc_bytes(sizeof *s);\n    s->data = ({ect}*)iris_alloc_bytes(sizeof({ect}) * (uint64_t)(n ? n : 1));\n    s->len = n;")?;
                writeln!(from, "    for (Py_ssize_t k = 0; k < n; k++)\n        if (!iris_from_py_{e}(items[k], &s->data[k])) return 0;\n    *out = s;\n    return 1;")?;
                writeln!(
                    unbox,
                    "    if (!v) Py_RETURN_NONE;\n    return iris_to_py_{i}((void*)v);"
                )?;
                writeln!(boxed, "    void* x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    return (IrisVal*)x;")?;
            }
            IrType::Map(k, val) => {
                let kid = self.id(k)?;
                let vid = self.id(val)?;
                // Keys are stored as their text; read back by the key type.
                let key_from_text = match **k {
                    IrType::Scalar(DType::F64 | DType::F32) => {
                        "PyFloat_FromDouble(strtod(key, NULL))"
                    }
                    IrType::Scalar(DType::Bool) => "PyBool_FromLong(strcmp(key, \"true\") == 0)",
                    IrType::Scalar(_) => "PyLong_FromString(key, NULL, 10)",
                    _ => "PyUnicode_FromString(key)",
                };
                writeln!(to, "    IrisMap* m = (IrisMap*)v;\n    PyObject* r = PyDict_New();\n    if (!r || !m) return r;\n    IrisList* keys = iris_map_keys(m);\n    int64_t n = iris_list_len(keys);")?;
                writeln!(to, "    for (int64_t k = 0; k < n; k++) {{\n        IrisVal* kv = iris_list_get(keys, k);\n        const char* key = kv->str;\n        IrisOption* got = iris_map_get(m, kv);")?;
                writeln!(to, "        PyObject* pk = {key_from_text};\n        PyObject* pv = (got && got->has_value) ? iris_unbox_py_{vid}(got->value) : (Py_INCREF(Py_None), Py_None);")?;
                writeln!(to, "        if (!pk || !pv || PyDict_SetItem(r, pk, pv) < 0) {{ Py_XDECREF(pk); Py_XDECREF(pv); Py_DECREF(r); return NULL; }}\n        Py_DECREF(pk);\n        Py_DECREF(pv);\n    }}\n    return r;")?;
                writeln!(
                    from,
                    "    if (!PyDict_Check(o)) {{\n{}    }}",
                    err("a dict")
                )?;
                writeln!(from, "    IrisMap* m = iris_map_new();\n    PyObject *pk, *pv;\n    Py_ssize_t pos = 0;")?;
                writeln!(from, "    while (PyDict_Next(o, &pos, &pk, &pv)) {{\n        IrisVal* bk = iris_box_py_{kid}(pk);\n        if (!bk) return 0;\n        IrisVal* bv = iris_box_py_{vid}(pv);\n        if (!bv) return 0;\n        iris_map_set(m, bk, bv);\n    }}\n    *out = m;\n    return 1;")?;
                self.boxed_heap(ty, i, &mut unbox, &mut boxed)?;
            }
            IrType::Option(inner) => {
                let e = self.id(inner)?;
                writeln!(to, "    IrisOption* p = (IrisOption*)v;\n    if (!p || !p->has_value) Py_RETURN_NONE;\n    return iris_unbox_py_{e}(p->value);")?;
                writeln!(from, "    if (o == Py_None) {{ *out = iris_make_none(); return 1; }}\n    IrisVal* x = iris_box_py_{e}(o);\n    if (!x) return 0;\n    *out = iris_make_some(x);\n    return 1;")?;
                self.boxed_heap(ty, i, &mut unbox, &mut boxed)?;
            }
            IrType::ResultType(okt, errt) => {
                let o_id = self.id(okt)?;
                let e_id = self.id(errt)?;
                writeln!(to, "    IrisResult* p = (IrisResult*)v;\n    if (!p) Py_RETURN_NONE;\n    if (p->is_ok) return iris_unbox_py_{o_id}(p->value);")?;
                writeln!(to, "    PyObject* e = iris_unbox_py_{e_id}(p->value);\n    if (!e) return NULL;\n    PyErr_SetObject(IrisError, e);\n    Py_DECREF(e);\n    return NULL;")?;
                writeln!(from, "    if (!PyTuple_Check(o) || PyTuple_GET_SIZE(o) != 2 || !PyUnicode_Check(PyTuple_GET_ITEM(o, 0))) {{\n        PyErr_Format(PyExc_TypeError, \"expected (\\\"ok\\\", value) or (\\\"err\\\", error) for `{tname}`\");\n        return 0;\n    }}")?;
                writeln!(from, "    const char* which = PyUnicode_AsUTF8(PyTuple_GET_ITEM(o, 0));\n    PyObject* x = PyTuple_GET_ITEM(o, 1);")?;
                writeln!(from, "    if (strcmp(which, \"ok\") == 0) {{ IrisVal* b = iris_box_py_{o_id}(x); if (!b) return 0; *out = iris_make_ok(b); return 1; }}")?;
                writeln!(from, "    if (strcmp(which, \"err\") == 0) {{ IrisVal* b = iris_box_py_{e_id}(x); if (!b) return 0; *out = iris_make_err(b); return 1; }}")?;
                writeln!(from, "    PyErr_Format(PyExc_ValueError, \"expected \\\"ok\\\" or \\\"err\\\" for `{tname}`, not \\\"%s\\\"\", which);\n    return 0;")?;
                self.boxed_heap(ty, i, &mut unbox, &mut boxed)?;
            }
            IrType::Tuple(elems) => {
                let ids: Vec<usize> = elems.iter().map(|t| self.id(t)).collect::<Result<_, _>>()?;
                let n = ids.len();
                writeln!(to, "    if (!v) Py_RETURN_NONE;\n    IrisList* l = (IrisList*)((IrisVal*)v)->ptr;\n    PyObject* r = PyTuple_New({n});\n    if (!r) return NULL;")?;
                for (k, e) in ids.iter().enumerate() {
                    writeln!(to, "    {{ PyObject* x = iris_unbox_py_{e}(iris_list_get(l, {k})); if (!x) {{ Py_DECREF(r); return NULL; }} PyTuple_SET_ITEM(r, {k}, x); }}")?;
                }
                writeln!(to, "    return r;")?;
                writeln!(from, "    if (!PyTuple_Check(o) || PyTuple_GET_SIZE(o) != {n}) {{\n        PyErr_Format(PyExc_TypeError, \"expected a tuple of {n} for `{tname}`\");\n        return 0;\n    }}")?;
                let mut args = Vec::new();
                for (k, e) in ids.iter().enumerate() {
                    writeln!(from, "    IrisVal* b{k} = iris_box_py_{e}(PyTuple_GET_ITEM(o, {k}));\n    if (!b{k}) return 0;")?;
                    args.push(format!("b{k}"));
                }
                let rest = if args.is_empty() {
                    String::new()
                } else {
                    format!(", {}", args.join(", "))
                };
                writeln!(
                    from,
                    "    *out = iris_make_tuple({n}{rest});\n    return 1;"
                )?;
                writeln!(
                    unbox,
                    "    if (!v) Py_RETURN_NONE;\n    return iris_to_py_{i}((void*)v);"
                )?;
                writeln!(boxed, "    void* x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    return (IrisVal*)x;")?;
            }
            IrType::Struct { name, fields } => {
                let ids: Vec<usize> = fields
                    .iter()
                    .map(|(_, t)| self.id(t))
                    .collect::<Result<_, _>>()?;
                let layout: Vec<String> = fields
                    .iter()
                    .enumerate()
                    .map(|(k, (_, t))| format!("{} f{k};", ctype(t)))
                    .collect();
                let rec = format!("iris_rec_{i}");
                writeln!(
                    self.decls,
                    "typedef struct {{ {} }} {rec};",
                    if layout.is_empty() {
                        "char unused;".to_owned()
                    } else {
                        layout.join(" ")
                    }
                )?;
                writeln!(to, "    {rec}* s = ({rec}*)v;\n    PyObject* r = PyDict_New();\n    if (!r || !s) return r;")?;
                for (k, ((fname, _), e)) in fields.iter().zip(&ids).enumerate() {
                    writeln!(to, "    {{ PyObject* x = iris_to_py_{e}(s->f{k}); if (!x || PyDict_SetItemString(r, \"{fname}\", x) < 0) {{ Py_XDECREF(x); Py_DECREF(r); return NULL; }} Py_DECREF(x); }}")?;
                }
                writeln!(to, "    return r;")?;
                writeln!(
                    from,
                    "    if (!PyDict_Check(o)) {{\n{}    }}",
                    err(&format!("a dict of the fields of {name}"))
                )?;
                writeln!(from, "    if (PyDict_Size(o) != {}) {{\n        PyErr_Format(PyExc_TypeError, \"`{name}` has {} fields, not %zd\", PyDict_Size(o));\n        return 0;\n    }}", fields.len(), fields.len())?;
                writeln!(
                    from,
                    "    {rec}* s = ({rec}*)iris_alloc_bytes(sizeof({rec}));"
                )?;
                for (k, ((fname, _), e)) in fields.iter().zip(&ids).enumerate() {
                    writeln!(from, "    {{ PyObject* x = PyDict_GetItemString(o, \"{fname}\");\n      if (!x) {{ PyErr_Format(PyExc_TypeError, \"missing field `{fname}` of `{name}`\"); return 0; }}\n      if (!iris_from_py_{e}(x, &s->f{k})) return 0; }}")?;
                }
                writeln!(from, "    *out = s;\n    return 1;")?;
                self.boxed_heap(ty, i, &mut unbox, &mut boxed)?;
            }
            IrType::Enum { name, variants } => {
                let field_tys = self
                    .module
                    .enum_variant_fields(name)
                    .cloned()
                    .unwrap_or_else(|| vec![Vec::new(); variants.len()]);
                let mut field_ids: Vec<Vec<usize>> = Vec::new();
                for fs in &field_tys {
                    field_ids.push(fs.iter().map(|t| self.id(t)).collect::<Result<_, _>>()?);
                }
                writeln!(to, "    int64_t tag = iris_get_variant_tag((IrisVal*)v);")?;
                for (k, variant) in variants.iter().enumerate() {
                    let ids = field_ids.get(k).cloned().unwrap_or_default();
                    if ids.is_empty() {
                        writeln!(
                            to,
                            "    if (tag == {k}) return PyUnicode_FromString(\"{variant}\");"
                        )?;
                    } else {
                        writeln!(to, "    if (tag == {k}) {{\n        IrisEnum* e = (IrisEnum*)((IrisVal*)v)->ptr;\n        PyObject* r = PyTuple_New({});\n        if (!r) return NULL;\n        PyTuple_SET_ITEM(r, 0, PyUnicode_FromString(\"{variant}\"));", ids.len() + 1)?;
                        for (f, e) in ids.iter().enumerate() {
                            writeln!(to, "        {{ PyObject* x = iris_unbox_py_{e}(e->fields[{f}]); if (!x) {{ Py_DECREF(r); return NULL; }} PyTuple_SET_ITEM(r, {}, x); }}", f + 1)?;
                        }
                        writeln!(to, "        return r;\n    }}")?;
                    }
                }
                writeln!(to, "    PyErr_Format(PyExc_ValueError, \"`{name}` has no variant %lld\", (long long)tag);\n    return NULL;")?;
                writeln!(from, "    PyObject* head = o;\n    Py_ssize_t nargs = 0;")?;
                writeln!(from, "    if (PyTuple_Check(o) && PyTuple_GET_SIZE(o) >= 1) {{ head = PyTuple_GET_ITEM(o, 0); nargs = PyTuple_GET_SIZE(o) - 1; }}")?;
                writeln!(from, "    if (!PyUnicode_Check(head)) {{\n        PyErr_Format(PyExc_TypeError, \"expected a variant of `{name}`: \\\"Name\\\" or (\\\"Name\\\", fields...)\");\n        return 0;\n    }}\n    const char* vn = PyUnicode_AsUTF8(head);")?;
                for (k, variant) in variants.iter().enumerate() {
                    let ids = field_ids.get(k).cloned().unwrap_or_default();
                    let n = ids.len();
                    writeln!(from, "    if (strcmp(vn, \"{variant}\") == 0) {{\n        if (nargs != {n}) {{ PyErr_Format(PyExc_TypeError, \"`{name}.{variant}` takes {n} fields, not %zd\", nargs); return 0; }}")?;
                    if ids.is_empty() {
                        writeln!(
                            from,
                            "        *out = (void*)(uintptr_t){k};\n        return 1;\n    }}"
                        )?;
                    } else {
                        let mut args = Vec::new();
                        for (f, e) in ids.iter().enumerate() {
                            writeln!(from, "        IrisVal* b{f} = iris_box_py_{e}(PyTuple_GET_ITEM(o, {}));\n        if (!b{f}) return 0;", f + 1)?;
                            args.push(format!("b{f}"));
                        }
                        writeln!(from, "        *out = iris_make_variant({k}, {n}, {});\n        return 1;\n    }}", args.join(", "))?;
                    }
                }
                writeln!(from, "    PyErr_Format(PyExc_ValueError, \"`{name}` has no variant `%s`\", vn);\n    return 0;")?;
                writeln!(
                    unbox,
                    "    if (!v) Py_RETURN_NONE;\n    return iris_to_py_{i}((void*)v);"
                )?;
                // A tag-only variant inside a collection is a boxed integer.
                writeln!(boxed, "    void* x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    if ((uintptr_t)x <= 0xFFFF) return iris_box_i64((int64_t)(uintptr_t)x);\n    return (IrisVal*)x;")?;
            }
            IrType::Tensor { .. } => {
                writeln!(to, "    IrisTensor* t = (IrisTensor*)v;\n    if (!t) Py_RETURN_NONE;\n    int64_t at = 0;\n    return iris_py_tensor_nest(t, 0, &at);")?;
                writeln!(
                    from,
                    "    *out = iris_py_tensor_from(o);\n    return *out != NULL;"
                )?;
                writeln!(unbox, "    return iris_to_py_{i}((void*)v);")?;
                writeln!(boxed, "    void* x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    return (IrisVal*)x;")?;
            }
            IrType::Grad(_) => {
                writeln!(to, "    IrisGrad* g = (IrisGrad*)v;\n    if (!g) Py_RETURN_NONE;\n    return Py_BuildValue(\"(dd)\", g->value, g->tangent);")?;
                writeln!(from, "    double value, tangent = 0.0;\n    if (PyTuple_Check(o)) {{ if (!PyArg_ParseTuple(o, \"dd\", &value, &tangent)) return 0; }}")?;
                writeln!(from, "    else {{ value = PyFloat_AsDouble(o); if (value == -1.0 && PyErr_Occurred()) return 0; }}\n    *out = iris_make_grad(value, tangent);\n    return 1;")?;
                self.boxed_heap(ty, i, &mut unbox, &mut boxed)?;
            }
            IrType::Sparse(inner) => {
                let e = self.id(inner)?;
                writeln!(to, "    IrisSparse* s = (IrisSparse*)v;\n    PyObject* r = PyDict_New();\n    if (!r || !s) return r;")?;
                writeln!(to, "    for (size_t k = 0; k < s->len; k++) {{\n        PyObject* pk = PyLong_FromSize_t(s->indices[k]);\n        PyObject* pv = iris_unbox_py_{e}(s->values[k]);")?;
                writeln!(to, "        if (!pk || !pv || PyDict_SetItem(r, pk, pv) < 0) {{ Py_XDECREF(pk); Py_XDECREF(pv); Py_DECREF(r); return NULL; }}\n        Py_DECREF(pk);\n        Py_DECREF(pv);\n    }}\n    return r;")?;
                writeln!(
                    from,
                    "    if (!PyDict_Check(o)) {{\n{}    }}",
                    err("a dict of index: value")
                )?;
                writeln!(from, "    Py_ssize_t n = PyDict_Size(o), pos = 0, k = 0;\n    PyObject *pk, *pv;\n    IrisSparse* s = (IrisSparse*)iris_alloc_bytes(sizeof(IrisSparse));")?;
                writeln!(from, "    s->indices = (size_t*)iris_alloc_bytes(sizeof(size_t) * (uint64_t)(n ? n : 1));\n    s->values = (IrisVal**)iris_alloc_bytes(sizeof(IrisVal*) * (uint64_t)(n ? n : 1));\n    s->len = s->cap = (size_t)n;")?;
                writeln!(from, "    while (PyDict_Next(o, &pos, &pk, &pv)) {{\n        size_t idx = PyLong_AsSize_t(pk);\n        if (idx == (size_t)-1 && PyErr_Occurred()) return 0;\n        IrisVal* b = iris_box_py_{e}(pv);\n        if (!b) return 0;\n        s->indices[k] = idx;\n        s->values[k] = b;\n        k++;\n    }}")?;
                // Ascending indices, as the runtime keeps them.
                writeln!(from, "    for (Py_ssize_t a = 1; a < n; a++)\n        for (Py_ssize_t b = a; b > 0 && s->indices[b - 1] > s->indices[b]; b--) {{\n            size_t ti = s->indices[b]; s->indices[b] = s->indices[b - 1]; s->indices[b - 1] = ti;\n            IrisVal* tv = s->values[b]; s->values[b] = s->values[b - 1]; s->values[b - 1] = tv;\n        }}\n    *out = s;\n    return 1;")?;
                self.boxed_heap(ty, i, &mut unbox, &mut boxed)?;
            }
            other => {
                let kind = opaque_kind(other).unwrap_or("value");
                let cap = format!("iris.{kind}");
                writeln!(
                    to,
                    "    if (!v) Py_RETURN_NONE;\n    return PyCapsule_New(v, \"{cap}\", NULL);"
                )?;
                writeln!(from, "    if (!PyCapsule_IsValid(o, \"{cap}\")) {{\n{}    }}\n    *out = PyCapsule_GetPointer(o, \"{cap}\");\n    return 1;", err(&format!("an IRIS {kind} (a capsule an IRIS function returned)")))?;
                if heap_box(other).is_some() {
                    self.boxed_heap(other, i, &mut unbox, &mut boxed)?;
                } else {
                    writeln!(unbox, "    return iris_to_py_{i}((void*)v);")?;
                    writeln!(boxed, "    void* x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    return (IrisVal*)x;")?;
                }
            }
        }

        writeln!(self.defs, "/* {tname} */")?;
        writeln!(
            self.defs,
            "static PyObject* iris_to_py_{i}({ct} v) {{\n{to}}}"
        )?;
        writeln!(
            self.defs,
            "static int iris_from_py_{i}(PyObject* o, {ct}* out) {{\n{from}}}"
        )?;
        writeln!(
            self.defs,
            "static PyObject* iris_unbox_py_{i}(IrisVal* v) {{\n{unbox}}}"
        )?;
        writeln!(
            self.defs,
            "static IrisVal* iris_box_py_{i}(PyObject* o) {{\n{boxed}}}\n"
        )?;
        Ok(())
    }

    /// Unbox/box for a heap value the runtime boxes with a helper.
    fn boxed_heap(
        &self,
        ty: &IrType,
        i: usize,
        unbox: &mut String,
        boxed: &mut String,
    ) -> Result<(), CodegenError> {
        let (b, u) = heap_box(ty).unwrap_or(("(IrisVal*)", ""));
        let unwrap = if u.is_empty() {
            "(void*)v".to_owned()
        } else {
            format!("{u}(v)")
        };
        writeln!(
            unbox,
            "    if (!v) Py_RETURN_NONE;\n    return iris_to_py_{i}({unwrap});"
        )?;
        writeln!(
            boxed,
            "    void* x;\n    if (!iris_from_py_{i}(o, &x)) return NULL;\n    return {b}(x);"
        )?;
        Ok(())
    }
}

/// Tensors: nested lists by shape, and back from anything list-like (or
/// with `tolist()`, as numpy arrays have).
const TENSOR_HELPERS: &str = r#"
static PyObject* iris_py_tensor_nest(IrisTensor* t, int32_t dim, int64_t* at) {
    if (t->ndim == 0) return PyFloat_FromDouble(t->numel > 0 ? (double)t->data[0] : 0.0);
    int64_t n = t->shape[dim];
    PyObject* r = PyList_New((Py_ssize_t)n);
    if (!r) return NULL;
    for (int64_t k = 0; k < n; k++) {
        PyObject* x = dim + 1 == t->ndim ? PyFloat_FromDouble((double)t->data[(*at)++]) : iris_py_tensor_nest(t, dim + 1, at);
        if (!x) { Py_DECREF(r); return NULL; }
        PyList_SET_ITEM(r, (Py_ssize_t)k, x);
    }
    return r;
}

static int iris_py_tensor_walk(PyObject* o, int32_t dim, IrisList* shape, IrisList* data) {
    if (PyList_Check(o) || PyTuple_Check(o)) {
        Py_ssize_t n = PySequence_Fast_GET_SIZE(o);
        if (dim == (int32_t)iris_list_len(shape)) {
            iris_list_push(shape, iris_box_i64((int64_t)n));
        } else if (dim > (int32_t)iris_list_len(shape) || iris_list_get(shape, dim)->i64 != (int64_t)n) {
            PyErr_SetString(PyExc_ValueError, "a tensor's nested lists must be rectangular");
            return 0;
        }
        PyObject** items = PySequence_Fast_ITEMS(o);
        for (Py_ssize_t k = 0; k < n; k++)
            if (!iris_py_tensor_walk(items[k], dim + 1, shape, data)) return 0;
        return 1;
    }
    if (dim != (int32_t)iris_list_len(shape)) {
        PyErr_SetString(PyExc_ValueError, "a tensor's nested lists must be rectangular");
        return 0;
    }
    double x = PyFloat_AsDouble(o);
    if (x == -1.0 && PyErr_Occurred()) return 0;
    iris_list_push(data, iris_box_f64(x));
    return 1;
}

static IrisTensor* iris_py_tensor_from(PyObject* o) {
    PyObject* owned = NULL;
    if (!PyList_Check(o) && !PyTuple_Check(o) && PyObject_HasAttrString(o, "tolist")) {
        owned = PyObject_CallMethod(o, "tolist", NULL);
        if (!owned) return NULL;
        o = owned;
    }
    if (!PyList_Check(o) && !PyTuple_Check(o)) {
        Py_XDECREF(owned);
        PyErr_SetString(PyExc_TypeError, "expected a tensor: nested lists of numbers");
        return NULL;
    }
    IrisList* shape = iris_list_new();
    IrisList* data = iris_list_new();
    int ok = iris_py_tensor_walk(o, 0, shape, data);
    Py_XDECREF(owned);
    if (!ok) return NULL;
    IrisTensor* t = iris_tensor_from_lists(data, shape);
    if (!t) PyErr_SetString(PyExc_ValueError, "an empty tensor");
    return t;
}
"#;

/// The C source of the extension module `name`. `apple`: symbols carry a
/// leading underscore.
pub fn emit_python_ext_named(
    module: &IrModule,
    name: &str,
    apple: bool,
) -> Result<String, CodegenError> {
    let fns = exported(module);
    let mut gen = Gen {
        module,
        types: Vec::new(),
        decls: String::new(),
        defs: String::new(),
    };
    let mut sigs = Vec::new();
    for f in &fns {
        let params: Vec<usize> = f
            .params
            .iter()
            .map(|p| gen.id(&p.ty))
            .collect::<Result<_, _>>()?;
        let ret = if is_unit(&f.return_ty) {
            None
        } else {
            Some(gen.id(&f.return_ty)?)
        };
        sigs.push((params, ret));
    }

    let mut out = String::new();
    writeln!(
        out,
        "/* Generated by the IRIS compiler: the CPython extension module `{name}`. */"
    )?;
    writeln!(out, "#define PY_SSIZE_T_CLEAN")?;
    writeln!(out, "#include <Python.h>")?;
    writeln!(out, "#include <stdint.h>")?;
    writeln!(out, "#include <stdlib.h>")?;
    writeln!(out, "#include <string.h>")?;
    writeln!(out, "#include \"iris_runtime.h\"\n")?;
    writeln!(
        out,
        "/* `{name}.IrisError`: a runtime error inside IRIS code, or an `err` result. */"
    )?;
    writeln!(out, "static PyObject* IrisError;\n")?;

    writeln!(out, "/* ── The IRIS functions (namespaced symbols) ── */")?;
    let prefix = if apple { "_" } else { "" };
    for f in &fns {
        let ret = if is_unit(&f.return_ty) {
            "void"
        } else {
            ctype(&f.return_ty)
        };
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| format!("{} {}", ctype(&p.ty), c_ident(&p.name)))
            .collect();
        let params = if params.is_empty() {
            "void".to_owned()
        } else {
            params.join(", ")
        };
        writeln!(
            out,
            "extern {ret} iris_fn_{n}({params}) __asm__(\"{prefix}{sym}{orig}\");",
            n = f.name,
            sym = crate::codegen::llvm_ir::USER_SYMBOL_PREFIX,
            orig = f.name
        )?;
    }
    writeln!(out)?;

    writeln!(out, "/* ── Conversions ── */")?;
    out.push_str(&gen.decls);
    writeln!(
        out,
        "static PyObject* iris_py_tensor_nest(IrisTensor* t, int32_t dim, int64_t* at);"
    )?;
    writeln!(out, "static IrisTensor* iris_py_tensor_from(PyObject* o);")?;
    out.push_str(TENSOR_HELPERS);
    writeln!(out)?;
    out.push_str(&gen.defs);

    writeln!(out, "/* ── CPython Method Wrappers ── */")?;
    for (f, (params, ret)) in fns.iter().zip(&sigs) {
        let n = params.len();
        writeln!(
            out,
            "static PyObject* py_{}(PyObject* self, PyObject* args) {{",
            f.name
        )?;
        writeln!(out, "    (void)self;")?;
        writeln!(
            out,
            "    if (PyTuple_GET_SIZE(args) != {n}) {{\n        PyErr_Format(PyExc_TypeError, \"{}() takes {n} argument{} (%zd given)\", PyTuple_GET_SIZE(args));\n        return NULL;\n    }}",
            f.name,
            if n == 1 { "" } else { "s" }
        )?;
        let mut call_args = Vec::new();
        for (k, (p, id)) in f.params.iter().zip(params).enumerate() {
            writeln!(out, "    {} arg_{k};", ctype(&p.ty))?;
            writeln!(
                out,
                "    if (!iris_from_py_{id}(PyTuple_GET_ITEM(args, {k}), &arg_{k})) {{\n        PyObject *t, *v, *tb;\n        PyErr_Fetch(&t, &v, &tb);\n        PyErr_Format(t ? t : PyExc_TypeError, \"{}(): argument `{}`: %S\", v ? v : Py_None);\n        Py_XDECREF(t); Py_XDECREF(v); Py_XDECREF(tb);\n        return NULL;\n    }}",
                f.name, p.name
            )?;
            call_args.push(format!("arg_{k}"));
        }
        // A runtime error comes back here, with the GIL released.
        // `volatile`: the trap's return restores only the stack and frame
        // pointers, so what it reads must be in memory, not a register.
        writeln!(out, "    IrisErrorTrap trap;\n    iris_error_trap_push(&trap);\n    PyThreadState* volatile ts = PyEval_SaveThread();")?;
        writeln!(out, "    if (IRIS_TRAP_SET(&trap)) {{\n        PyEval_RestoreThread(ts);\n        iris_error_trap_pop(&trap);\n        PyErr_SetString(IrisError, trap.message);\n        return NULL;\n    }}")?;
        match ret {
            None => {
                writeln!(out, "    iris_fn_{}({});", f.name, call_args.join(", "))?;
                writeln!(out, "    PyEval_RestoreThread(ts);\n    iris_error_trap_pop(&trap);\n    Py_RETURN_NONE;")?;
            }
            Some(id) => {
                writeln!(
                    out,
                    "    {} result = iris_fn_{}({});",
                    ctype(&f.return_ty),
                    f.name,
                    call_args.join(", ")
                )?;
                writeln!(out, "    PyEval_RestoreThread(ts);\n    iris_error_trap_pop(&trap);\n    return iris_to_py_{id}(result);")?;
            }
        }
        writeln!(out, "}}\n")?;
    }

    writeln!(out, "/* ── Module Method Table ── */")?;
    writeln!(out, "static PyMethodDef {name}Methods[] = {{")?;
    for f in &fns {
        let sig: Vec<String> = f
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty))
            .collect();
        writeln!(
            out,
            "    {{\"{n}\", py_{n}, METH_VARARGS, \"{n}({}) -> {}\"}},",
            sig.join(", ").replace('"', "'"),
            f.return_ty.to_string().replace('"', "'"),
            n = f.name
        )?;
    }
    writeln!(out, "    {{NULL, NULL, 0, NULL}}")?;
    writeln!(out, "}};\n")?;

    writeln!(out, "/* ── Module Definition ── */")?;
    writeln!(out, "static struct PyModuleDef {name}_module = {{")?;
    writeln!(out, "    PyModuleDef_HEAD_INIT,")?;
    writeln!(out, "    \"{name}\",")?;
    writeln!(out, "    \"IRIS functions, compiled to native code.\",")?;
    writeln!(out, "    -1,")?;
    writeln!(out, "    {name}Methods")?;
    writeln!(out, "}};\n")?;

    writeln!(out, "/* ── Module Initialization Entry Point ── */")?;
    writeln!(out, "PyMODINIT_FUNC PyInit_{name}(void) {{")?;
    writeln!(out, "    PyObject* m = PyModule_Create(&{name}_module);")?;
    writeln!(out, "    if (!m) return NULL;")?;
    writeln!(
        out,
        "    IrisError = PyErr_NewException(\"{name}.IrisError\", PyExc_RuntimeError, NULL);"
    )?;
    writeln!(out, "    if (!IrisError || PyModule_AddObjectRef(m, \"IrisError\", IrisError) < 0) {{\n        Py_DECREF(m);\n        return NULL;\n    }}")?;
    writeln!(out, "    return m;")?;
    writeln!(out, "}}")?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Building the module
// ---------------------------------------------------------------------------

/// What a Python installation needs for building an extension.
struct Python {
    include: PathBuf,
    /// The import library to link on Windows (`python3XY.lib`).
    import_lib: Option<PathBuf>,
    ext_suffix: String,
}

fn find_python() -> Result<Python, CodegenError> {
    // On Windows `python` is the user's own interpreter; `python3` is often
    // MSYS2's.
    let candidates: Vec<String> = match std::env::var("IRIS_PYTHON") {
        Ok(p) => vec![p],
        Err(_) if cfg!(windows) => vec!["python".into(), "python3".into()],
        Err(_) => vec!["python3".into(), "python".into()],
    };
    // The import library: python.org's `libs\pythonXY.lib`, or a MinGW
    // build's `libpythonX.Y.dll.a`.
    let script = "import sysconfig, sys, os\n\
                  print(sysconfig.get_paths()['include'])\n\
                  print(sysconfig.get_config_var('EXT_SUFFIX') or '')\n\
                  lib = os.path.join(sys.base_prefix, 'libs', 'python%d%d.lib' % sys.version_info[:2])\n\
                  if not os.path.exists(lib) and os.name == 'nt':\n\
                  \x20   lib = os.path.join(sysconfig.get_config_var('LIBDIR') or '', sysconfig.get_config_var('LDLIBRARY') or '')\n\
                  print(lib if os.path.isfile(lib) else '')";
    let mut tried = Vec::new();
    for py in candidates {
        let Ok(out) = Command::new(&py).args(["-c", script]).output() else {
            tried.push(format!("{py}: not found"));
            continue;
        };
        if !out.status.success() {
            tried.push(format!(
                "{py}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            continue;
        }
        let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        let mut lines = text.lines();
        let include = PathBuf::from(lines.next().unwrap_or(""));
        let ext_suffix = lines.next().unwrap_or("").to_owned();
        let lib = lines.next().unwrap_or("");
        if !include.join("Python.h").exists() {
            tried.push(format!(
                "{py}: no Python.h in {} (install the Python development headers)",
                include.display()
            ));
            continue;
        }
        return Ok(Python {
            include,
            import_lib: (!lib.is_empty()).then(|| PathBuf::from(lib)),
            ext_suffix,
        });
    }
    Err(CodegenError::ToolchainMissing {
        detail: format!("a Python with its development headers is needed to build an extension ({}); set IRIS_PYTHON to choose one", tried.join("; ")),
    })
}

/// `--emit python-ext -o NAME.pyd` (`.so` elsewhere): compile the module, its
/// glue and the runtime into an extension Python imports as `NAME` -- the
/// output's name up to its first dot.
pub fn build_python_ext(module: &IrModule, output: &Path) -> Result<PathBuf, CodegenError> {
    let python = find_python()?;
    let file = output
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("iris_ext");
    let name = c_ident(file.split('.').next().unwrap_or("iris_ext"));
    let target = crate::codegen::build::resolve_target_triple(None);
    let apple = target.contains("apple");
    let glue = emit_python_ext_named(module, &name, apple)?;
    let ir = crate::codegen::llvm_ir::emit_llvm_ir_for_library(module)?;
    let mut link: Vec<String> = Vec::new();
    if let Some(lib) = &python.import_lib {
        link.push(lib.display().to_string());
    } else if apple {
        // Symbols resolve against the interpreter that loads the module.
        link.extend(["-undefined".into(), "dynamic_lookup".into()]);
    }
    let defines = if target.contains("windows") {
        vec!["-DMS_WIN64".to_owned()]
    } else {
        Vec::new()
    };
    let built = crate::codegen::build::build_shared_library(
        &ir,
        &[("iris_python_glue.c", glue.as_str())],
        std::slice::from_ref(&python.include),
        &defines,
        &link,
        output,
    )?;
    let _ = python.ext_suffix;
    Ok(built)
}
