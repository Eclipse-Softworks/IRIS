const limit = 100000;
const sieve = new Int32Array(limit + 1);
for (let i = 0; i <= limit; i++) {
    sieve[i] = 1;
}

for (let i = 2; i * i <= limit; i++) {
    if (sieve[i] === 1) {
        for (let j = i * i; j <= limit; j += i) {
            sieve[j] = 0;
        }
    }
}

let count = 0;
for (let i = 2; i <= limit; i++) {
    if (sieve[i] === 1) {
        count++;
    }
}
console.log(count);
