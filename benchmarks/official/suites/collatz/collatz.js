function collatzLength(n) {
    let steps = 0;
    let x = n;
    while (x !== 1) {
        if (x % 2 === 0) {
            x = Math.floor(x / 2);
        } else {
            x = 3 * x + 1;
        }
        steps++;
    }
    return steps;
}

let maxSteps = 0;
let maxN = 1;
const limit = 500000;
for (let n = 1; n <= limit; n++) {
    const steps = collatzLength(n);
    if (steps > maxSteps) {
        maxSteps = steps;
        maxN = n;
    }
}
console.log(maxN);
console.log(maxSteps);
