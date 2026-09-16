use iris::evolution::compile_candidate;
use iris::evolution::genome::{
    GeneNode, GeneType, GeneValue, GenomeState, VecBinaryOp, VecUnaryOp,
};
use std::collections::HashMap;

#[test]
fn test_vector_operations_evaluation() {
    let mut state = GenomeState::default();
    let env = HashMap::new();

    // 1. Vector addition: [1, 2, 3, 4] + [10, 20, 30, 40] = [11, 22, 33, 44]
    let vec_add = GeneNode::VecBinary {
        op: VecBinaryOp::Add,
        lhs: Box::new(GeneNode::ConstVec([1, 2, 3, 4])),
        rhs: Box::new(GeneNode::ConstVec([10, 20, 30, 40])),
    };
    assert_eq!(vec_add.gene_type(), GeneType::Vec4);
    assert_eq!(
        vec_add.evaluate_value(&env, &mut state).unwrap(),
        GeneValue::Vec4([11, 22, 33, 44])
    );

    // 2. Vector dot product: [2, 3, 4, 5] . [1, 2, 3, 4] = 2 + 6 + 12 + 20 = 40
    let vec_dot = GeneNode::VecBinary {
        op: VecBinaryOp::Dot,
        lhs: Box::new(GeneNode::ConstVec([2, 3, 4, 5])),
        rhs: Box::new(GeneNode::ConstVec([1, 2, 3, 4])),
    };
    assert_eq!(vec_dot.gene_type(), GeneType::I64);
    assert_eq!(
        vec_dot.evaluate_value(&env, &mut state).unwrap(),
        GeneValue::I64(40)
    );

    // 3. Vector ArgMax: [10, 80, 25, 40] -> index 1
    let vec_argmax = GeneNode::VecUnary {
        op: VecUnaryOp::ArgMax,
        child: Box::new(GeneNode::ConstVec([10, 80, 25, 40])),
    };
    assert_eq!(vec_argmax.gene_type(), GeneType::I64);
    assert_eq!(
        vec_argmax.evaluate_value(&env, &mut state).unwrap(),
        GeneValue::I64(1)
    );

    // 4. Vector Sum: [5, 10, 15, 20] -> 50
    let vec_sum = GeneNode::VecUnary {
        op: VecUnaryOp::Sum,
        child: Box::new(GeneNode::ConstVec([5, 10, 15, 20])),
    };
    assert_eq!(
        vec_sum.evaluate_value(&env, &mut state).unwrap(),
        GeneValue::I64(50)
    );

    // 5. Vector scale: [1, -2, 3, -4] * 3 = [3, -6, 9, -12]
    let vec_scale = GeneNode::VecBinary {
        op: VecBinaryOp::Scale,
        lhs: Box::new(GeneNode::ConstVec([1, -2, 3, -4])),
        rhs: Box::new(GeneNode::ConstI64(3)),
    };
    assert_eq!(
        vec_scale.evaluate_value(&env, &mut state).unwrap(),
        GeneValue::Vec4([3, -6, 9, -12])
    );

    // 6. Vector element extract: [10, 20, 30, 40][2] = 30
    let vec_extract = GeneNode::VecExtract {
        vec: Box::new(GeneNode::ConstVec([10, 20, 30, 40])),
        index: 2,
    };
    assert_eq!(vec_extract.gene_type(), GeneType::I64);
    assert_eq!(
        vec_extract.evaluate_value(&env, &mut state).unwrap(),
        GeneValue::I64(30)
    );

    // 7. Vector pack: pack 4 scalars
    let vec_pack = GeneNode::VecPack([
        Box::new(GeneNode::ConstI64(7)),
        Box::new(GeneNode::ConstI64(14)),
        Box::new(GeneNode::ConstI64(21)),
        Box::new(GeneNode::ConstI64(28)),
    ]);
    assert_eq!(vec_pack.gene_type(), GeneType::Vec4);
    assert_eq!(
        vec_pack.evaluate_value(&env, &mut state).unwrap(),
        GeneValue::Vec4([7, 14, 21, 28])
    );
}

#[test]
fn test_persistent_state_and_accumulators() {
    let mut state = GenomeState::default();
    let env = HashMap::new();

    // 1. Scalar state write & read
    let write_node = GeneNode::StateWrite {
        reg: 3,
        val: Box::new(GeneNode::ConstI64(42)),
    };
    let read_node = GeneNode::StateRead(3);

    let _ = write_node.evaluate_value(&env, &mut state).unwrap();
    assert_eq!(state.scalars[3], 42);
    assert_eq!(
        read_node.evaluate_value(&env, &mut state).unwrap(),
        GeneValue::I64(42)
    );

    // 2. Vector state write & read
    let v_write = GeneNode::StateVecWrite {
        reg: 1,
        val: Box::new(GeneNode::ConstVec([100, 200, 300, 400])),
    };
    let v_read = GeneNode::StateVecRead(1);

    let _ = v_write.evaluate_value(&env, &mut state).unwrap();
    assert_eq!(state.vectors[1], [100, 200, 300, 400]);
    assert_eq!(
        v_read.evaluate_value(&env, &mut state).unwrap(),
        GeneValue::Vec4([100, 200, 300, 400])
    );

    // 3. State accumulator (exponential moving average: new = old + (val - old)/factor)
    // Initial R2 = 0
    let accum = GeneNode::StateAccum {
        reg: 2,
        val: Box::new(GeneNode::ConstI64(100)),
        factor: 2,
    };
    // Step 1: 0 + (100 - 0)/2 = 50
    let _ = accum.evaluate_value(&env, &mut state).unwrap();
    assert_eq!(state.scalars[2], 50);

    // Step 2: 50 + (100 - 50)/2 = 75
    let _ = accum.evaluate_value(&env, &mut state).unwrap();
    assert_eq!(state.scalars[2], 75);

    // Step 3: 75 + (100 - 75)/2 = 87
    let _ = accum.evaluate_value(&env, &mut state).unwrap();
    assert_eq!(state.scalars[2], 87);
}

#[test]
fn test_vector_evaluate_vec4_sensory_dispatch() {
    let mut state = GenomeState::default();

    // Neuro-symbolic policy: dot_product(sensors, weights) -> action argmax
    // sensors = [temp_delta, hazard, energy_deficit, health_deficit]
    // weights = [1, 2, -1, 4]
    let genome = GeneNode::VecBinary {
        op: VecBinaryOp::Add,
        lhs: Box::new(GeneNode::Var("input_vec".to_string(), GeneType::Vec4)),
        rhs: Box::new(GeneNode::ConstVec([0, 10, 50, 5])),
    };

    let sensory_input = [15, 20, 10, 0];
    let (out_vec, action) = genome.evaluate_vec4(&sensory_input, &mut state).unwrap();

    // Expected: [15+0, 20+10, 10+50, 0+5] = [15, 30, 60, 5]
    assert_eq!(out_vec, [15, 30, 60, 5]);
    // ArgMax of [15, 30, 60, 5] is index 2 (value 60)
    assert_eq!(action, 2);
}

#[test]
fn test_vector_iris_code_generation_compiles() {
    let genome = GeneNode::VecUnary {
        op: VecUnaryOp::ArgMax,
        child: Box::new(GeneNode::VecBinary {
            op: VecBinaryOp::Add,
            lhs: Box::new(GeneNode::ConstVec([10, 20, 30, 40])),
            rhs: Box::new(GeneNode::ConstVec([4, 3, 2, 1])),
        }),
    };

    assert!(genome.uses_vectors());
    let source = genome.to_iris_source("policy", "input");

    assert!(source.contains("def vec4_add("));
    assert!(source.contains("def vec4_argmax("));
    assert!(source.contains("def policy(input: i64) -> i64 effect alloc {"));

    // Verify source compiles through IRIS compiler
    let module = compile_candidate(&source, "test_vector_module");
    assert!(
        module.is_ok(),
        "Emitted vector IRIS source failed to compile: {:?}",
        module.err()
    );
}

#[test]
fn test_python_vector_code_generation() {
    let genome = GeneNode::VecBinary {
        op: VecBinaryOp::Dot,
        lhs: Box::new(GeneNode::ConstVec([1, 2, 3, 4])),
        rhs: Box::new(GeneNode::ConstVec([10, 20, 30, 40])),
    };

    let py_expr = genome.to_python_expr();
    assert_eq!(
        py_expr,
        "sum(_a * _b for _a, _b in zip([1, 2, 3, 4], [10, 20, 30, 40]))"
    );

    let py_lambda = genome.to_python_lambda("x");
    assert!(py_lambda.contains("_vr="));
}
