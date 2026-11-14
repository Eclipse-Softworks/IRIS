# Autonomous Drone Hunter & Target Acquisition Simulation in IRIS

An end-to-end autonomous UAV hunting simulation written in 100% native IRIS. The system models multi-spectral sensory perception, 3D terrain occlusion, evasive ground target dynamics, proportional navigation intercept, laser standoff designation, and idiomatic telemetry debriefing.

---

## 1. Architectural Overview

```
                      +---------------------------------------+
                      |       Operational Search Sector       |
                      |   (3D Terrain, Ridges, Occlusions)    |
                      +-------------------+-------------------+
                                          |
                        +-----------------+-----------------+
                        |                                   |
              [Autonomous Drone Hunter]            [Evasive Target Rover]
                        |                                   |
         +--------------+--------------+             * Low-Speed Camouflage
         |              |              |             * High-Speed Evasion
     [Sensors]     [Guidance]       [Power]          * Canyon Breakout
   * RF Receiver * Area Spiral   * Hover drag        * Terrain Clamping
   * FLIR Thermal* Triangulation * Sensor watts
   * 3D LiDAR/EO * Prop-Nav Intercept
                 * Laser Orbit Lock
                 * Return-to-Base (RTB)
                        |
                        v
          +----------------------------+
          |  Bayesian Track Fusion     |
          |  (Alpha-Beta Recursive)    |
          +--------------+-------------+
                         |
                         v
       +----------------------------------+
       |   Idiomatic Mission Debriefing   |
       |  * Result<MissionReport, str>    |
       |  * ASCII 2D Tactical Display     |
       |  * JSON Telemetry Downlink       |
       +----------------------------------+
```

---

## 2. Core Modules

| Module | Description |
|---|---|
| [`types.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/autonomous_drone_hunter/types.iris) | Domain models, algebraic choices (`TargetSignature`, `HuntPhase`, `SensorContact`, `MissionStatus`), records, and telemetry packets. |
| [`math_3d.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/autonomous_drone_hunter/math_3d.iris) | 3D vector geometry, Euclidean metrics, normalization, angle wrapping, and polynomial Padé `atan2_approx`. |
| [`environment.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/autonomous_drone_hunter/environment.iris) | Continuous terrain elevation surface, multi-step line-of-sight occlusion raycasting, ambient wind fields, and dynamic evasive target state machine. |
| [`sensors.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/autonomous_drone_hunter/sensors.iris) | Multi-spectral perception suite (RF direction finder, FLIR thermal camera, optical LiDAR) and Bayesian recursive track fusion. |
| [`guidance.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/autonomous_drone_hunter/guidance.iris) | Autonomous state machine, Archimedean spiral search generator, Proportional Navigation pursuit law, standoff loiter orbit, and 6S LiPo power model. |
| [`reporter.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/autonomous_drone_hunter/reporter.iris) | Ground control reporting pipeline: 2D ASCII tactical radar situation display, structured telemetry scorecards, and JSON downlink generator. |
| [`main.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/autonomous_drone_hunter/main.iris) | Simulation testbench executing the complete hunt-to-touchdown lifecycle. |

---

## 3. Idiomatic IRIS Highlights

1. **Algebraic Data Types & Exhaustive Matching**:
   ```iris
   pub choice HuntPhase {
       AreaSearch(f64, f64),
       SignalTriangulation(Vector3, f64),
       InterceptPursuit(Vector3, f64),
       TerminalLaserLock(i64, Vector3),
       ReturnToBase(Vector3),
       MissionDebrief,
   }
   ```
   State transitions and tactical events are handled declaratively using `when phase { ... }`.

2. **Result-Oriented Control Flow**:
   ```iris
   pub def run_drone_hunter_simulation() -> result<MissionReport, str>
   ```
   The mission return value packages success or failure reasons inside native `result<T, E>` types without exceptions or unhandled aborts.

3. **Strongly Typed Domain Records**:
   Kinematics (`DroneKinematics`), battery capacity (`DronePower`), and target tracks (`TargetTrack`) encapsulate state invariants with zero raw pointer overhead.

4. **Multi-File Modular Encapsulation**:
   Inter-module coupling is cleanly structured using `bring` statements:
   ```iris
   bring "types.iris"
   bring "math_3d.iris"
   bring "environment.iris"
   bring "sensors.iris"
   bring "guidance.iris"
   bring "reporter.iris"
   ```

---

## 4. Execution & Verification

Run the simulation directly using the IRIS compiler:

```powershell
iris run projects/autonomous_drone_hunter/main.iris
```

### Expected Output Summary
- **Phase 1 (T=0..2s)**: Drone departs home base $(0, 0, 0)$ into an expanding Archimedean spiral search.
- **Phase 2 (T=1.5s)**: Weak RF beacon detected; bearing triangulation engaged.
- **Phase 3 (T=3.0s)**: Conical FLIR acquires thermal engine exhaust; Proportional Navigation intercept initiated. Target detects incoming hunter and executes high-speed evasion into canyon coordinates.
- **Phase 4 (T=31.0s)**: High-precision LiDAR achieves boresight lock at 25.9m standoff distance with 97.8% confidence.
- **Phase 5 (T=31.5s)**: Target coordinates logged; laser designator confirms neutralization; drone disengages to Return To Base (RTB).
- **Phase 6 (T=93.0s)**: Touchdown at home base $(0, 0, 0)$. Tactical ASCII map, telemetry scorecard, and JSON ground control downlink payload rendered.
