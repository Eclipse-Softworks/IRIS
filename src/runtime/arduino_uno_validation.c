/* Allocation-free Arduino Uno R3 board support and IRIS validation transport. */
#ifndef F_CPU
#define F_CPU 16000000UL
#endif

#include <stdint.h>
#include <avr/io.h>

#include "iris_embedded.h"

static void uart_init(void) {
    /* 9600 baud, 8N1 at the Uno's 16 MHz clock: UBRR0 = 103. */
    UBRR0H = 0;
    UBRR0L = 103;
    UCSR0A = 0;
    UCSR0B = _BV(TXEN0);
    UCSR0C = _BV(UCSZ01) | _BV(UCSZ00);
}

static void uart_byte(uint8_t value) {
    while ((UCSR0A & _BV(UDRE0)) == 0) {
    }
    UDR0 = value;
}

static void uart_string(const char *value) {
    while (*value != '\0') {
        uart_byte((uint8_t)*value++);
    }
}

static void uart_u64(uint64_t value) {
    char digits[20];
    uint8_t used = 0;
    do {
        digits[used++] = (char)('0' + value % 10);
        value /= 10;
    } while (value != 0);
    while (used != 0) {
        uart_byte((uint8_t)digits[--used]);
    }
}

static void uart_i64(int64_t value) {
    if (value < 0) {
        uart_byte('-');
        uart_u64((uint64_t)(-(value + 1)) + 1);
    } else {
        uart_u64((uint64_t)value);
    }
}

void iris_board_validation_report(const char *version,
                                  const char *target,
                                  const char *fingerprint,
                                  int64_t result,
                                  int64_t allocations,
                                  int32_t faults) {
    uart_string(version);
    uart_string(" target=");
    uart_string(target);
    uart_string(" fingerprint=");
    uart_string(fingerprint);
    uart_string(" result=");
    uart_i64(result);
    uart_string(" allocations=");
    uart_i64(allocations);
    uart_string(" faults=");
    uart_i64(faults);
    uart_string("\r\n");
}

static volatile uint8_t *port_for_pin(uint8_t pin) {
    if (pin <= 7) {
        return &PORTD;
    }
    if (pin <= 13) {
        return &PORTB;
    }
    return &PORTC;
}

static volatile uint8_t *ddr_for_pin(uint8_t pin) {
    if (pin <= 7) {
        return &DDRD;
    }
    if (pin <= 13) {
        return &DDRB;
    }
    return &DDRC;
}

static volatile uint8_t *input_for_pin(uint8_t pin) {
    if (pin <= 7) {
        return &PIND;
    }
    if (pin <= 13) {
        return &PINB;
    }
    return &PINC;
}

static uint8_t bit_for_pin(uint8_t pin) {
    if (pin <= 7) {
        return pin;
    }
    if (pin <= 13) {
        return (uint8_t)(pin - 8);
    }
    return (uint8_t)(pin - 14);
}

int64_t iris_embedded_gpio_read(int64_t pin_value) {
    if (pin_value < 0 || pin_value > 19) {
        return 0;
    }
    uint8_t pin = (uint8_t)pin_value;
    uint8_t bit = bit_for_pin(pin);
    *ddr_for_pin(pin) &= (uint8_t)~_BV(bit);
    return ((*input_for_pin(pin) & _BV(bit)) != 0) ? 1 : 0;
}

int64_t iris_embedded_gpio_write(int64_t pin_value, int64_t value) {
    if (pin_value < 0 || pin_value > 19) {
        return 0;
    }
    uint8_t pin = (uint8_t)pin_value;
    uint8_t bit = bit_for_pin(pin);
    volatile uint8_t *ddr = ddr_for_pin(pin);
    volatile uint8_t *port = port_for_pin(pin);
    *ddr |= _BV(bit);
    if (value != 0) {
        *port |= _BV(bit);
    } else {
        *port &= (uint8_t)~_BV(bit);
    }
    return 0;
}

int64_t iris_embedded_adc_read(int64_t channel) {
    if (channel < 0 || channel > 5) {
        return 0;
    }
    ADMUX = _BV(REFS0) | (uint8_t)channel;
    ADCSRA = _BV(ADEN) | _BV(ADPS2) | _BV(ADPS1) | _BV(ADPS0);
    ADCSRA |= _BV(ADSC);
    while ((ADCSRA & _BV(ADSC)) != 0) {
    }
    return ADC;
}

int64_t iris_embedded_pwm_write(int64_t pin, int64_t duty) {
    /* A bounded digital fallback is deterministic until a timer BSP is chosen. */
    return iris_embedded_gpio_write(pin, duty >= 128);
}

int64_t iris_embedded_monotonic_us(void) {
    /* Timer-free fallback: callers can provide a stronger board-specific hook. */
    return 0;
}

int64_t iris_embedded_watchdog_kick(void) {
    __asm__ __volatile__("wdr");
    return 0;
}

int64_t iris_embedded_uart_write_byte(int64_t byte) {
    uart_byte((uint8_t)byte);
    return 0;
}

int main(void) {
    uart_init();
    iris_embedded_validation_run();
    for (;;) {
        (void)IRIS_EMBEDDED_ENTRY_SYMBOL();
    }
}
