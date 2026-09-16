#include <stdio.h>
#include <stdint.h>

int64_t collatz_length(int64_t n) {
    int64_t steps = 0;
    int64_t x = n;
    while (x != 1) {
        if ((x % 2) == 0) {
            x = x / 2;
        } else {
            x = 3 * x + 1;
        }
        steps++;
    }
    return steps;
}

int main(void) {
    int64_t max_steps = 0;
    int64_t max_n = 1;
    int64_t limit = 500000;
    for (int64_t n = 1; n <= limit; n++) {
        int64_t steps = collatz_length(n);
        if (steps > max_steps) {
            max_steps = steps;
            max_n = n;
        }
    }
    printf("%lld\n%lld\n", max_n, max_steps);
    return 0;
}
