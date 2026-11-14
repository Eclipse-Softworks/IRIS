# Allocation-free microcontroller components

IRIS can emit freestanding, relocatable embedded components without invoking a
C compiler:

```text
iris embedded control_loop.iris --target cortex-m4f
iris embedded control_loop.iris --target cortex-m33
iris embedded control_loop.iris --target esp32-c3
iris embedded control_loop.iris --target esp32
iris embedded uno_validation.iris --target arduino-uno
```

The command produces `iris_module.ll`, `iris_module.o`,
`iris_embedded_runtime.o`, `iris_embedded_harness.o`, `iris_embedded.h`, and an
`iris_embedded.json` proof manifest. Arduino Uno bundles also contain the
allocation-free `arduino_uno_validation.c` BSP. Cortex-M4F, Cortex-M33, and ESP32-C3 use
the standard LLVM ARM/RISC-V backends. Classic Xtensa ESP32 requires
Espressif's LLVM distribution; a stock LLVM installation returns a toolchain
diagnostic instead of pretending to support that target.

## Proven profile

The compiler walks every function reachable from the selected entry and only
accepts scalar operations, control flow, direct calls, and fixed stack arrays.
It rejects dynamic strings and collections, records, choices, options/results,
closures, trait objects, tensors, heap-backed AD, effects/handlers, concurrency,
host services, arbitrary FFI, and recursive call cycles. It then checks the
emitted ELF architecture and rejects objects containing allocator symbols.

The entry ABI is:

```iris
def main() -> i64
```

A result of zero means that the device-side validation succeeded. Fixed arrays
are included in `fixed_array_bytes` in the manifest. Loops are permitted on the
Cortex-M/ESP32 profiles because they do not grow the call stack; recursion is
not.

The Arduino Uno preset targets `avr-unknown-unknown` with CPU `atmega328p`, a
16 MHz clock, and the Uno's 32 KB flash/2 KB SRAM budget. Its LLVM 17 safety
profile rejects fixed arrays and cyclic control flow before codegen: LLVM 17
crashes on the unoptimized array-loop IR and can drop an i64 induction update
at O1. Scalar calls, branches, checked arithmetic and board hooks are supported
and physically validated.

Hardware access is supplied by the board support package through the bounded,
allocation-free hooks declared in `iris_embedded.h` (GPIO, ADC, PWM, monotonic
time, watchdog, and UART). Only those extern names are accepted by the proof,
and on bare-metal targets they are plain declarations: the BSP must define
every hook the program calls.

### State between steps

The board support package calls the entry again and again, so a program that
must remember something between calls (a debouncer's count, a PID integral)
keeps it in program state: `var` at the top level, of type `i64`, `f64` or
`bool`. Each variable is a global in `.bss`. The entry initialises all of them
on its first call, because bare-metal startup code runs no constructors, and
the proof covers that initialiser too. A `var` of any other type (a list, a
string, a record) is rejected: it would need the heap.

```iris
var presses: i64 = 0
var last: i64 = 0

def main() -> i64 effect io {
    val now = iris_embedded_gpio_read(2);
    if now == 1 && last == 0 { presses = presses + 1 };
    last = now;
    iris_embedded_gpio_write(13, presses % 2);
    return 0
}
```

[projects/uno_controls](../projects/uno_controls/README.md) has a toggle, a
debounced state machine and a fixed-point PID built this way, with host
simulations of each. The proof fingerprint is reproducible: rebuilding the
same source gives the same fingerprint.

### LLVM backends

Object emission uses the LLVM-C library found first: `IRIS_LLVM_C_API`, the
IRIS toolchain bundle, then system installations. The stock Windows LLVM
packages omit some backends (LLVM 23.1.1 has no AVR). When the preferred
library lacks the target's backend, IRIS uses the first other installed
LLVM-C that has it, for example LLVM 17's for the Uno. If none has it, the
error names the missing backend.

## Link and validate on hardware

Link all three generated objects into the firmware. Implement the platform
hooks that the program actually uses and this reporting callback:

```c
void iris_board_validation_report(
    const char *version,
    const char *target,
    const char *fingerprint,
    int64_t result,
    int64_t allocations,
    int32_t faults)
{
    /* Send one line over the board's UART/USB serial implementation. */
    printf("%s target=%s fingerprint=%s result=%lld allocations=%lld faults=%ld\n",
           version, target, fingerprint, (long long)result,
           (long long)allocations, (long)faults);
}
```

Call `iris_embedded_validation_run()` after board initialization. A valid line
has this form:

```text
IRIS-HW/1 target=esp32-c3 fingerprint=0123456789abcdef result=0 allocations=0 faults=0
```

The host verifier accepts the line only when its target and fingerprint match
the exact proof manifest and all three result counters are zero. Merely creating
an object is reported as cross-compiled, never as hardware-validated. The report
callback is outside the proven control path and may use the board SDK; the
generated IRIS module, embedded runtime, and harness remain allocator-free.

For ESP-IDF, add the three objects and `iris_embedded.h` to a component and call
the harness from `app_main`. ESP-IDF selects ESP32-C3 with
`idf.py set-target esp32c3`. For Cortex-M, add the objects to the board's normal
linker-script/startup project and call the harness after clocks and UART are
initialized.

### Arduino Uno R3 / BDD Ultimate Starter Kit V2

Use Arduino AVR-GCC to compile the emitted BSP and link it with the three IRIS
objects. Convert the ELF to Intel HEX, then upload it with AVRDUDE using the Uno
bootloader protocol (`atmega328p`, `arduino`, 115200 baud). The BSP transmits
reports at 9600 baud. It uses register-level UART/GPIO/ADC/watchdog operations,
a bounded digital PWM fallback, a timer-free monotonic placeholder returning
zero, and no heap allocation. Replace the PWM/clock hooks with the application's
timer policy before production control deployment.

In one physical validation session on 2026-08-31, a BDD kit clone (enumerated as
COM9 on that host; port names are machine-specific) was identified by signature
`1E 95 0F` as an ATmega328P. The 1,602-byte validation image used 104 bytes of
SRAM and produced:

```text
IRIS-HW/1 target=arduino-uno fingerprint=2a140e7d7f1663d6 result=0 allocations=0 faults=0
```

The pre-existing 32 KB temperature-sketch flash image was backed up and restored
once during validation. The board was subsequently flashed with the interactive
IRIS button/LED demo below. This clone's STK500v1 bootloader aliases EEPROM
read/write commands to program flash, so backup/restore automation must use flash
only unless a real ISP programmer is attached.

The board specifications are documented by Arduino's
[Uno Rev3 hardware page](https://docs.arduino.cc/hardware/uno-rev3/) and
[official datasheet](https://docs.arduino.cc/resources/datasheets/A000066-datasheet.pdf).

### Interactive button-to-LED demo

`examples/embedded/uno_button_led.iris` reads digital pin D2 and drives the
Uno's built-in D13 `L` LED. Wire a tactile pushbutton and 10 kΩ pull-down
resistor as follows:

```text
Uno 5V ---- pushbutton ----+---- Uno D2
                           |
                          10 kΩ
                           |
Uno GND -------------------+

Uno D13 ---------------- built-in L LED (no extra wiring)
```

Place the tactile button across the breadboard's center trench. Connect one
side to 5V. Connect the opposite side to D2, and place the 10 kΩ resistor from
that D2 row to GND. The resistor keeps D2 low while released; pressing the
button connects D2 to 5V. Do not connect 5V directly to GND.

The image physically flashed on 2026-08-31 produced this startup report:

```text
IRIS-HW/1 target=arduino-uno fingerprint=93b74e06a2aff2c6 result=0 allocations=0 faults=0
```

After the report, the BSP repeatedly calls the IRIS entry without allocating:
released means LED off, pressed means LED on. Open the board's enumerated serial
port (for example `COM9` in the recorded validation session) at 9600 baud and
press the Uno reset button to capture the startup report again.
