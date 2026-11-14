# Arduino Uno controls

Three allocation-free programs for an Arduino Uno: press-to-toggle,
a debounced four-mode lamp, and a fixed-point PID loop. Each is a step
function over top-level `var` state. The board support package calls the
entry repeatedly, and the state stays in SRAM between calls. The same step
functions run on a host, where `simulate.iris` drives them with a bouncing
button, a held button and a simulated heater.

| File | Role |
| --- | --- |
| [toggle.iris](toggle.iris) | Rising-edge toggle and a press counter (no debouncing) |
| [modes.iris](modes.iris) | Debouncer (a level must hold `DEBOUNCE_STEPS`) and a lamp state machine: short press advances off → on → slow blink → fast blink → off on release; a long press switches off |
| [pid.iris](pid.iris) | Integer PID in thousandths: derivative on the measurement, a clamped integral that stops accumulating while the output is pinned (anti-windup), duty 0..255 |
| [uno_toggle.iris](uno_toggle.iris) | Board entry: button on D2 → built-in LED on D13 |
| [uno_modes.iris](uno_modes.iris) | Board entry: button on D2 → LED on D13, one ADC conversion per step for pacing |
| [uno_pid.iris](uno_pid.iris) | Board entry: sensor on A0, setpoint potentiometer on A1, actuator on D9 |
| [simulate.iris](simulate.iris) | Host checks of all three |

## Build

```text
iris embedded projects/uno_controls/uno_modes.iris --target arduino-uno -o build/uno_modes
```

This writes the IRIS module, the embedded runtime and the validation
harness as AVR objects. It also writes the proof manifest
(`iris_embedded.json`) and the board support file `arduino_uno_validation.c`.
Link them with AVR-GCC and upload with AVRDUDE as described in
[docs/embedded.md](../../docs/embedded.md). Wiring for the buttons is in that
document (D2 to 5V through the button, 10 kΩ pull-down to GND); D13 is the
built-in `L` LED.

The Uno profile has no loops (known issue #70), so timing is counted in
steps. `uno_modes.iris` starts every step with one ADC conversion, which
takes 13 ADC clocks (104 µs at the BSP's 16 MHz / 128 prescaler); the
constants in `modes.iris` assume about 8,000 steps a second. That rate
comes from the datasheet and has not been measured. The bundled BSP's PWM
is a digital fallback (on at duty 128 and above), so the PID drives D9 on/off
until `iris_embedded_pwm_write` is given a timer PWM.

## What was checked (2026-10-19, Windows)

`simulate.iris` on both backends (native binary and interpreter, identical
output):

```text
toggle: 3 presses, LED follows each rising edge
slow blink: lit 4000 of 8000 steps
modes: bounce ignored, off -> on -> slow -> fast -> off, long press switches off
pid 200 -> 600: settled at step 78, peak 606, final 600, duty 119..255
pid windup: integral 205860 at saturation; off the limit after 0 steps; final 500
uno controls: toggle, debounced modes and fixed-point PID checked
```

The PID run uses a heater that settles at 200 counts plus 3 per unit of duty,
with a 25-step time constant. It holds within 3 counts of the setpoint from
step 78, overshooting by 6 counts. With the setpoint out of reach, the integral
stopped at 205,860, below its 255,000 clamp.

All three board entries pass the allocation-free proof, and their
fingerprints are the same on every rebuild:

| Program | Proof fingerprint | Object code | Undefined symbols |
| --- | --- | --- | --- |
| uno_toggle | `182524e3e73aac18` | 1.6 KB | the two GPIO hooks |
| uno_modes | `d7ddf18375519ecf` | 3.3 KB | GPIO and ADC hooks, `__moddi3` |
| uno_pid | `05b66e20308922b8` | 3.9 KB | ADC and PWM hooks, `__muldi3`, `__divdi3` |

All three also reference `__do_clear_bss`. The `__*di3` helpers come from
libgcc and `__do_clear_bss` from avr-libc's startup code; AVR-GCC links both.
`tests/embedded_allocation_free.rs` checks the proofs, the fingerprints and
the state initialisation.

**Not done:** linking and flashing. This machine has no AVR-GCC and no board
was attached, so none of these images has produced an `IRIS-HW/1` report;
`iris embedded` says so ("hardware status: not validated"). The last
physically validated Uno image is `examples/embedded/uno_button_led.iris`
(2026-08-31), whose fingerprint is unchanged.
