function partition(arr, low, high) {
    const pivot = arr[high];
    let i = low - 1;
    for (let j = low; j < high; j++) {
        if (arr[j] <= pivot) {
            i++;
            const temp = arr[i];
            arr[i] = arr[j];
            arr[j] = temp;
        }
    }
    const temp = arr[i + 1];
    arr[i + 1] = arr[high];
    arr[high] = temp;
    return i + 1;
}

function quicksort(arr, low, high) {
    while (low < high) {
        const pi = partition(arr, low, high);
        if (pi - low < high - pi) {
            quicksort(arr, low, pi - 1);
            low = pi + 1;
        } else {
            quicksort(arr, pi + 1, high);
            high = pi - 1;
        }
    }
}

const n = 100000;
const arr = new BigInt64Array(n);
let seed = 123456789n;
for (let i = 0; i < n; i++) {
    seed = (seed * 1103515245n + 12345n) % 2147483648n;
    arr[i] = seed;
}

quicksort(arr, 0, n - 1);

console.log(arr[0].toString());
console.log(arr[n - 1].toString());
