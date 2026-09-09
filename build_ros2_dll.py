import os
from pathlib import Path
import subprocess
import sys

def main():
    root = Path(__file__).resolve().parent
    ros2_dir = Path(os.environ.get("ROS2_DIR", r"C:\dev\ros2_humble\ros2-windows"))
    if not ros2_dir.exists():
        print(f"Error: ROS2 directory {ros2_dir} does not exist.")
        sys.exit(1)

    include_base = ros2_dir / "include"
    lib_dir = ros2_dir / "lib"
    clang = Path(os.environ.get("CLANG", r"C:\Program Files\LLVM\bin\clang.exe"))
    linker = Path(os.environ.get("LLD_LINK", r"C:\Program Files\LLVM\bin\lld-link.exe"))
    ucrt_lib = Path(os.environ.get(
        "UCRT_LIB", r"C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\ucrt\x64"
    ))
    um_lib = Path(os.environ.get(
        "WINDOWS_UM_LIB", r"C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x64"
    ))
    required = [clang, linker, include_base, lib_dir, ucrt_lib, um_lib]
    missing = [str(path) for path in required if not path.exists()]
    if missing:
        print("Missing ROS 2 bridge build dependencies:\n  " + "\n  ".join(missing))
        sys.exit(1)

    include_packages = [
        "rcl", "rcutils", "rmw", "rosidl_runtime_c",
        "rosidl_typesupport_interface", "std_msgs", "geometry_msgs",
        "builtin_interfaces", "rcl_yaml_param_parser", "rcpputils",
        "service_msgs", "type_description_interfaces",
        "rosidl_dynamic_typesupport",
    ]
    includes = []
    for package in include_packages:
        path = include_base / package
        if path.is_dir():
            includes.extend(["-isystem", str(path)])

    build_dir = root / "target" / "ros2-bridge"
    build_dir.mkdir(parents=True, exist_ok=True)
    bridge_obj = build_dir / "ros2_bridge.obj"
    shim_obj = build_dir / "ros2_crtshim.obj"
    output = root / "iris_ros2.dll"

    compile_bridge = [
        str(clang), "-target", "x86_64-pc-windows-msvc", "-c", "-O2",
        "-o", str(bridge_obj), str(root / "src/runtime/ros2_bridge.c"),
        *includes, "-Wno-ignored-attributes", "-Wno-pragma-pack",
        "-Wno-microsoft-static-assert",
    ]
    compile_shim = [
        str(clang), "-target", "x86_64-pc-windows-msvc", "-c", "-O2",
        "-o", str(shim_obj), str(root / "src/runtime/ros2_crtshim.c"),
    ]
    link = [
        str(linker), "-dll", "-noentry", f"-out:{output}",
        f"-def:{root / 'src/runtime/ros2_bridge.def'}",
        str(bridge_obj), str(shim_obj), f"-libpath:{lib_dir}",
        f"-libpath:{ucrt_lib}", f"-libpath:{um_lib}",
        "rcl.lib", "rcutils.lib", "rmw.lib", "rosidl_runtime_c.lib",
        "std_msgs__rosidl_typesupport_c.lib", "std_msgs__rosidl_generator_c.lib",
        "geometry_msgs__rosidl_typesupport_c.lib",
        "geometry_msgs__rosidl_generator_c.lib", "ucrt.lib", "kernel32.lib",
    ]

    for label, command in [
        ("compile bridge", compile_bridge),
        ("compile CRT shim", compile_shim),
        ("link bridge", link),
    ]:
        print(f"{label}: {' '.join(command)}")
        result = subprocess.run(command, cwd=root, capture_output=True, text=True)
        if result.returncode != 0:
            print(result.stdout)
            print(result.stderr, file=sys.stderr)
            sys.exit(result.returncode)

    print(f"ROS 2 bridge built: {output}")

if __name__ == "__main__":
    main()
