# Allocation-free board components

These programs use the restricted embedded compiler profile. They return a
status code because dynamic assertion/panic machinery is outside that profile.
Compile them as components, not as desktop executables:

```text
iris embedded examples/embedded/control_loop.iris --target cortex-m4f
iris embedded examples/embedded/control_loop.iris --target esp32-c3
iris embedded examples/embedded/uno_button_led.iris --target arduino-uno
```

Cortex-M/ESP32 components permit fixed arrays and loops. The Uno/LLVM 17
profile excludes them; its board support package repeatedly calls main instead.
Classic Xtensa ESP32 needs Espressif's LLVM distribution.

## Uno R3 / BDD Ultimate Starter Kit V2

Use the built-in D13 L LED and a pushbutton on D2:

```text
5V ---- pushbutton ----+---- D2
                       |
                      10k
                       |
GND -------------------+
```

Place the button across the breadboard center trench. Its output row connects
to D2 and a 10 kΩ resistor to GND. Pressed drives the built-in LED on;
released drives it off. No external LED resistor is needed for the built-in LED.

Link the generated module/runtime/harness objects with the emitted AVR board
support file, then upload using the board's toolchain. This repository rewrite
does not flash the connected board.
See [the full embedded guide](../../docs/embedded.md) for linking, upload,
serial output, and the platform's clock/PWM restrictions.

A hardware report is accepted only when its target and fingerprint match the
new build and result, allocations, and faults are all zero. Earlier recorded
device fingerprints are historical evidence and do not validate a rewritten
program or a newly connected device.
