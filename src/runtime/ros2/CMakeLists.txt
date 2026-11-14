# The IRIS ROS 2 bridge: a C ABI (`iris_ros2.dll` / `libiris_ros2.so`) over
# the full ROS 2 client stack -- rclcpp and its executor, generic
# (runtime-typed) publishers and subscriptions, services, clients and actions
# on the rclcpp/rclcpp_action base classes, timers and clocks (sim-time aware),
# parameters, rclcpp_lifecycle, and tf2_ros.
#
# Build it inside a sourced ROS 2 environment:
#   call C:\dev\ros2_humble\ros2-windows\local_setup.bat      (or source setup.bash)
#   cmake -S src/runtime/ros2 -B target/ros2-bridge
#   cmake --build target/ros2-bridge --config RelWithDebInfo
# or run `python build_ros2_bridge.py`, which does exactly that.
cmake_minimum_required(VERSION 3.16)
project(iris_ros2 CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
  set(CMAKE_BUILD_TYPE RelWithDebInfo)
endif()

find_package(rclcpp REQUIRED)
find_package(rclcpp_action REQUIRED)
find_package(rclcpp_lifecycle REQUIRED)
find_package(rcpputils REQUIRED)
find_package(rmw REQUIRED)
find_package(rosidl_typesupport_introspection_cpp REQUIRED)
find_package(tf2 REQUIRED)
find_package(tf2_ros REQUIRED)
find_package(geometry_msgs REQUIRED)
find_package(action_msgs REQUIRED)
find_package(lifecycle_msgs REQUIRED)
find_package(rcl_interfaces REQUIRED)

set(BRIDGE_SOURCES
  core.cpp
  json.cpp
  introspection.cpp
)
if(EXISTS ${CMAKE_CURRENT_SOURCE_DIR}/bridge.cpp)
  list(APPEND BRIDGE_SOURCES bridge.cpp)
endif()
if(EXISTS ${CMAKE_CURRENT_SOURCE_DIR}/actions.cpp)
  list(APPEND BRIDGE_SOURCES actions.cpp)
endif()

add_library(iris_ros2 SHARED ${BRIDGE_SOURCES})
# Exports are marked in the source (IRIS_ROS2_API); nothing else is visible.
set_target_properties(iris_ros2 PROPERTIES
  CXX_VISIBILITY_PRESET hidden
  VISIBILITY_INLINES_HIDDEN ON)
target_include_directories(iris_ros2 PRIVATE ${CMAKE_CURRENT_SOURCE_DIR})
target_link_libraries(iris_ros2 PRIVATE
  rclcpp::rclcpp
  rclcpp_action::rclcpp_action
  rclcpp_lifecycle::rclcpp_lifecycle
  rcpputils::rcpputils
  rmw::rmw
  rosidl_typesupport_introspection_cpp::rosidl_typesupport_introspection_cpp
  tf2::tf2
  tf2_ros::tf2_ros
  ${geometry_msgs_TARGETS}
  ${action_msgs_TARGETS}
  ${lifecycle_msgs_TARGETS}
  ${rcl_interfaces_TARGETS}
)
if(MSVC)
  target_compile_options(iris_ros2 PRIVATE /W3 /EHsc /utf-8)
  # The SDK is a release build; match its CRT.
  set_property(TARGET iris_ros2 PROPERTY MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
else()
  target_compile_options(iris_ros2 PRIVATE -Wall -Wextra)
endif()
