# Autonomous Quadrotor Drone Autopilot & Extended Kalman Filter (EKF)

A 6-DOF aerodynamic flight simulator, Extended Kalman Filter (EKF) sensor fusion engine, and adaptive cascaded attitude/position flight controller written in native IRIS.

---

## Architecture

- **6-DOF Rigid-Body Aerodynamics**: Full non-linear translational and rotational dynamics, Euler-Cromer numerical integration, aerodynamic translational drag, gyroscopic torque coupling, and stochastic crosswind gusts.
- **Extended Kalman Filter (EKF)**: Fuses noisy 3-axis accelerometer and position telemetry with high-frequency gyroscope angular rates to estimate position, velocity, and roll/pitch orientation.
- **Cascaded Flight Controller**:
  - *Outer Loop*: 3D position and altitude error translates into commanded horizontal tilt angles and collective vertical thrust.
  - *Inner Loop*: Attitude error PID generating differential motor torques.
  - *Adaptive Mixer*: Dynamically compensates for in-flight motor degradation (e.g. 25% thrust drop on motor 1) to prevent catastrophic loss of control.

---

## Running the Mission

Execute the 1200-cycle (100 Hz) mission testbench:

```powershell
.\target\release\iris.exe run projects/quadrotor_autopilot/main.iris
```
