# Multi-Agent Artificial Life Ecosystem: Darwinian Neural Evolution

A 2D continuous spatial ecosystem demonstrating emergent multi-agent survival behaviors, neural network policy control, and open-ended Darwinian evolution written in native IRIS.

---

## Dynamics & Architecture

- **Neural Brain Controller**: Each agent possesses an embedded feedforward neural network policy mapping 6 sensory inputs (toroidal food vector, predator proximity vector, internal energy level, and bias) to 2 motor outputs (propulsive thrust and steering angle).
- **Darwinian Natural Selection**:
  - Organisms expend metabolic calories proportional to movement velocity and basal metabolic rate.
  - Efficient foragers accumulate surplus energy; upon crossing the reproduction threshold (Energy $\ge 75$), they undergo mitotic budding, passing a mutated genome to their offspring.
  - Agents whose energy reserves drop to zero perish, applying rigorous natural selection pressure.
- **Toroidal Spatial Ecology**: 2D wrapped topology ($100\text{m} \times 100\text{m}$) preventing boundary trapping, with stochastically regenerating food patches preserving ecological carrying capacity.

---

## Running the Simulation

Execute the 300-tick evolutionary testbench:

```powershell
.\target\release\iris.exe run projects/artificial_life_ecosystem/main.iris
```
