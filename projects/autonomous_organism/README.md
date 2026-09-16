# Autonomous Living Organism: Real-Time Adaptive Evolution

This project demonstrates a living computational organism written in IRIS that continuously regulates its internal homeostasis (energy, core temperature, physical integrity) while surviving in an unpredictable, non-stationary environment.

## The Challenge

Unlike static optimization where an algorithm fits a fixed function, this organism lives in a world that cycles across four extreme climate eras:

1. **Temperate Glade**: Mild temperature (22°C), abundant food resources, low hazard. The organism must forage to build energy reserves.
2. **Solar Heatwave**: Scorching ambient heat (46°C), severe dehydration. The organism must actively shed heat (`CoolDown`) and conserve metabolic calories.
3. **Cryo Freeze**: Sub-zero blizzards (-18°C), frozen terrain. The organism must burn calories to generate metabolic heat (`HeatUp`) to avoid fatal hypothermia.
4. **Toxic Radiation Storm**: High disturbance exposure. The organism must take defensive shelter (`Shelter`) to prevent structural health degradation.

## Run

### 1. Interactive 2D World Simulation (Self-Evolution Stages)
Run the real-time, interactive 2D spatial grid world dedicated binary simulating all 5 stages of machine-code self-evolution:
```powershell
cargo run -p autonomous_organism -- --ticks 150 --fps 8
```

Interactive Keyboard Controls:
* `[Space]`: Pause / Resume simulation
* `[s]`: Step single tick
* `[e]`: Force trigger self-evolution crisis immediately
* `[r]`: Inject random ecological disaster / climate regime shift
* `[1-4]`: Force climate era (1: Glade, 2: Heatwave, 3: Cryo Freeze, 4: Toxic Storm)
* `[+/-]`: Speed up / slow down simulation FPS
* `[q]`: Quit gracefully

### 2. Standalone Python Polyglot TUI Visualizer
Run the identical interactive 2D world simulation in Python using the `iris_evolution` C-ABI bindings:
```powershell
python projects/autonomous_organism/tui_sim.py --ticks 120 --fps 8
```

### 3. Standalone IRIS Script
Run the pure IRIS simulation:
```powershell
iris run projects/autonomous_organism/main.iris
```
