//! Controller simulation has no machine-specific ROS paths or hardware effects.
#[path = "support/learning.rs"]
mod learning;

#[test]
fn robotic_controller_obeys_actuator_bounds_and_reaches_target() {
    learning::run(
        "projects/robotic_actuator_control/src/main.iris",
        "native",
        "robot controller: simulated target reached within actuator bounds",
    );
}
