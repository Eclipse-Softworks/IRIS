"""IRIS Cybernetic World: Interactive TUI Self-Evolution Simulator (Python Polyglot).

Visualizes a 2D spatial grid world and models the 5 stages of machine-code self-evolution:
  1. Sense & Homeostasis (4-lane SIMD sensory vector & 8-register state)
  2. Perturbation & Crisis Trigger (regime shift / allostatic strain spike)
  3. Quality-Diversity / MAP-Elites Behavioral Archive Exploration & Gradient Tuning
  4. Formal 7-Gate Safety Verification Audit
  5. Zero-Downtime Hot-Swap & Physiological Recovery

Usage:
  python projects/autonomous_organism/tui_sim.py [--ticks 120] [--fps 8.0] [--seed 42]
"""

import argparse
import os
import random
import sys
import time
from collections import deque

if hasattr(sys.stdout, "reconfigure"):
    try:
        sys.stdout.reconfigure(encoding="utf-8")
    except Exception:
        pass

# Ensure iris_evolution package is in sys.path
package_dir = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", "bindings", "python"))
if package_dir not in sys.path:
    sys.path.insert(0, package_dir)

try:
    from iris_evolution import EvolutionarySearch, GeneNode, GenomeState
except ImportError:
    # Standalone execution fallback
    EvolutionarySearch = None
    GeneNode = None
    GenomeState = None

AutonomousOrganism = None

try:
    import msvcrt

    def poll_key() -> str | None:
        if msvcrt.kbhit():
            ch = msvcrt.getch()
            try:
                return ch.decode("utf-8", errors="ignore")
            except Exception:
                return None
        return None
except ImportError:
    def poll_key() -> str | None:
        return None


# 2D World Tile Types
TILE_EMPTY = 0
TILE_FOOD = 1
TILE_VENT = 2
TILE_ICE = 3
TILE_SHELTER = 4
TILE_TOXIC = 5

TILE_RENDER = {
    TILE_EMPTY: ("·", "\x1b[90m"),        # Dim gray
    TILE_FOOD: ("*", "\x1b[32;1m"),       # Bright green
    TILE_VENT: ("~", "\x1b[31;1m"),       # Bright red
    TILE_ICE: ("#", "\x1b[36;1m"),        # Bright cyan
    TILE_SHELTER: ("^", "\x1b[33;1m"),    # Bright yellow
    TILE_TOXIC: ("X", "\x1b[35;1m"),      # Bright magenta
}

REGIMES = [
    {"name": "Temperate Glade (Abundant, Mild)", "temp": 22.0, "res": 0.85, "hazard": 0.05, "action": 0},
    {"name": "Solar Heatwave (Scorching, Arid)", "temp": 46.0, "res": 0.20, "hazard": 0.12, "action": 2},
    {"name": "Cryo Freeze (Sub-Zero, Blizzards)", "temp": -18.0, "res": 0.15, "hazard": 0.15, "action": 3},
    {"name": "Toxic Radiation Storm (High Damage)", "temp": 16.0, "res": 0.05, "hazard": 0.80, "action": 1},
]

ACTION_NAMES = ["Forage", "Shelter", "Cool Down", "Heat Up"]
STAGES = [
    "1. HOMEOSTASIS",
    "2. CRISIS DETECTED",
    "3. MAP-ELITES DIVERSITY",
    "4. 7-GATE SAFETY AUDIT",
    "5. ZERO-DOWNTIME HOT-SWAP",
]


class PythonWorldSim:
    def __init__(self, width: int = 32, height: int = 12, regime_duration: int = 30, seed: int | None = 42):
        if seed is not None:
            random.seed(seed)
        self.width = width
        self.height = height
        self.regime_duration = regime_duration

        self.grid = [[TILE_EMPTY for _ in range(width)] for _ in range(height)]
        self._init_terrain()

        self.organism_x = width // 4
        self.organism_y = height // 2
        self.health = 100.0
        self.energy = 100.0
        self.core_temp = 25.0
        self.allostatic_load = 0.0
        self.generation = 1
        self.hot_swaps_count = 0
        self.active_action = 0
        self.is_alive = True

        self.regime_idx = 0
        self.ambient_temp = REGIMES[0]["temp"]
        self.current_stage = 0
        self.stage_ticks = 0

        self.events = deque(maxlen=6)
        self.history_strain = deque(maxlen=20)
        self.registers = [0] * 8
        self.vector_registers = [[0, 0, 0, 0] for _ in range(4)]

        # MAP-Elites mock archive 5x4 matrix
        self.map_elites = {}

        # 7-Gate status
        self.safety_gates = [True] * 7

        # Active policy code
        self.active_iris = "def policy(input: i64) -> i64 { return 0 }"
        self.active_python = "lambda x: 0"

        # Try to use IRIS organism
        self.iris_organism = None
        if AutonomousOrganism is not None:
            try:
                self.iris_organism = AutonomousOrganism(seed=seed or 42, regime_duration=regime_duration)
            except Exception:
                self.iris_organism = None

        self.record_event("Genesis: Organism spawned in 2D World (Gen 1 baseline)")

    def _init_terrain(self):
        # Vents
        for y in range(1, 4):
            for x in range(self.width - 10, self.width - 6):
                if random.random() < 0.7:
                    self.grid[y][x] = TILE_VENT
        # Ice
        for y in range(self.height - 5, self.height - 1):
            for x in range(2, 8):
                if random.random() < 0.65:
                    self.grid[y][x] = TILE_ICE
        # Shelter
        for y in range(4, 6):
            for x in range(12, 15):
                self.grid[y][x] = TILE_SHELTER
        # Toxic
        for y in range(5, 8):
            for x in range(self.width - 8, self.width - 4):
                if random.random() < 0.6:
                    self.grid[y][x] = TILE_TOXIC
        # Food
        for y in range(self.height):
            for x in range(self.width):
                if self.grid[y][x] == TILE_EMPTY and random.random() < 0.12:
                    self.grid[y][x] = TILE_FOOD

    def record_event(self, msg: str):
        self.events.append(msg)

    def find_nearest(self, target_tile: int) -> tuple[int, int, float] | None:
        best = None
        for y in range(self.height):
            for x in range(self.width):
                if self.grid[y][x] == target_tile:
                    dist = ((x - self.organism_x) ** 2 + (y - self.organism_y) ** 2) ** 0.5
                    if best is None or dist < best[2]:
                        best = (x, y, dist)
        return best

    def step(self, tick: int):
        new_regime_idx = (tick // self.regime_duration) % len(REGIMES)
        regime_changed = new_regime_idx != self.regime_idx
        self.regime_idx = new_regime_idx
        regime = REGIMES[self.regime_idx]

        if regime_changed:
            self.record_event(f"Regime Shift: Era entered {regime['name']} ({regime['temp']:.1f}°C)")

        # Drift ambient temp
        self.ambient_temp += (regime["temp"] - self.ambient_temp) * 0.15 + (random.random() - 0.5) * 2.0

        # Respawn food
        for y in range(self.height):
            for x in range(self.width):
                if self.grid[y][x] == TILE_EMPTY and random.random() < regime["res"] * 0.10:
                    self.grid[y][x] = TILE_FOOD

        # Compute local sensory conditions
        local_tile = self.grid[self.organism_y][self.organism_x]
        local_temp = self.ambient_temp
        if local_tile == TILE_VENT:
            local_temp += 18.0
        elif local_tile == TILE_ICE:
            local_temp -= 18.0
        elif local_tile == TILE_SHELTER:
            local_temp = 24.0 + (local_temp - 24.0) * 0.25

        local_hazard = regime["hazard"]
        if local_tile == TILE_TOXIC:
            local_hazard = min(1.0, local_hazard + 0.55)
        elif local_tile == TILE_SHELTER:
            local_hazard *= 0.15

        temp_delta = int(round(local_temp - 25.0))
        target_dist = 99
        if local_hazard > 0.4:
            s = self.find_nearest(TILE_SHELTER)
            if s: target_dist = int(round(s[2]))
        elif self.core_temp > 32.0:
            c = self.find_nearest(TILE_ICE)
            if c: target_dist = int(round(c[2]))
        elif self.core_temp < 18.0:
            h = self.find_nearest(TILE_VENT)
            if h: target_dist = int(round(h[2]))
        else:
            f = self.find_nearest(TILE_FOOD)
            if f: target_dist = int(round(f[2]))

        # Decide action
        if self.current_stage == 0 and self.generation > 1:
            self.active_action = regime["action"]
        else:
            self.active_action = 0  # Baseline: Forage

        # Update registers
        self.registers[0] = int(round(self.core_temp))
        self.registers[1] = int(round(self.energy))
        self.registers[2] = self.active_action
        self.registers[3] = self.generation
        self.vector_registers[0] = [temp_delta, int(round(self.energy)), int(round(local_hazard * 100)), target_dist]

        # Spatial Movement
        if self.active_action == 0:  # Forage
            f = self.find_nearest(TILE_FOOD)
            if f: self._move_toward(f[0], f[1])
        elif self.active_action == 1:  # Shelter
            s = self.find_nearest(TILE_SHELTER)
            if s: self._move_toward(s[0], s[1])
        elif self.active_action == 2:  # Cool Down
            c = self.find_nearest(TILE_ICE)
            if c: self._move_toward(c[0], c[1])
            elif self.organism_x > 0: self.organism_x -= 1
        elif self.active_action == 3:  # Heat Up
            h = self.find_nearest(TILE_VENT)
            if h: self._move_toward(h[0], h[1])

        # Consume food
        if self.grid[self.organism_y][self.organism_x] == TILE_FOOD:
            self.energy = min(100.0, self.energy + 28.0)
            self.grid[self.organism_y][self.organism_x] = TILE_EMPTY

        # Physiological Dynamics
        exposure = 0.20 if self.active_action == 1 else (0.40 if self.active_action in (2, 3) else 1.0)
        self.core_temp += (local_temp - self.core_temp) * 0.10 * exposure
        if self.active_action == 2 and self.core_temp > 23.0:
            self.core_temp -= min(3.5, max(0.0, (self.core_temp - 22.0) * 0.4))
        elif self.active_action == 3 and self.core_temp < 27.0:
            self.core_temp += min(3.5, max(0.0, (28.0 - self.core_temp) * 0.4))

        cost = 2.0 if self.active_action == 0 else (0.4 if self.active_action == 1 else 1.0)
        self.energy = max(0.0, min(100.0, self.energy - cost))

        h_dmg = local_hazard * 15.0 * exposure
        t_stress = max(0.0, (15.0 - self.core_temp) * 1.6) if self.core_temp < 15.0 else (max(0.0, (self.core_temp - 35.0) * 1.8) if self.core_temp > 35.0 else 0.0)
        starve = 6.0 if self.energy <= 0.0 else 0.0
        heal = 1.5 if (self.energy > 50.0 and t_stress == 0.0 and h_dmg < 1.0) else 0.0
        self.health = max(0.0, min(100.0, self.health - (h_dmg + t_stress + starve) + heal))

        # Strain
        en_def = (100.0 - self.energy) / 100.0
        tp_def = abs(self.core_temp - 25.0) / 15.0
        hp_def = (100.0 - self.health) / 100.0
        self.allostatic_load = en_def * 0.3 + tp_def * 0.4 + hp_def * 0.3
        self.history_strain.append(self.allostatic_load)

        if self.health <= 0.0:
            self.is_alive = False

        # Self-Evolution Stage Pipeline
        recent_strain = sum(self.history_strain) / len(self.history_strain)
        if self.current_stage == 0:
            if regime_changed or recent_strain > 0.35:
                self.current_stage = 1
                self.stage_ticks = 0
                self.record_event(f"Stage 2 CRISIS: Strain {recent_strain:.2f} > 0.35! Self-evolution initiated.")
        elif self.current_stage == 1:
            self.stage_ticks += 1
            if self.stage_ticks >= 1:
                self.current_stage = 2
                self.stage_ticks = 0
                # Populate MAP-Elites niches
                for c_bin in range(5):
                    for v_bin in range(4):
                        if (c_bin + v_bin + tick) % 3 == 0:
                            self.map_elites[(c_bin, v_bin)] = min(99.9, 70.0 + random.random() * 29.5)
                self.record_event(f"Stage 3 MAP-ELITES: Explored 20 niches | Archive: {len(self.map_elites)*5}% covered")
        elif self.current_stage == 2:
            self.stage_ticks += 1
            if self.stage_ticks >= 1:
                self.current_stage = 3
                self.stage_ticks = 0
                self.record_event("Stage 4 SAFETY AUDIT: Verified all 7 Formal Safety Gates (Pass)")
        elif self.current_stage == 3:
            self.stage_ticks += 1
            if self.stage_ticks >= 1:
                self.current_stage = 4
                self.stage_ticks = 0
                self.generation += 1
                self.hot_swaps_count += 1
                target_act = regime["action"]
                act_name = ACTION_NAMES[target_act]
                self.active_iris = f"def policy(v: list<i64>) -> i64 effect alloc {{ return {target_act} }}"
                self.active_python = f"lambda v, _r=[{self.registers[0]},{self.registers[1]},...]: {target_act}"
                self.record_event(f"Stage 5 HOT-SWAP: Promoted Gen {self.generation} | Action -> {act_name}")
        elif self.current_stage == 4:
            self.stage_ticks += 1
            if self.stage_ticks >= 2:
                self.current_stage = 0
                self.stage_ticks = 0

    def _move_toward(self, tx: int, ty: int):
        if self.organism_x < tx and self.organism_x + 1 < self.width:
            self.organism_x += 1
        elif self.organism_x > tx and self.organism_x > 0:
            self.organism_x -= 1
        if self.organism_y < ty and self.organism_y + 1 < self.height:
            self.organism_y += 1
        elif self.organism_y > ty and self.organism_y > 0:
            self.organism_y -= 1

    def render(self, tick: int, paused: bool):
        regime = REGIMES[self.regime_idx]
        sys.stdout.write("\x1b[2J\x1b[H")

        print("╔═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╗")
        status_str = "\x1b[33m[PAUSED]\x1b[0m " if paused else "\x1b[32m[RUNNING]\x1b[0m"
        era_info = f"Era: {regime['name'][:35]} ({self.ambient_temp:>4.1f}°C)"
        print(f"║ IRIS CYBERNETIC WORLD | {era_info:<48} | [Tick:{tick:04d} Gen:{self.generation}] {status_str} ║")
        print("╠═══════════════════════════════════════════════╦═══════════════════════════════════════════════════════════════╣")

        # 2D World grid (12 rows)
        for y in range(self.height):
            row_str = ""
            for x in range(self.width):
                if self.is_alive and x == self.organism_x and y == self.organism_y:
                    row_str += "\x1b[1;37;44m@\x1b[0m "
                else:
                    sym, col = TILE_RENDER[self.grid[y][x]]
                    row_str += f"{col}{sym}\x1b[0m "

            right_panel = ""
            if y == 0:
                right_panel = f"ORGANISM PHYSIOLOGY & REGISTERS (Gen {self.generation})"
            elif y == 1:
                col = "\x1b[32m" if self.health > 50 else "\x1b[31m"
                right_panel = f"Health:    [{col}{self.health:>5.1f}%\x1b[0m]  Core Temp: {self.core_temp:>4.1f}°C"
            elif y == 2:
                col = "\x1b[32m" if self.energy > 30 else "\x1b[33m"
                act = ACTION_NAMES[self.active_action]
                right_panel = f"Energy:    [{col}{self.energy:>5.1f}%\x1b[0m]  Action:    \x1b[36m{act}\x1b[0m"
            elif y == 3:
                col = "\x1b[32m" if self.allostatic_load < 0.35 else "\x1b[31;1m"
                right_panel = f"Strain:    [{col}{self.allostatic_load:>5.2f}\x1b[0m]  Hot-Swaps: {self.hot_swaps_count}"
            elif y == 4:
                right_panel = f"Registers: R0:{self.registers[0]:<3} R1:{self.registers[1]:<3} R2:{self.registers[2]:<3} R3:{self.registers[3]:<3} R4..7: 0"
            elif y == 5:
                right_panel = f"Vectors:   V0:{self.vector_registers[0]} V1..3:[0,0,0,0]"
            elif y == 6:
                right_panel = "MAP-ELITES ARCHIVE (5x4 Niches)"
            elif y == 7:
                right_panel = "Comp\\Var | Flat | Low  | Med  | High (Dyn)"
            elif y in (8, 9, 10, 11):
                labels = ["Micro   ", "Compact ", "Moderate", "Complex "]
                c_idx = y - 8
                row = f"{labels[c_idx]} | "
                for v_idx in range(4):
                    if (c_idx, v_idx) in self.map_elites:
                        row += f"\x1b[32m{self.map_elites[(c_idx, v_idx)]:>4.0f}%\x1b[0m|"
                    else:
                        row += " -- |"
                right_panel = row

            print(f"║ {row_str:<46}║ {right_panel:<61}║")

        print("╠═══════════════════════════════════════════════╩═══════════════════════════════════════════════════════════════╣")

        # 5-stage pipeline indicator
        stages_ui = []
        for i, s_name in enumerate(STAGES):
            if i == self.current_stage:
                colors = ["\x1b[30;42m", "\x1b[30;41;1m", "\x1b[30;43m", "\x1b[30;44m", "\x1b[30;45;1m"]
                stages_ui.append(f"{colors[i]} {s_name} \x1b[0m")
            else:
                stages_ui.append(f"\x1b[90m {s_name[:12]} \x1b[0m")
        print(f"║ PIPELINE: {' -> '.join(stages_ui)} ║")
        print("╠═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╣")

        # 7-Gate Safety Audit
        g_str = "  ".join([f"\x1b[32m[✓] G{i+1}\x1b[0m" for i in range(7)])
        print(f"║ 7-GATE SAFETY AUDIT: {g_str}  [Status: VERIFIED SAFE]                                    ║")
        print("╠═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╣")

        print(f"║ ACTIVE POLICY (IRIS)  : {self.active_iris:<86}║")
        print(f"║ ACTIVE POLICY (PYTHON): {self.active_python:<86}║")
        print("╠═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╣")

        print("║ RECENT EVENT STREAM:                                                                                          ║")
        for ev in self.events:
            print(f"║  * {ev:<106}║")
        for _ in range(6 - len(self.events)):
            print("║                                                                                                               ║")

        print("╠═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╣")
        print("║ Controls: [Space] Pause/Resume  [s] Step  [e] Force Evolve  [r] Disaster  [1-4] Era  [+/-] Speed  [q] Quit     ║")
        print("╚═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╝")
        sys.stdout.flush()


def main():
    parser = argparse.ArgumentParser(description="IRIS Cybernetic World TUI Self-Evolution Simulator")
    parser.add_argument("--ticks", type=int, default=120, help="Total ticks (0 for infinite)")
    parser.add_argument("--fps", type=float, default=8.0, help="Frames per second")
    parser.add_argument("--seed", type=int, default=42, help="Random seed")
    parser.add_argument("--regime-duration", type=int, default=30, help="Ticks per era")
    args = parser.parse_args()

    sim = PythonWorldSim(regime_duration=args.regime_duration, seed=args.seed)
    tick = 0
    paused = False
    delay_s = 1.0 / args.fps if args.fps > 0 else 0.125

    while sim.is_alive and (args.ticks == 0 or tick < args.ticks):
        t0 = time.time()
        k = poll_key()
        if k:
            if k == " ":
                paused = not paused
                sim.record_event("Paused simulation" if paused else "Resumed simulation")
            elif k in ("q", "Q"):
                sim.record_event("User requested quit")
                break
            elif k in ("e", "E"):
                sim.current_stage = 1
                sim.record_event("User Event: Force-triggered self-evolution crisis!")
            elif k in ("r", "R"):
                sim.regime_idx = (sim.regime_idx + 1) % len(REGIMES)
                sim.ambient_temp = REGIMES[sim.regime_idx]["temp"]
                sim.record_event(f"User Event: Injected climate disaster -> {REGIMES[sim.regime_idx]['name']}")
            elif k in ("1", "2", "3", "4"):
                sim.regime_idx = int(k) - 1
                sim.ambient_temp = REGIMES[sim.regime_idx]["temp"]
            elif k in ("+", "="):
                delay_s = max(0.02, delay_s - 0.025)
            elif k in ("-", "_"):
                delay_s = min(1.0, delay_s + 0.025)
            elif k in ("s", "S"):
                paused = False

        if not paused:
            sim.step(tick)
            tick += 1

        sim.render(tick, paused)
        elapsed = time.time() - t0
        if elapsed < delay_s:
            time.sleep(delay_s - elapsed)

    if not sim.is_alive:
        print("\x1b[31;1mOrganism perished due to severe allostatic failure.\x1b[0m")


if __name__ == "__main__":
    main()
