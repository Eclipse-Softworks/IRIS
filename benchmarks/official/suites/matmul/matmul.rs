fn main() {
    let n: usize = 256;
    let size = n * n;
    let mut a = vec![0.0f64; size];
    let mut b = vec![0.0f64; size];
    let mut c = vec![0.0f64; size];

    for i in 0..size {
        let row = i / n;
        let col = i % n;
        a[i] = (((row * col) % 13) as f64) * 0.1;
        b[i] = (((row + col) % 17) as f64) * 0.1;
    }

    for r in 0..n {
        for col in 0..n {
            let mut sum = 0.0f64;
            for k in 0..n {
                sum += a[r * n + k] * b[k * n + col];
            }
            c[r * n + col] = sum;
        }
    }

    println!("{}\n{}", c[0] as i64, c[size - 1] as i64);
}
