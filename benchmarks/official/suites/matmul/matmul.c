#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

int main(void) {
    int64_t n = 256;
    int64_t size = n * n;
    double *a = (double *)malloc(size * sizeof(double));
    double *b = (double *)malloc(size * sizeof(double));
    double *c = (double *)malloc(size * sizeof(double));

    for (int64_t i = 0; i < size; i++) {
        int64_t row = i / n;
        int64_t col = i % n;
        a[i] = ((double)((row * col) % 13)) * 0.1;
        b[i] = ((double)((row + col) % 17)) * 0.1;
        c[i] = 0.0;
    }

    for (int64_t r = 0; r < n; r++) {
        for (int64_t col = 0; col < n; col++) {
            double sum = 0.0;
            for (int64_t k = 0; k < n; k++) {
                sum += a[r * n + k] * b[k * n + col];
            }
            c[r * n + col] = sum;
        }
    }

    printf("%lld\n%lld\n", (int64_t)c[0], (int64_t)c[size - 1]);

    free(a);
    free(b);
    free(c);
    return 0;
}
