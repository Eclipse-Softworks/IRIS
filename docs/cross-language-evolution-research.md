# Cross-Language Code Genome Evolution: Architecture, Interop, and Autonomous Python Genomes

**Author**: IRIS Core Architecture & Intelligent Systems Group  
**Status**: Formal Research Specification & Architecture Document  
**Target Platform**: IRIS C-ABI Shared Library (`cdylib`), Python 3.8–3.14+ Interop, Polyglot AST Synthesis  

---

## 1. Executive Summary & Problem Formulation

In modern artificial intelligence and systems engineering, machine learning workflows, robotics environments (e.g., Gymnasium, MuJoCo), and distributed agent platforms are overwhelmingly orchestrated in higher-level dynamic languages—predominantly **Python**. However, runtime execution, genetic programming (GP) AST manipulation, combinatorial tree search, and memory-safe compilation require bare-metal execution speed and strict formal invariants, which are best provided by **IRIS**.

The fundamental challenge is:
> **How can a developer in Python (or C, C++, Go, Node.js) initialize, drive, and observe a self-evolving code genome that adapts to non-stationary conditions, while emitting native, executable Python code or executing natively via bare-metal C-ABI?**

This document establishes the theoretical and practical foundations for **polyglot code genome evolution** built directly into IRIS, enabling Python programs to autonomously synthesize, mutate, evaluate, and hot-swap their own executable Python genomes.

---

## 2. Polyglot Genome Synthesis Architecture

### 2.1 The Intermediate Genome Representation (IGR)
In IRIS, the code genome is structured as a typed abstract syntax tree (`GeneNode`). Rather than being hardcoded to IRIS syntax, `GeneNode` serves as a universal **Intermediate Genome Representation (IGR)**.

```
       ┌────────────────────────────────────────────────────────┐
       │             Intermediate Genome (GeneNode)             │
       │     (AST Tree: Ops, Logic, Arithmetic, Conditionals)   │
       └──────────────────────────┬─────────────────────────────┘
                                  │
         ┌────────────────────────┼─────────────────────────┐
         ▼                        ▼                         ▼
┌──────────────────┐    ┌──────────────────┐     ┌──────────────────┐
│  to_iris_expr()  │    │ to_python_expr() │     │   to_c_expr()    │
│ (Native IRIS JIT)│    │(Python Callable) │     │ (C99 Expression) │
└──────────────────┘    └──────────────────┘     └──────────────────┘
```

### 2.2 Syntax Mapping: IRIS vs. Python

To produce syntactically valid, idiomatic Python code from an evolved genome, the compiler translates IRIS AST semantics into Python AST semantics:

| Semantic Construct | IRIS Grammar (`to_iris_expr`) | Python Grammar (`to_python_expr`) |
| :--- | :--- | :--- |
| **Ternary Conditional** | `(if c { t } else { e })` | `(t if c else e)` |
| **Integer Division** | `(if d == 0 { 1 } else { n / d })` | `(1 if d == 0 else (n // d))` |
| **Modulo Division** | `(if d == 0 { 0 } else { n % d })` | `(0 if d == 0 else (n % d))` |
| **Logical Negation** | `(!x)` | `(not x)` |
| **Logical Conjunction** | `(a && b)` | `(a and b)` |
| **Logical Disjunction** | `(a || b)` | `(a or b)` |
| **Boolean Literals** | `true` / `false` | `True` / `False` |
| **Extrema Functions** | `(if a < b { a } else { b })` | `min(a, b)` |
| **Absolute Value** | `(if x < 0 { -x } else { x })` | `abs(x)` |

### 2.3 Two Execution Modes in Python

A Python application utilizing the IRIS evolution engine benefits from two complementary execution paradigms:

1. **Bare-Metal Native Execution (C-ABI)**:
   * Python calls `iris_genome_eval(genome_ptr, input_val)`.
   * Execution takes place directly in compiled Rust/C machine code without entering the Python bytecode interpreter loop (GIL-independent, high throughput).
2. **Pure Python AST Code Generation**:
   * Python retrieves the evolved expression string via `iris_genome_to_python_expr(genome_ptr)`.
   * Python constructs a first-class function using `eval(f"lambda x: {py_expr}")` or `compile()`.
   * The resulting callable is pure Python: it can be serialized with `pickle`, inspected with Python's standard `inspect` module, traced with `cProfile`, or deployed to environments that do not bundle native binaries.

---

## 3. C-ABI Dynamic Library Design

To enable seamless interop with Python (via standard library `ctypes`), C, C++, Go, and Rust, IRIS is configured as a `cdylib` generating `iris.dll` (Windows), `libiris.so` (Linux), and `libiris.dylib` (macOS).

### 3.1 Memory Safety and Handle Management

To preserve Rust's safety guarantees across the foreign function interface (FFI):
1. **Opaque Pointers**: Internal complex types (`GeneNode`, `EvolutionarySearch`, `Organism`) are boxed and exposed as opaque pointers (`*mut IrisGenome`, `*mut IrisSearchSession`, `*mut IrisLiveOrganism`).
2. **Explicit Deallocation**: Every allocated handle has a corresponding destructor (`iris_genome_free`, `iris_search_free`, `iris_organism_free`, `iris_string_free`).
3. **Panic Safety**: All `extern "C"` functions wrap their logic in `std::panic::catch_unwind` to prevent unwinding across foreign stack frames (which causes undefined behavior / aborts).

### 3.2 FFI API Specification

```rust
// Search Session Management
#[no_mangle]
pub unsafe extern "C" fn iris_evolution_search_create(
    pop_size: u32,
    generations: u32,
    mutation_rate: f64,
    crossover_rate: f64,
    parsimony_weight: f64,
    seed: u64,
) -> *mut IrisSearchSession;

#[no_mangle]
pub unsafe extern "C" fn iris_evolution_search_add_case(
    session: *mut IrisSearchSession,
    input: i64,
    expected: i64,
    weight: f64,
);

#[no_mangle]
pub unsafe extern "C" fn iris_evolution_search_run(
    session: *mut IrisSearchSession,
) -> *mut IrisGenome;

#[no_mangle]
pub unsafe extern "C" fn iris_evolution_search_free(session: *mut IrisSearchSession);

// Genome Inspection & Code Synthesis
#[no_mangle]
pub unsafe extern "C" fn iris_genome_eval(genome: *const IrisGenome, input: i64) -> i64;

#[no_mangle]
pub unsafe extern "C" fn iris_genome_to_iris_expr(genome: *const IrisGenome) -> *mut c_char;

#[no_mangle]
pub unsafe extern "C" fn iris_genome_to_python_expr(genome: *const IrisGenome) -> *mut c_char;

#[no_mangle]
pub unsafe extern "C" fn iris_genome_to_python_func(
    genome: *const IrisGenome,
    func_name: *const c_char,
    param_name: *const c_char,
) -> *mut c_char;

#[no_mangle]
pub unsafe extern "C" fn iris_genome_free(genome: *mut IrisGenome);
#[no_mangle]
pub unsafe extern "C" fn iris_string_free(s: *mut c_char);

// Autonomous Organism Simulation
#[repr(C)]
pub struct IrisOrganismStateC {
    pub health: f64,
    pub energy: f64,
    pub core_temp: f64,
    pub age: u64,
    pub allostatic_load: f64,
    pub generation: u64,
    pub active_action: i64,
    pub is_alive: bool,
    pub generation_changed: bool,
}

#[no_mangle]
pub unsafe extern "C" fn iris_organism_create(seed: u64, regime_duration: u32) -> *mut IrisLiveOrganism;

#[no_mangle]
pub unsafe extern "C" fn iris_organism_step(
    organism: *mut IrisLiveOrganism,
    ambient_temp: f64,
    resource_density: f64,
    hazard_intensity: f64,
) -> IrisOrganismStateC;

#[no_mangle]
pub unsafe extern "C" fn iris_organism_step_auto(
    organism: *mut IrisLiveOrganism,
) -> IrisOrganismStateC;

#[no_mangle]
pub unsafe extern "C" fn iris_organism_get_active_genome(
    organism: *const IrisLiveOrganism,
) -> *mut IrisGenome;

#[no_mangle]
pub unsafe extern "C" fn iris_organism_free(organism: *mut IrisLiveOrganism);
```

---

## 4. Python Native Wrapper: `iris_evolution`

The Python library is built with Python's standard library `ctypes`. This delivers critical advantages:
* **Zero External Dependencies**: Does not require `cffi`, `numpy`, `wheel`, `setuptools-rust`, or C compilers on the client system.
* **Universal Compatibility**: Functions identically across Python 3.8, 3.9, 3.10, 3.11, 3.12, 3.13, and 3.14+.
* **Automatic Library Discovery**: Automatically resolves `iris.dll`, `libiris.so`, or `libiris.dylib` across standard system paths, `IRIS_LIB_PATH` environment variable, or target directories.

### 4.1 Developer Ergonomics in Python

#### A. Direct Genome Evolution in Python
```python
from iris_evolution import EvolutionarySearch

# Initialize search engine from Python
search = EvolutionarySearch(pop_size=30, generations=25, parsimony_weight=0.01)

# Add target training observations
search.add_case(input_val=-5, expected_val=-15)
search.add_case(input_val=0, expected_val=0)
search.add_case(input_val=10, expected_val=30)

# Run evolutionary search
genome = search.run()

# Emits Python syntax: e.g. "lambda x: (x * 3)"
py_func = genome.to_callable()
print(py_func(7))  # Output: 21

# Emits Python source definition
print(genome.to_python_func(func_name="policy", param_name="x"))
```

#### B. Autonomous Self-Evolving Organism in Python
```python
from iris_evolution import AutonomousOrganism

organism = AutonomousOrganism(seed=42)

for tick in range(100):
    # Environmental state from Python simulation or gym
    state = organism.step(ambient_temp=45.0, resource_density=0.2, hazard_intensity=0.1)
    
    # Check if organism autonomously adapted its Python policy
    if state.generation_changed:
        py_callable = organism.get_active_callable()
        print(f"Tick {tick}: Hot-swapped Gen {state.generation}! New Python Policy: {organism.get_python_expr()}")
```

---

## 5. Security & Containment in Foreign Host Languages

When foreign languages evaluate dynamically evolved code, two attack surfaces exist:
1. **Host Language Exploits (Python `eval` risks)**:
   * Since IRIS `GeneNode` AST only allows pure arithmetic, boolean operations, and variables (`input`), it is mathematically impossible for an evolved expression to contain `__import__`, `os.system`, `eval`, or arbitrary attribute lookups.
   * Compiling the expression via `compile(expr, "<iris_genome>", "eval")` or `eval(code, {"__builtins__": {}})` guarantees complete sandbox containment.
2. **Memory Overflows**:
   * Tree depth limits (e.g. `max_depth = 4` to `6`) prevent stack overflow during AST traversal.
   * `DivChecked` and `ModChecked` protect against ZeroDivisionError and arithmetic panics.
