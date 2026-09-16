# Official IRIS Multi-Language Benchmark Results

Comprehensive performance benchmarking of **IRIS** against industry-standard systems and application languages:

- **C (Clang 23.1.1)**: `-O3 -fomit-frame-pointer`
- **Rust (rustc 1.91.1)**: `-C opt-level=3`
- **IRIS (1.0.0-rc1)**: `--emit binary` (LLVM 23.1.1 Aggressive L3 + LLD)
- **Go (1.25.4)**: `go build`
- **Node.js (v24.11.0)**: Google V8 JIT
- **Python (3.14.0)**: CPython

### FIB Benchmark
| Language | Median Time (ms) | Min (ms) | Relative to C | Peak RSS (MB) | Status |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **C (Clang 23)** | 156.00 ms | 155.53 ms | **1.00x** | 3.2 MB | Verified |
| **Rust** | 156.53 ms | 156.52 ms | **1.00x** | 3.3 MB | Verified |
| **IRIS (LLVM 23)** | 162.48 ms | 161.16 ms | **1.04x** | 4.4 MB | Verified |
| **Go** | 271.91 ms | 270.46 ms | 1.74x | 4.5 MB | Verified |
| **Node.js** | 724.79 ms | 721.19 ms | 4.65x | 45.1 MB | Verified |
| **Python** | 11022.14 ms | 10712.91 ms | 70.65x | 10.0 MB | Verified |

### COLLATZ Benchmark
| Language | Median Time (ms) | Min (ms) | Relative to C | Peak RSS (MB) | Status |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **Rust** | 87.77 ms | 87.60 ms | **0.99x** | 3.3 MB | Verified |
| **IRIS (LLVM 23)** | 87.83 ms | 86.92 ms | **0.99x** | 4.3 MB | Verified |
| **C (Clang 23)** | 88.57 ms | 88.47 ms | **1.00x** | 3.2 MB | Verified |
| **Go** | 171.76 ms | 170.97 ms | 1.94x | 4.4 MB | Verified |
| **Node.js** | 767.23 ms | 760.58 ms | 8.66x | 45.3 MB | Verified |
| **Python** | 8405.49 ms | 8216.00 ms | 94.91x | 10.0 MB | Verified |

### SIEVE Benchmark
| Language | Median Time (ms) | Min (ms) | Relative to C | Peak RSS (MB) | Status |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **C (Clang 23)** | 10.34 ms | 9.77 ms | **1.00x** | 3.8 MB | Verified |
| **Rust** | 12.26 ms | 11.74 ms | 1.19x | 3.9 MB | Verified |
| **Go** | 13.53 ms | 12.27 ms | 1.31x | 5.2 MB | Verified |
| **IRIS (LLVM 23)** | 15.30 ms | 15.16 ms | 1.48x | 6.6 MB | Verified |
| **Python** | 67.52 ms | 63.24 ms | 6.53x | 10.8 MB | Verified |
| **Node.js** | 73.13 ms | 71.54 ms | 7.07x | 45.6 MB | Verified |

### MATMUL Benchmark
| Language | Median Time (ms) | Min (ms) | Relative to C | Peak RSS (MB) | Status |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **Rust** | 44.19 ms | 43.38 ms | **1.00x** | 4.4 MB | Verified |
| **C (Clang 23)** | 44.31 ms | 43.92 ms | **1.00x** | 4.5 MB | Verified |
| **Go** | 50.13 ms | 47.94 ms | 1.13x | 5.8 MB | Verified |
| **IRIS (LLVM 23)** | 67.79 ms | 66.32 ms | 1.53x | 8.8 MB | Verified |
| **Node.js** | 139.90 ms | 137.77 ms | 3.16x | 48.4 MB | Verified |
| **Python** | 3584.20 ms | 3261.34 ms | 80.88x | 17.6 MB | Verified |

### QUICKSORT Benchmark
| Language | Median Time (ms) | Min (ms) | Relative to C | Peak RSS (MB) | Status |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **C (Clang 23)** | 16.95 ms | 16.65 ms | **1.00x** | 3.8 MB | Verified |
| **Rust** | 18.73 ms | 18.51 ms | 1.11x | 3.6 MB | Verified |
| **Go** | 21.80 ms | 21.69 ms | 1.29x | 5.1 MB | Verified |
| **IRIS (LLVM 23)** | 25.13 ms | 24.45 ms | 1.48x | 6.6 MB | Verified |
| **Node.js** | 94.18 ms | 92.16 ms | 5.56x | 47.7 MB | Verified |
| **Python** | 370.12 ms | 345.84 ms | 21.84x | 13.9 MB | Verified |

### Relative Performance Overview (Lower is Faster, C = 1.00x)

```
[FIB]
  C (Clang 23)     | ##                                    1.00x (156.0ms)
  Rust             | ##                                    1.00x (156.5ms)
  IRIS (LLVM 23)   | ##                                    1.04x (162.5ms)
  Go               | ###                                   1.74x (271.9ms)
  Node.js          | #########                             4.65x (724.8ms)
  Python           | ###################################  70.65x (11022.1ms)

[COLLATZ]
  Rust             | #                                     0.99x (87.8ms)
  IRIS (LLVM 23)   | #                                     0.99x (87.8ms)
  C (Clang 23)     | ##                                    1.00x (88.6ms)
  Go               | ###                                   1.94x (171.8ms)
  Node.js          | #################                     8.66x (767.2ms)
  Python           | ###################################  94.91x (8405.5ms)

[SIEVE]
  C (Clang 23)     | ##                                    1.00x (10.3ms)
  Rust             | ##                                    1.19x (12.3ms)
  Go               | ##                                    1.31x (13.5ms)
  IRIS (LLVM 23)   | ##                                    1.48x (15.3ms)
  Python           | #############                         6.53x (67.5ms)
  Node.js          | ##############                        7.07x (73.1ms)

[MATMUL]
  Rust             | #                                     1.00x (44.2ms)
  C (Clang 23)     | ##                                    1.00x (44.3ms)
  Go               | ##                                    1.13x (50.1ms)
  IRIS (LLVM 23)   | ###                                   1.53x (67.8ms)
  Node.js          | ######                                3.16x (139.9ms)
  Python           | ###################################  80.88x (3584.2ms)

[QUICKSORT]
  C (Clang 23)     | ##                                    1.00x (16.9ms)
  Rust             | ##                                    1.11x (18.7ms)
  Go               | ##                                    1.29x (21.8ms)
  IRIS (LLVM 23)   | ##                                    1.48x (25.1ms)
  Node.js          | ###########                           5.56x (94.2ms)
  Python           | ###################################  21.84x (370.1ms)

```
