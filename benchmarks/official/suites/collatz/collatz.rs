fn collatz_length(n: i64) -> i64 {
    let mut steps = 0;
    let mut x = n;
    while x != 1 {
        if x % 2 == 0 {
            x /= 2;
        } else {
            x = 3 * x + 1;
        }
        steps += 1;
    }
    steps
}

fn main() {
    let mut max_steps = 0;
    let mut max_n = 1;
    let limit = 500_000;
    for n in 1..=limit {
        let steps = collatz_length(n);
        if steps > max_steps {
            max_steps = steps;
            max_n = n;
        }
    }
    println!("{max_n}\n{max_steps}");
}
