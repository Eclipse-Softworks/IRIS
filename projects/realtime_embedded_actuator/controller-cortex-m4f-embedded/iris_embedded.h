#ifndef IRIS_EMBEDDED_H
#define IRIS_EMBEDDED_H

#include <stdint.h>

#define IRIS_EMBEDDED_REPORT_VERSION "IRIS-HW/1"
#define IRIS_EMBEDDED_TARGET "cortex-m4f"
#define IRIS_EMBEDDED_FINGERPRINT "911b5e527d621cb0"
#define IRIS_EMBEDDED_FIXED_ARRAY_BYTES 32ULL
#define IRIS_EMBEDDED_ENTRY_SYMBOL iris_embedded_entry_main

int64_t iris_embedded_entry_main(void);
extern volatile int32_t iris_embedded_fault_code;
extern const int64_t iris_embedded_allocation_count;
void iris_embedded_validation_run(void);
void iris_board_validation_report(const char* version, const char* target, const char* fingerprint, int64_t result, int64_t allocations, int32_t faults);

int64_t iris_embedded_gpio_read(int64_t pin);
int64_t iris_embedded_gpio_write(int64_t pin, int64_t value);
int64_t iris_embedded_adc_read(int64_t channel);
int64_t iris_embedded_pwm_write(int64_t channel, int64_t duty);
int64_t iris_embedded_monotonic_us(void);
int64_t iris_embedded_watchdog_kick(void);
int64_t iris_embedded_uart_write_byte(int64_t byte);

#endif
