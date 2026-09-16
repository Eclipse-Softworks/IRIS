fn main() {
    let limit: usize = 100_000;
    let mut sieve = vec![1i64; limit + 1];

    let mut i = 2;
    while (i * i) <= limit {
        if sieve[i] == 1 {
            let mut j = i * i;
            while j <= limit {
                sieve[j] = 0;
                j += i;
            }
        }
        i += 1;
    }

    let mut count = 0;
    for i in 2..=limit {
        if sieve[i] == 1 {
            count += 1;
        }
    }
    println!("{count}");
}
