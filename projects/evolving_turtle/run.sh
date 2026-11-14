#!/usr/bin/env bash
# One command (Linux, in a sourced ROS 2 environment): a ROS 2 robot whose
# controller is replaced while it drives.
#
#     source /opt/ros/humble/setup.bash
#     projects/evolving_turtle/run.sh
#
# Starts turtlesim (headless when there is no display), runs `iris
# evolve-live` with turtle1 as the plant over ROS 2, and stops turtlesim. Exits
# non-zero unless the run shows the expected story: the allocating candidate
# refused, the unclamped one promoted then rolled back, the saturated one
# promoted, and the audit chain verified.
#
# Environment: IRIS (default: target/release or target/debug iris in this
# repository), IRIS_ROS2_LIB (default: libiris_ros2.so in the repository).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
iris="${IRIS:-}"
if [ -z "$iris" ]; then
  for candidate in "$repo/target/release/iris" "$repo/target/debug/iris"; do
    if [ -x "$candidate" ]; then iris="$candidate"; break; fi
  done
fi
iris="${iris:-iris}"
if [ -z "${IRIS_ROS2_LIB:-}" ] && [ -f "$repo/libiris_ros2.so" ]; then
  export IRIS_ROS2_LIB="$repo/libiris_ros2.so"
fi
if [ "${RMW_IMPLEMENTATION:-}" = "rmw_cyclonedds_cpp" ]; then
  export CYCLONEDDS_URI="file://$here/cyclonedds_local.xml"
fi
if [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
  export QT_QPA_PLATFORM=offscreen
fi

# The node itself, not `ros2 run`: killing the `ros2 run` launcher leaves its
# turtlesim_node running, and a second run would then drive two turtle1s.
"$(ros2 pkg prefix turtlesim)/lib/turtlesim/turtlesim_node" > /tmp/iris_turtlesim.log 2>&1 &
turtlesim=$!
trap 'kill "$turtlesim" 2>/dev/null || true; wait "$turtlesim" 2>/dev/null || true' EXIT
sleep 2

audit="$repo/target/evolving_turtle"
mkdir -p "$audit"
rm -f "$audit/audit.jsonl" "$audit/audit.head.json"
hash="$(sha256sum "$here/constitution.txt" | cut -d' ' -f1)"
log="$(mktemp)"

"$iris" evolve-live \
  --plant "$here/ros2_plant.iris" \
  --baseline "$here/baseline.iris" \
  --candidate "$here/candidates/with_history.iris" \
  --candidate "$here/candidates/lqr_unclamped.iris" \
  --candidate "$here/candidates/lqr_saturated.iris" \
  --cases "$here/cases.json" \
  --constitution "$here/constitution.txt" --constitution-sha256 "$hash" \
  --audit "$audit/audit.jsonl" --audit-head "$audit/audit.head.json" \
  --min-output=-2 --max-output 2 --setpoints 2,8,3,6 --hold 6 --dt 0.032 --offer-every 4 \
  --require-allocation-free 2>&1 | tee "$log"

for line in "rejected        with_history" "promoted        lqr_unclamped" \
            "rolled back     generation 2" "promoted        lqr_saturated" "audit chain verified"; do
  if ! grep -qF "$line" "$log"; then
    echo "the run did not show: $line" >&2
    exit 1
  fi
done
echo
echo "evolving turtle: refused, promoted, rolled back, promoted, audit verified"
