# Quantum Wave Mechanics & Universal Quantum Circuit Simulator

A high-precision quantum physics and quantum computing engine written in native IRIS. 

The simulator combines continuous wave mechanics (1D Time-Dependent Schrödinger Equation) with discrete quantum computing registers ($N$-qubit statevectors, unitary quantum gates, entanglement synthesis, and Grover's search algorithm).

---

## Capabilities

- **Complex Arithmetic Engine**: Native complex algebra ($a + bi$), polar conversions, and Euler's formula $e^{i\theta} = \cos\theta + i\sin\theta$.
- **1D Time-Dependent Schrödinger Solver**:
  - Simulates Gaussian wavepacket dynamics incident on a rectangular potential barrier.
  - Demonstrates non-classical quantum tunneling where particle kinetic energy $E < V_0$, measuring transmission and reflection coefficients while preserving unitary probability norm.
- **N-Qubit Quantum Statevector Register**:
  - Full $2^N$-dimensional complex amplitude statevectors.
  - Universal quantum gate set: Hadamard ($H$), Pauli-$X$ (NOT), Pauli-$Y$, Pauli-$Z$, Phase, and Controlled-NOT ($CNOT$).
- **Quantum Algorithms**:
  - *Bell State Synthesizer*: Generates maximally entangled EPR pairs ($|\Phi^+\rangle = \frac{|00\rangle + |11\rangle}{\sqrt{2}}$).
  - *Grover's Search Algorithm*: 3-qubit database search demonstrating quadratic quantum speedup and amplitude amplification, driving measurement probability of marked states from $12.5\%$ to $> 94.5\%$.

---

## Running the Quantum Engine

Execute the complete quantum verification suite:

```powershell
.\target\release\iris.exe run projects/quantum_simulator/main.iris
```
