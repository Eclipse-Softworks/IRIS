"""Build the IRIS ROS 2 bridge (`iris_ros2.dll` / `libiris_ros2.so`).

The bridge is a C++ library on the ROS 2 client stack (rclcpp, rclcpp_action,
rclcpp_lifecycle, tf2_ros), so it is built the way any ROS 2 package is: with
CMake, inside a sourced ROS 2 environment. This script sources the environment
when it is not already active, configures and builds `src/runtime/ros2`, and
copies the library to the repository root, where `std.ros2` loads it.

    python build_ros2_bridge.py            # RelWithDebInfo, the SDK's own config
    python build_ros2_bridge.py --clean

Environment overrides:
    ROS2_DIR      ROS 2 install root (default: the sourced one, else
                  C:\\dev\\ros2_humble\\ros2-windows or /opt/ros/humble)
    ROS2_PYTHON   directory of the Python the SDK was built against (Windows
                  Humble: Python 3.8, default C:\\Python38)
    CMAKE_GENERATOR  CMake generator (Windows default: Visual Studio 17 2022)
"""

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
SOURCE = ROOT / "src" / "runtime" / "ros2"
BUILD = ROOT / "target" / "ros2-bridge"
CONFIG = "RelWithDebInfo"


def ros2_dir() -> Path:
    if os.environ.get("ROS2_DIR"):
        return Path(os.environ["ROS2_DIR"])
    default = r"C:\dev\ros2_humble\ros2-windows" if os.name == "nt" else "/opt/ros/humble"
    return Path(default)


def sourced() -> bool:
    return bool(os.environ.get("AMENT_PREFIX_PATH")) and bool(os.environ.get("ROS_DISTRO"))


def in_ros_env(command: str) -> list:
    """Wrap a shell command so it runs inside the ROS 2 environment."""
    if os.name == "nt":
        # A batch file, because `cmd /c` cannot parse the quoting Python
        # generates for a compound command line.
        lines = ["@echo off"]
        if not sourced():
            setup = ros2_dir() / "local_setup.bat"
            if not setup.exists():
                sys.exit(f"ROS 2 not found: {setup} does not exist (set ROS2_DIR)")
            python = Path(os.environ.get("ROS2_PYTHON", r"C:\Python38"))
            if python.exists():
                # The Humble SDK's CMake config calls the Python it was built with.
                lines.append(f'set "PATH={python};{python / "Scripts"};%PATH%"')
            lines.append(f'call "{setup}"')
        lines += [command, "exit /b %errorlevel%"]
        BUILD.mkdir(parents=True, exist_ok=True)
        script = BUILD / "_in_ros_env.bat"
        script.write_text("\r\n".join(lines) + "\r\n", encoding="utf-8")
        return ["cmd", "/c", str(script)]
    if sourced():
        return ["bash", "-c", command]
    setup = ros2_dir() / "setup.bash"
    if not setup.exists():
        sys.exit(f"ROS 2 not found: {setup} does not exist (set ROS2_DIR)")
    return ["bash", "-c", f'source "{setup}" && {command}']


def run(command: str) -> None:
    wrapped = in_ros_env(command)
    print("+", command, flush=True)
    result = subprocess.run(wrapped, cwd=ROOT)
    if result.returncode != 0:
        sys.exit(result.returncode)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--clean", action="store_true", help="delete the build tree first")
    args = parser.parse_args()

    if args.clean and BUILD.exists():
        shutil.rmtree(BUILD)

    generator = os.environ.get("CMAKE_GENERATOR")
    configure = f'cmake -S "{SOURCE}" -B "{BUILD}"'
    if os.name == "nt":
        configure += f' -G "{generator or "Visual Studio 17 2022"}" -A x64'
    else:
        if generator:
            configure += f' -G "{generator}"'
        configure += f" -DCMAKE_BUILD_TYPE={CONFIG}"
    run(configure)
    run(f'cmake --build "{BUILD}" --config {CONFIG}')

    if os.name == "nt":
        built = BUILD / CONFIG / "iris_ros2.dll"
        target = ROOT / "iris_ros2.dll"
    else:
        built = BUILD / "libiris_ros2.so"
        target = ROOT / "libiris_ros2.so"
    if not built.exists():
        sys.exit(f"build finished but {built} is missing")
    shutil.copy2(built, target)
    print(f"ROS 2 bridge built: {target}")


if __name__ == "__main__":
    main()
