#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

int main(void) {
    int64_t limit = 100000;
    int64_t *sieve = (int64_t *)malloc((limit + 1) * sizeof(int64_t));
    for (int64_t i = 0; i <= limit; i++) {
        sieve[i] = 1;
    }

    for (int64_t i = 2; (i * i) <= limit; i++) {
        if (sieve[i] == 1) {
            for (int64_t j = i * i; j <= limit; j += i) {
                sieve[j] = 0;
            }
        }
    }

    int64_t count = 0;
    for (int64_t i = 2; i <= limit; i++) {
        if (sieve[i] == 1) {
            count++;
        }
    }

    printf("%lld\n", count);
    free(sieve);
    return 0;
}
