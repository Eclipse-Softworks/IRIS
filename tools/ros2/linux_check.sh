#!/usr/bin/env bash
# Build the IRIS ROS 2 bridge on Linux (ROS 2 Humble, Ubuntu 22.04) and run
# the ROS 2 tests against it, in a container, from any host with Docker:
#
#     tools/ros2/linux_check.sh
#     IRIS_LINUX_STEPS="interop flagship" tools/ros2/linux_check.sh
#
# IRIS_LINUX_STEPS picks steps (live, interop, sockets, flagship; default
# all); the bridge is always built first.
#
# Runs, in order: the bridge build (build_ros2_bridge.py, CMake against
# /opt/ros/humble), tests/ros2_live.rs (std.ros2 programs, native and ORC),
# tools/ros2/interop_check.py (the bridge against the ros2 CLI and the C++
# demo nodes), and the awaitable-socket tests (tests/test_async_sockets.iris
# on both backends, tests/async_gateway.rs), which also exercise poll() on
# Linux, and the flagship (projects/evolving_turtle/run.sh and
# tests/evolving_turtle.rs: turtlesim as the plant of `iris evolve-live`).
#
# The first run builds a local image, iris-ros2:humble-2 (ros:humble with the
# C++ demo packages and turtlesim, clang/lld 18 for IRIS's native code,
# stable Rust).
# Source is copied in without binaries; Cargo's registry and the target
# directory live in named volumes. Build jobs: IRIS_ROS2_JOBS (default 4).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
JOBS="${IRIS_ROS2_JOBS:-4}"
STEPS="${IRIS_LINUX_STEPS:-all}"
IMAGE=iris-ros2:humble-2

export MSYS_NO_PATHCONV=1

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "building $IMAGE (once)"
  docker build -t "$IMAGE" - <<'DOCKERFILE'
FROM ros:humble
ENV DEBIAN_FRONTEND=noninteractive PATH=/root/.cargo/bin:$PATH
RUN apt-get update -yq \
 && apt-get install -yq --no-install-recommends \
      ros-humble-demo-nodes-cpp ros-humble-action-tutorials-cpp ros-humble-tf2-ros \
      ros-humble-example-interfaces ros-humble-rclcpp-action ros-humble-rclcpp-lifecycle \
      ros-humble-ros2lifecycle ros-humble-ros2action \
      curl ca-certificates rsync wget gnupg lsb-release software-properties-common python3 \
 && rm -rf /var/lib/apt/lists/*
# IRIS emits LLVM IR with opaque pointers; Ubuntu 22.04's clang 14 predates them.
RUN wget -qO /tmp/llvm.sh https://apt.llvm.org/llvm.sh \
 && bash /tmp/llvm.sh 18 \
 && rm -rf /var/lib/apt/lists/* /tmp/llvm.sh
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
# The flagship's simulator (projects/evolving_turtle).
RUN apt-get update -yq \
 && apt-get install -yq --no-install-recommends ros-humble-turtlesim \
 && rm -rf /var/lib/apt/lists/*
DOCKERFILE
fi

docker run --rm \
  -v "$ROOT:/src:ro" \
  -v iris-ros2-registry:/root/.cargo/registry \
  -v iris-ros2-target:/work/target \
  -e IRIS_CLANG=/usr/bin/clang-18 \
  -e IRIS_NO_INTERP_FALLBACK=1 \
  -e CARGO_BUILD_JOBS="$JOBS" \
  -e STEPS="$STEPS" \
  "$IMAGE" bash -euo pipefail -c '
    want() { [ "$STEPS" = all ] || [[ " $STEPS " == *" $1 "* ]]; }
    export PATH=/usr/lib/llvm-18/bin:$PATH
    mkdir -p /work
    cd /src
    rsync -a --delete \
      --exclude target --exclude node_modules --exclude ".git" \
      --exclude "*.dll" --exclude "*.exe" --exclude "*.lib" --exclude "*.pdb" \
      --exclude "*.vsix" --exclude "*.so" --exclude "*.dylib" \
      Cargo.toml Cargo.lock build.rs LLVM_VERSION build_ros2_bridge.py \
      src tests tools examples projects crates stdlib benches \
      /work/
    cd /work
    set +u; source /opt/ros/humble/setup.bash; set -u
    echo "== bridge build"
    python3 build_ros2_bridge.py
    export IRIS_ROS2_LIB=/work/libiris_ros2.so
    cargo build --bin iris
    if want live; then
      echo "== tests/ros2_live.rs"
      cargo test --test ros2_live -- --nocapture --test-threads=1
    fi
    if want interop; then
      echo "== tools/ros2/interop_check.py"
      python3 tools/ros2/interop_check.py
    fi
    if want sockets; then
      echo "== awaitable sockets"
      target/debug/iris --emit binary -o /tmp/sockets tests/test_async_sockets.iris && /tmp/sockets
      IRIS_FORCE_INTERP=1 target/debug/iris --emit eval tests/test_async_sockets.iris
      cargo test --test async_gateway -- --nocapture
    fi
    if want flagship; then
      echo "== flagship: projects/evolving_turtle"
      IRIS=/work/target/debug/iris bash projects/evolving_turtle/run.sh
      cargo test --test evolving_turtle -- --nocapture
    fi
  '
