use std::ffi::CStr;
use std::ptr;

use iris::evolution::ffi::{
    iris_evolution_search_add_case, iris_evolution_search_create, iris_evolution_search_free,
    iris_evolution_search_run, iris_genome_eval, iris_genome_free, iris_genome_to_iris_expr,
    iris_genome_to_python_expr, iris_genome_to_python_func, iris_string_free,
};

#[test]
fn test_ffi_search_and_polyglot_emission() {
    unsafe {
        let session = iris_evolution_search_create(30, 20, 0.35, 0.7, 0.01, 12345);
        assert!(!session.is_null());

        // Target: policy(x) = x (identity)
        iris_evolution_search_add_case(session, 0, 0, 1.0);
        iris_evolution_search_add_case(session, 1, 1, 1.0);
        iris_evolution_search_add_case(session, 5, 5, 1.0);
        iris_evolution_search_add_case(session, -3, -3, 1.0);

        let genome = iris_evolution_search_run(session);
        assert!(!genome.is_null());

        // Direct C-ABI evaluation
        let val0 = iris_genome_eval(genome, 0);
        let val1 = iris_genome_eval(genome, 1);
        let val5 = iris_genome_eval(genome, 5);
        let val_m3 = iris_genome_eval(genome, -3);

        assert_eq!(val0, 0);
        assert_eq!(val1, 1);
        assert_eq!(val5, 5);
        assert_eq!(val_m3, -3);

        // IRIS expression string
        let iris_str_ptr = iris_genome_to_iris_expr(genome);
        assert!(!iris_str_ptr.is_null());
        let iris_str = CStr::from_ptr(iris_str_ptr).to_str().unwrap();
        assert!(!iris_str.is_empty());
        iris_string_free(iris_str_ptr);

        // Python expression string
        let py_expr_ptr = iris_genome_to_python_expr(genome);
        assert!(!py_expr_ptr.is_null());
        let py_expr = CStr::from_ptr(py_expr_ptr).to_str().unwrap();
        assert!(!py_expr.is_empty());
        iris_string_free(py_expr_ptr);

        // Python function definition
        let py_func_ptr = iris_genome_to_python_func(genome, ptr::null(), ptr::null());
        assert!(!py_func_ptr.is_null());
        let py_func = CStr::from_ptr(py_func_ptr).to_str().unwrap();
        assert!(py_func.contains("def policy(x: int) -> int:"));
        assert!(py_func.contains("return "));
        iris_string_free(py_func_ptr);

        iris_genome_free(genome);
        iris_evolution_search_free(session);
    }
}
