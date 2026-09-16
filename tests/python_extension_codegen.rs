use iris::codegen::emit_python_ext;
use iris::{compile, compile_to_module, EmitKind};

#[test]
fn test_python_ext_codegen_via_lib_compile() {
    let src = r#"
def fast_add(a: i64, b: i64) -> i64 {
    return a + b
}

def scale_factor(x: f64, factor: f64) -> f64 {
    return x * factor
}

def is_positive(num: i64) -> bool {
    return num > 0
}
"#;

    let c_code = compile(src, "fast_math", EmitKind::PythonExt).expect("compile python-ext");

    // Check headers & preamble
    assert!(
        c_code.contains("#include <Python.h>"),
        "must include Python.h"
    );
    assert!(
        c_code.contains("#define PY_SSIZE_T_CLEAN"),
        "must define PY_SSIZE_T_CLEAN"
    );

    // Check extern declarations
    assert!(c_code.contains("extern int64_t fast_add(int64_t a, int64_t b);"));
    assert!(c_code.contains("extern double scale_factor(double x, double factor);"));
    assert!(c_code.contains("extern bool is_positive(int64_t num);"));

    // Check wrapper functions
    assert!(c_code.contains("static PyObject* py_fast_add(PyObject* self, PyObject* args)"));
    assert!(c_code.contains("PyArg_ParseTuple(args, \"LL\", &arg_0, &arg_1)"));
    assert!(c_code.contains("PyLong_FromLongLong((long long)result)"));

    assert!(c_code.contains("static PyObject* py_scale_factor(PyObject* self, PyObject* args)"));
    assert!(c_code.contains("PyArg_ParseTuple(args, \"dd\", &arg_0, &arg_1)"));
    assert!(c_code.contains("PyFloat_FromDouble((double)result)"));

    assert!(c_code.contains("static PyObject* py_is_positive(PyObject* self, PyObject* args)"));
    assert!(c_code.contains("PyArg_ParseTuple(args, \"L\", &arg_0)"));
    assert!(c_code.contains("PyBool_FromLong(result ? 1 : 0)"));

    // Check PyMethodDef table
    assert!(c_code.contains("static PyMethodDef fast_mathMethods[] = {"));
    assert!(c_code.contains("{\"fast_add\", py_fast_add, METH_VARARGS,"));
    assert!(c_code.contains("{\"scale_factor\", py_scale_factor, METH_VARARGS,"));
    assert!(c_code.contains("{\"is_positive\", py_is_positive, METH_VARARGS,"));
    assert!(c_code.contains("{NULL, NULL, 0, NULL}"));

    // Check PyModuleDef struct & PyInit entry point
    assert!(c_code.contains("static struct PyModuleDef fast_math_module = {"));
    assert!(c_code.contains("PyMODINIT_FUNC PyInit_fast_math(void) {"));
    assert!(c_code.contains("return PyModule_Create(&fast_math_module);"));
}

#[test]
fn test_python_ext_codegen_direct() {
    let src = r#"
def noop_work() -> () {
    let x = 10
}
"#;

    let module = compile_to_module(src, "telemetry_ext").expect("compile to module");
    let c_code = emit_python_ext(&module).expect("emit python ext");

    assert!(c_code.contains("PyMODINIT_FUNC PyInit_telemetry_ext(void)"));
    assert!(c_code.contains("static PyObject* py_noop_work(PyObject* self, PyObject* args)"));
    assert!(c_code.contains("Py_RETURN_NONE;"));
}
