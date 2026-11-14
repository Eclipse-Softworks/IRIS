# Turtlesim autopilot

An IRIS ROS 2 node that flies turtlesim's `turtle1` around a square, the pen
changing colour on each side, while a second turtle it spawns pursues the
first. It uses each part of `std.ros2` against a real ROS 2 node:

| ROS 2 | Used for |
| --- | --- |
| Services | `/clear`, `/turtle1/teleport_absolute`, `/turtle1/set_pen`, `/kill`, `/spawn` |
| Action | `/turtle1/rotate_absolute` turns `turtle1` toward the first corner |
| Topics | each turtle's `turtlesim/msg/Pose` in, `geometry_msgs/msg/Twist` out |
| Parameters | `linear_gain`, `angular_gain`, `max_speed`, `tolerance`, with ranges, read every tick |
| tf2 | `world → turtle1` and `world → iris_follower` on `/tf` |

| File | Role | Needs |
| --- | --- | --- |
| [autopilot.iris](autopilot.iris) | Steering, pursuit and turtlesim's kinematics, as pure functions | -- |
| [simulate.iris](simulate.iris) | Flies the same controller against simulated kinematics and asserts the result | nothing (both backends) |
| [main.iris](main.iris) | The live node | ROS 2, the bridge, a running turtlesim |

## Run

```powershell
# shell 1 (Qt needs its plugin path on the Windows binary release)
call C:\dev\ros2_humble\ros2-windows\local_setup.bat
set QT_QPA_PLATFORM_PLUGIN_PATH=C:\dev\ros2_humble\ros2-windows\bin\platforms
ros2 run turtlesim turtlesim_node

# shell 2
python build_ros2_bridge.py        # once
iris run projects\turtlesim_autopilot\main.iris
```

While it flies:

```powershell
ros2 param set /iris_turtle_autopilot max_speed 0.8    # slower from the next tick
ros2 run tf2_ros tf2_echo world turtle1                # the frames it broadcasts
rviz2                                                  # fixed frame: world, add a TF display
```

The offline check needs nothing installed:

```powershell
iris run projects\turtlesim_autopilot\simulate.iris
```

## What was observed (2026-10-18, Windows, ROS 2 Humble, Cyclone DDS)

- Native and interpreted runs both flew the square: four corners within the
  0.1 tolerance in 18.6 s, the follower 1.36 units behind at the end.
- `ros2 param set /iris_turtle_autopilot max_speed 0.8` during the flight
  slowed the remaining sides from ~4.7 s to ~9.3 s (28.2 s in total);
  `tf2_echo world turtle1` read the broadcast frame mid-flight.
- `simulate.iris` gives the same controller 1,167 steps (18.7 simulated
  seconds) on both backends.

Limits: turtlesim's world is 11×11 with no obstacles; the controller is
proportional with no collision avoidance between the turtles.
