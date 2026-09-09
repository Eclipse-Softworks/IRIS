# Advanced: simulated actuator controller

A proportional controller approaches a target while saturating each action to the range [-1, 1]. Every simulation step checks the actuator bound, and the final position must be within 0.01 of the target.

## Run

```text
iris run projects/robotic_actuator_control/src/main.iris
```

Default execution is simulation only: no serial connection, GPIO writes, or DDS transport. Pure ROS QoS values do not require a ROS installation.

## Verify

```text
python tools/verify_learning.py --filter projects/robotic_actuator_control/
```

The catalog runner checks the native and interpreter paths separately. Every
assertion is part of the program, so incorrect results fail the run.

## Extend

Introduce disturbance and sensor noise, then validate the control policy before connecting the embedded board hooks.
