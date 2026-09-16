# IRIS Production Autonomic Microservice

An autonomic, self-governing microservice demonstrator written in native IRIS and backed by the IRIS Genetic Programming and Hot-Swap Engine.

## Architecture & Concepts

This service demonstrates the autonomic computing principles outlined in [Production-Grade Evolving Software Specification](../../docs/production-grade-evolving-software.md):

1. **MAPE-K Autonomic Loop**:
   - **Monitor**: Continuous sampling of real-time telemetry (P50, P95, P99 latency, HTTP status distribution, queue depth).
   - **Analyze**: Evaluation of multi-dimensional allostatic strain against SLA contracts (Target P99 $\le 15.0$ ms, Error budget $\le 1.0\%$).
   - **Plan**: Genetic programming candidate search and Pareto frontier optimization.
   - **Execute**: 7-gate safety verification and LLVM ORC JIT zero-downtime hot-swapping.
   - **Knowledge**: Genetic Memory Bank enabling $O(1)$ recall of proven policies during recurring adverse regimes.

2. **Production Load Regimes**:
   - **Nominal Traffic**: Standard balanced traffic (200 OK, full computation).
   - **Flash Crowd Surge**: 50x sudden concurrency spike triggering dynamic load shedding (429 Too Many Requests).
   - **Upstream Brownout**: Downstream latency degradation triggering intelligent cache fallback (203 Stale/Fast).
   - **Cache Thrashing**: High-entropy cache bypasses triggering adaptive rate throttling (202 Queued).

## Running the Demonstrator

### 1. Native IRIS Simulation
Run the native IRIS service simulation script:
```bash
iris run projects/autonomous_service/service.iris
```

### 2. Full Rust/LLVM JIT Autonomic Daemon
Run the production-grade daemon with live terminal telemetry, Prometheus metrics, and JSON health endpoints:
```bash
iris service-daemon --ticks 60 --target-p99 15.0 --error-budget 0.01
```

For automated monitoring or CI pipelines (headless mode with audit ledger export):
```bash
iris service-daemon --ticks 60 --headless --audit audit_ledger.json
```

### 3. Prometheus Metrics & Health Inspection
The service exposes standard Prometheus `/metrics` format:
```prometheus
# HELP iris_requests_total Total requests served by IRIS service
# TYPE iris_requests_total counter
iris_requests_total 60
# HELP iris_p99_latency_ms Moving window P99 latency in milliseconds
# TYPE iris_p99_latency_ms gauge
iris_p99_latency_ms 1.20
# HELP iris_allostatic_strain Multi-dimensional SLO strain (0.0=healthy, 1.0=critical)
# TYPE iris_allostatic_strain gauge
iris_allostatic_strain 0.0420
# HELP iris_service_generation Active hot-swapped code generation ID
# TYPE iris_service_generation gauge
iris_service_generation 4
# HELP iris_hot_swaps_total Total autonomous hot-swaps completed
# TYPE iris_hot_swaps_total counter
iris_hot_swaps_total 3
# HELP iris_cache_hit_ratio Moving window cache hit ratio
# TYPE iris_cache_hit_ratio gauge
iris_cache_hit_ratio 1.0000
```
