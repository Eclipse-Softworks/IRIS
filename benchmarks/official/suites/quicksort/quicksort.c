#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

int64_t partition(int64_t *arr, int64_t low, int64_t high) {
    int64_t pivot = arr[high];
    int64_t i = low - 1;
    for (int64_t j = low; j < high; j++) {
        if (arr[j] <= pivot) {
            i++;
            int64_t temp = arr[i];
            arr[i] = arr[j];
            arr[j] = temp;
        }
    }
    int64_t temp = arr[i + 1];
    arr[i + 1] = arr[high];
    arr[high] = temp;
    return i + 1;
}

void quicksort(int64_t *arr, int64_t low, int64_t high) {
    if (low < high) {
        int64_t pi = partition(arr, low, high);
        quicksort(arr, low, pi - 1);
        quicksort(arr, pi + 1, high);
    }
}

int main(void) {
    int64_t n = 100000;
    int64_t *arr = (int64_t *)malloc(n * sizeof(int64_t));
    int64_t seed = 123456789;
    for (int64_t i = 0; i < n; i++) {
        seed = (seed * 1103515245 + 12345) % 2147483648LL;
        arr[i] = seed;
    }

    quicksort(arr, 0, n - 1);

    printf("%lld\n%lld\n", arr[0], arr[n - 1]);
    free(arr);
    return 0;
}
