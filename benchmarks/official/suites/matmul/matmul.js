const n = 256;
const size = n * n;
const a = new Float64Array(size);
const b = new Float64Array(size);
const c = new Float64Array(size);

for (let i = 0; i < size; i++) {
    const row = Math.floor(i / n);
    const col = i % n;
    a[i] = ((row * col) % 13) * 0.1;
    b[i] = ((row + col) % 17) * 0.1;
}

for (let r = 0; r < n; r++) {
    for (let col = 0; col < n; col++) {
        let sum = 0.0;
        for (let k = 0; k < n; k++) {
            sum += a[r * n + k] * b[k * n + col];
        }
        c[r * n + col] = sum;
    }
}

console.log(Math.floor(c[0]));
console.log(Math.floor(c[size - 1]));
