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

    // The IRIS functions, under the symbols a library build gives them.
    let sym = |s: &str| {
        if cfg!(target_os = "macos") {
            format!("_iris.{s}")
        } else {
            format!("iris.{s}")
        }
    };
    assert!(c_code.contains(&format!(
        "extern int64_t iris_fn_fast_add(int64_t a, int64_t b) __asm__(\"{}\");",
        sym("fast_add")
    )));
    assert!(c_code.contains(&format!(
        "extern double iris_fn_scale_factor(double x, double factor) __asm__(\"{}\");",
        sym("scale_factor")
    )));
    // `bool` is an `i1`: read as a byte, its low bit.
    assert!(c_code.contains(&format!(
        "extern uint8_t iris_fn_is_positive(int64_t num) __asm__(\"{}\");",
        sym("is_positive")
    )));

    // Wrapper functions: arguments converted (range-checked), the call made
    // with the GIL released and a runtime error trapped, the result converted.
    assert!(c_code.contains("static PyObject* py_fast_add(PyObject* self, PyObject* args)"));
    assert!(c_code.contains("fast_add() takes 2 arguments (%zd given)"));
    assert!(c_code.contains("PyLong_FromLongLong((long long)v)"));
    assert!(c_code.contains("int64_t result = iris_fn_fast_add(arg_0, arg_1);"));
    assert!(c_code.contains("PyEval_SaveThread()"));
    assert!(c_code.contains("if (IRIS_TRAP_SET(&trap))"));
    assert!(c_code.contains("PyErr_SetString(IrisError, trap.message);"));

    assert!(c_code.contains("static PyObject* py_scale_factor(PyObject* self, PyObject* args)"));
    assert!(c_code.contains("PyFloat_FromDouble((double)v)"));

    assert!(c_code.contains("static PyObject* py_is_positive(PyObject* self, PyObject* args)"));
    assert!(c_code.contains("PyBool_FromLong(v & 1)"));

    // Check PyMethodDef table
    assert!(c_code.contains("static PyMethodDef fast_mathMethods[] = {"));
    assert!(c_code.contains("{\"fast_add\", py_fast_add, METH_VARARGS,"));
    assert!(c_code.contains("{\"scale_factor\", py_scale_factor, METH_VARARGS,"));
    assert!(c_code.contains("{\"is_positive\", py_is_positive, METH_VARARGS,"));
    assert!(c_code.contains("{NULL, NULL, 0, NULL}"));

    // Check PyModuleDef struct & PyInit entry point
    assert!(c_code.contains("static struct PyModuleDef fast_math_module = {"));
    assert!(c_code.contains("PyMODINIT_FUNC PyInit_fast_math(void) {"));
    assert!(c_code.contains("PyObject* m = PyModule_Create(&fast_math_module);"));
    assert!(
        c_code.contains("PyErr_NewException(\"fast_math.IrisError\", PyExc_RuntimeError, NULL)")
    );
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
