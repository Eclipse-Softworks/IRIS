fn partition(arr: &mut [i64], low: usize, high: usize) -> usize {
    let pivot = arr[high];
    let mut i = low;
    for j in low..high {
        if arr[j] <= pivot {
            arr.swap(i, j);
            i += 1;
        }
    }
    arr.swap(i, high);
    i
}

fn quicksort(arr: &mut [i64], low: usize, high: usize) {
    if low < high {
        let pi = partition(arr, low, high);
        if pi > 0 {
            quicksort(arr, low, pi - 1);
        }
        quicksort(arr, pi + 1, high);
    }
}

fn main() {
    let n = 100_000;
    let mut arr = vec![0i64; n];
    let mut seed: i64 = 123456789;
    for i in 0..n {
        seed = (seed.wrapping_mul(1103515245).wrapping_add(12345)) % 2147483648;
        arr[i] = seed;
    }

    quicksort(&mut arr, 0, n - 1);

    println!("{}\n{}", arr[0], arr[n - 1]);
}
