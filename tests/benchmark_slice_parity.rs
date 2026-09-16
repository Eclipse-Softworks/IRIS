use iris::compile_to_module;
use iris::ir::instr::IrInstr;

#[test]
fn test_quicksort_slice_parity() {
    let src = r#"
def partition_slice(arr: &mut [i64], low: i64, high: i64) -> i64 {
    val pivot = arr[high];
    var i = low - 1;
    var j = low;
    while j < high {
        if arr[j] <= pivot {
            i = i + 1;
            val temp = arr[i];
            arr[i] = arr[j];
            arr[j] = temp;
        } else { 0 };
        j = j + 1;
    }
    val temp = arr[i + 1];
    arr[i + 1] = arr[high];
    arr[high] = temp;
    return i + 1
}

def quicksort_slice(arr: &mut [i64], low: i64, high: i64) -> i64 {
    if low < high {
        val pi = partition_slice(arr, low, high);
        quicksort_slice(arr, low, pi - 1);
        quicksort_slice(arr, pi + 1, high);
        return 0
    } else {
        return 0
    }
}
"#;

    let module = compile_to_module(src, "quicksort_slice").expect("compile quicksort slice to IR");
    let part_fn = module
        .functions()
        .iter()
        .find(|f| f.name == "partition_slice")
        .expect("find partition_slice");

    let load_count = part_fn
        .blocks()
        .iter()
        .flat_map(|b| &b.instrs)
        .filter(|i| matches!(i, IrInstr::ArrayLoad { .. }))
        .count();
    let store_count = part_fn
        .blocks()
        .iter()
        .flat_map(|b| &b.instrs)
        .filter(|i| matches!(i, IrInstr::ArrayStore { .. }))
        .count();

    assert!(
        load_count >= 4,
        "partition_slice must perform multiple native ArrayLoads, found {}",
        load_count
    );
    assert!(
        store_count >= 4,
        "partition_slice must perform multiple native ArrayStores, found {}",
        store_count
    );
}

#[test]
fn test_sieve_slice_parity() {
    let src = r#"
def run_sieve(sieve: &mut [i64], limit: i64) -> i64 {
    var i = 2;
    while (i * i) <= limit {
        if sieve[i] == 1 {
            var j = i * i;
            while j <= limit {
                sieve[j] = 0;
                j = j + i;
            }
        } else { 0 };
        i = i + 1;
    }

    var count = 0;
    i = 2;
    while i <= limit {
        if sieve[i] == 1 {
            count = count + 1;
        } else { 0 };
        i = i + 1;
    }
    return count
}
"#;

    let module = compile_to_module(src, "sieve_slice").expect("compile sieve slice to IR");
    let sieve_fn = module
        .functions()
        .iter()
        .find(|f| f.name == "run_sieve")
        .expect("find run_sieve");

    let load_count = sieve_fn
        .blocks()
        .iter()
        .flat_map(|b| &b.instrs)
        .filter(|i| matches!(i, IrInstr::ArrayLoad { .. }))
        .count();
    let store_count = sieve_fn
        .blocks()
        .iter()
        .flat_map(|b| &b.instrs)
        .filter(|i| matches!(i, IrInstr::ArrayStore { .. }))
        .count();

    assert!(
        load_count >= 2,
        "run_sieve must perform native ArrayLoads, found {}",
        load_count
    );
    assert!(
        store_count >= 1,
        "run_sieve must perform native ArrayStores, found {}",
        store_count
    );
}
