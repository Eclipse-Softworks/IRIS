# Building and validating the ROS 2 bridge

`std.ros2` loads `iris_ros2.dll`, a C bridge over `rcl`. The bridge is built
separately because it requires a ROS 2 SDK; ordinary IRIS builds do not.

Validated on 2026-08-30 against ROS 2 Humble at
`C:\dev\ros2_humble\ros2-windows`.

## Build

```powershell
py build_ros2_dll.py
```

The script compiles `src/runtime/ros2_bridge.c` for the MSVC ABI and links it
with `lld-link`. ROS 2's Windows binaries and headers use that ABI even though
IRIS programs themselves use the MinGW target. The script accepts these path
overrides:

- `ROS2_DIR`
- `CLANG`
- `LLD_LINK`
- `UCRT_LIB`
- `WINDOWS_UM_LIB`

The local Build Tools installation lacks `vcruntime.lib`, so
`src/runtime/ros2_crtshim.c` supplies `_fltused` and `memcpy`. The link uses
`-noentry`; UCRT remains dynamically linked.

## Run the live validations

```powershell
$env:PATH = 'C:\dev\ros2_humble\ros2-windows\bin;' + $env:PATH
iris run examples\10_integration\ros2_topics.iris
```

Observed results:

- Float64, Int64 and String publish/wait/take round trips pass.
- QoS-aware reliable publishers/subscriptions pass.
- Vector3, Twist and Pose round trips preserve signed fractional values.
- The refreshed topic example runs as a native program against the bridge.
  Its catalog entry is `manual` because ordinary CI has no ROS graph.

## Supported surface

- dynamic contexts, nodes, publishers and subscriptions;
- QoS history/depth/reliability/durability profiles;
- typed publish/take for Float64, Int64, String, Vector3, Twist and Pose;
- wait sets and a deterministic single-threaded IRIS executor;
- local managed-lifecycle state;
- quaternion and rigid-transform composition/inversion plus a local transform
  buffer.

Services, actions, parameters, sensor-specific messages, simulated time and
wire-level tf2 transport are not implemented. The lifecycle and transform
layers are IRIS-side deterministic abstractions, not replacements for ROS 2
lifecycle services or `/tf`/`/tf_static` interoperability.

## ABI notes

`ffi_call_i64` transports all foreign arguments in integer slots. Floating
geometry values are scaled by `1e8` on the IRIS side and unscaled in the bridge.
The dispatcher supports exact arities through 12 arguments and rejects larger
calls. Earlier versions silently truncated calls above six arguments, corrupting
QoS, Twist and Pose calls.
