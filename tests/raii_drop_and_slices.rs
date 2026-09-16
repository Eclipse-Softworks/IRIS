use iris::compile_to_module;
use iris::ir::instr::IrInstr;
use iris::ir::types::IrType;
use iris::parser::ast::{AstScalarKind, AstType};
use iris::parser::lexer::Lexer;
use iris::parser::parse::Parser;

#[test]
fn test_raii_drop_destructor_inserted_on_exit() {
    let src = r#"
record Resource {
    id: i64,
}

trait Drop {
    def drop(r: Resource) -> i64
}

impl Drop for Resource {
    def drop(r: Resource) -> i64 {
        return 0
    }
}

def worker() -> i64 {
    val r = Resource { id: 42 }
    return 100
}
"#;

    let module = compile_to_module(src, "test_raii").expect("compile to IR");
    let worker_fn = module
        .functions()
        .iter()
        .find(|f| f.name == "worker")
        .expect("find worker function");

    // Verify that worker_fn contains a Call to Drop__Resource__drop before Return
    let has_drop_call = worker_fn.blocks().iter().any(|b| {
        b.instrs.iter().any(|instr| {
            matches!(instr, IrInstr::Call { callee, .. } if callee.contains("Drop__Resource__drop") || callee.contains("Resource__drop"))
        })
    });

    assert!(
        has_drop_call,
        "worker function must contain automated destructor call for Resource implementing Drop"
    );
}

#[test]
fn test_raii_drop_moved_return_does_not_drop() {
    let src = r#"
record Resource {
    id: i64,
}

trait Drop {
    def drop(r: Resource) -> i64
}

impl Drop for Resource {
    def drop(r: Resource) -> i64 {
        return 0
    }
}

def transfer(r: Resource) -> Resource {
    return r
}
"#;

    let module = compile_to_module(src, "test_raii_move").expect("compile to IR");
    let transfer_fn = module
        .functions()
        .iter()
        .find(|f| f.name == "transfer")
        .expect("find transfer function");

    // In transfer, the returned value 'r' is moved, so no destructor call should be emitted
    let has_drop_call = transfer_fn.blocks().iter().any(|b| {
        b.instrs
            .iter()
            .any(|instr| matches!(instr, IrInstr::Call { callee, .. } if callee.contains("drop")))
    });

    assert!(
        !has_drop_call,
        "moved return value must NOT be dropped at function exit"
    );
}

#[test]
fn test_slice_views_ast_and_ir() {
    let src = r#"
def take_slice(s: &[f64]) -> i64 {
    return 0
}

def mutate_slice(s: &mut [i64]) -> i64 {
    return 0
}
"#;

    let tokens = Lexer::new(src).tokenize().expect("tokenize");
    let mut parser = Parser::new(&tokens);
    let (ast, errors) = parser.parse_module_recovering();
    assert!(errors.is_empty(), "parse errors: {:?}", errors);

    // Verify take_slice has &[f64] (Ref to Slice)
    let p0 = &ast.functions[0].params[0].ty;
    match p0 {
        AstType::Ref(inner, _) => match **inner {
            AstType::Slice(ref elem, _) => match **elem {
                AstType::Scalar(AstScalarKind::F64, _) => {}
                ref other => panic!("expected f64, got {:?}", other),
            },
            ref other => panic!("expected Slice, got {:?}", other),
        },
        ref other => panic!("expected Ref, got {:?}", other),
    }

    // Lower and verify IrType::Slice
    let module = compile_to_module(src, "test_slice").expect("lower to IR");
    let f0 = &module.functions()[0];
    assert!(
        matches!(&f0.params[0].ty, IrType::Slice { elem, is_mut: false } if elem.as_ref() == &IrType::Scalar(iris::ir::types::DType::F64))
    );

    let f1 = &module.functions()[1];
    assert!(
        matches!(&f1.params[0].ty, IrType::Slice { elem, is_mut: true } if elem.as_ref() == &IrType::Scalar(iris::ir::types::DType::I64))
    );
}

#[test]
fn test_ctfe_evaluated_array_lengths() {
    let src = r#"
record Cache {
    buffer: [i64; 4 * 16 + 8],
}
"#;

    let module = compile_to_module(src, "test_ctfe_array").expect("compile to IR");
    let fields = module.struct_def("Cache").expect("Cache struct def");
    assert_eq!(fields.len(), 1);
    match &fields[0].1 {
        IrType::Array { len, .. } => assert_eq!(*len, 72), // 4 * 16 + 8 = 72
        other => panic!("expected Array type, got {:?}", other),
    }
}

#[test]
fn test_slice_load_and_store_indexing() {
    let src = r#"
def sum_slice(s: &[i64], i: i64) -> i64 {
    return s[i]
}

def update_slice(s: &mut [i64], i: i64, v: i64) -> i64 {
    s[i] = v
    return 0
}
"#;

    let module = compile_to_module(src, "test_slice_index").expect("compile to module");
    let f0 = module
        .functions()
        .iter()
        .find(|f| f.name == "sum_slice")
        .expect("find sum_slice");
    let has_array_load = f0.blocks().iter().any(|b| {
        b.instrs
            .iter()
            .any(|i| matches!(i, IrInstr::ArrayLoad { .. }))
    });
    assert!(
        has_array_load,
        "sum_slice must contain IrInstr::ArrayLoad for slice index"
    );

    let f1 = module
        .functions()
        .iter()
        .find(|f| f.name == "update_slice")
        .expect("find update_slice");
    let has_array_store = f1.blocks().iter().any(|b| {
        b.instrs
            .iter()
            .any(|i| matches!(i, IrInstr::ArrayStore { .. }))
    });
    assert!(
        has_array_store,
        "update_slice must contain IrInstr::ArrayStore for slice index assignment"
    );
}

#[test]
fn test_stdlib_slice_utilities_in_ir() {
    let src = r#"
def slice_sum(s: &[i64], len: i64) -> i64 {
    var acc = 0;
    var i = 0;
    while i < len {
        acc = acc + s[i];
        i = i + 1;
    }
    return acc
}

def slice_fill(s: &mut [i64], len: i64, value: i64) -> i64 {
    var i = 0;
    while i < len {
        s[i] = value;
        i = i + 1;
    }
    return 0
}
"#;

    let module = compile_to_module(src, "test_slice_utils").expect("compile to module");
    let f_sum = module
        .functions()
        .iter()
        .find(|f| f.name == "slice_sum")
        .unwrap();
    let has_load = f_sum.blocks().iter().any(|b| {
        b.instrs
            .iter()
            .any(|i| matches!(i, IrInstr::ArrayLoad { .. }))
    });
    assert!(has_load, "slice_sum must contain native IrInstr::ArrayLoad");

    let f_fill = module
        .functions()
        .iter()
        .find(|f| f.name == "slice_fill")
        .unwrap();
    let has_store = f_fill.blocks().iter().any(|b| {
        b.instrs
            .iter()
            .any(|i| matches!(i, IrInstr::ArrayStore { .. }))
    });
    assert!(
        has_store,
        "slice_fill must contain native IrInstr::ArrayStore"
    );
}
