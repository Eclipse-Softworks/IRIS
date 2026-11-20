# TerraPulse (`projects/terra_pulse`)

**TerraPulse** is a live multi-source planetary telemetry, geophysics analytics, and observatory visualization application written natively in idiomatic **IRIS**.

Every execution fetches real, live wire data over HTTPS (WinHTTP TLS on Windows) from public geophysical, orbital, geomagnetic, and meteorological services — with zero placeholder or synthetic data in the runtime path.

## Live Telemetry Feeds Consumed

1. **USGS Earthquake Hazards Program (CSV)**:
   - `all_day`, `2.5_day` (default), `2.5_week`, and `4.5_week` live global catalogs.
   - Parsed by a custom RFC-4180 CSV parser (`parser.iris`) supporting quoted commas (for example, `"70 km E of Cedarville, California"`) and escaped quotes (`""`), with header-indexed column lookup and `?` / `??` validation.
2. **NOAA Space Weather Prediction Center (JSON)**:
   - Planetary geomagnetic storm index (`Kp`, `a_running`, station counts) across recent 3-hour intervals (`services.swpc.noaa.gov`).
3. **WhereTheISS Live Orbital Ephemeris (JSON)**:
   - Instantaneous ISS state vector (latitude, longitude, altitude km, velocity km/h, footprint km, daylight/eclipsed visibility, solar coordinates) via `api.wheretheiss.at`.
4. **Open-Meteo Dynamic Atmospheric Conditions (JSON)**:
   - Automatically geo-correlated against the top live USGS seismic epicenters and the live ISS sub-satellite ground track discovered in Stage 1 (`api.open-meteo.com`).

## Language Architecture & IRIS Features Exercised

- **Pure Domain Model (`domain.iris`)**: Validated WGS-84 coordinate constructors (`GeoCoord`), `choice` ADTs (`TelemetrySource`, `DepthRegime`, `AlertSeverity`, `TelemetryError`), `const def` physical constants, exhaustive `when`, `TelemetryPrintable` trait with `[T where T: TelemetryPrintable]` generic dispatch and `dyn TelemetryPrintable` dynamic collections.
- **RFC-4180 CSV & Structured JSON Parsers (`parser.iris`)**: Zero network/disk side effects, strict bounds checks returning `result<T, TelemetryError>`.
- **Two-Stage Concurrent Acquisition & Algebraic Effects (`telemetry.iris`)**:
  - `spawn` worker threads + bounded `channel` fan-out for Stage 1 (USGS + NOAA + ISS) and Stage 2 (dynamic Open-Meteo coordinates).
  - Scoped `handle ... with` handlers over algebraic effect declarations (`eff_fetch_resource`, `eff_record_trace`) tested deterministically offline while production uses `std.http.http_send`.
- **SIMD Kernels, Dereference Write-Back, Reverse-Mode AD & Bayesian Inference (`analytics.iris`)**:
  - Vectorized `par for` kernels over native `DiffTensor` (`--simd` compatible AVX2/scalar) plus `atomic` threshold counting.
  - `&mut` pointer selection + `*bucket += 1` dereference write-back (`tally_depth_regimes`).
  - Live **Gutenberg-Richter frequency-magnitude law** ($\log_{10} N(\ge M) = a - b M$) calibrated directly on fetched USGS earthquakes via IRIS native `tape`/`backward`/`grad` reverse-mode automatic differentiation.
  - **Normal-Inverse-Gamma** conjugate Bayesian 95% credible intervals via `std.uncertainty`.
- **SQLite Longitudinal History & Resource Safety (`store.iris`)**:
  - Parameterized `db_exec_params` / `db_query_params` (`std.sql`), immediate `defer { val _ = db_close(db) }` lifetime guards, and run-over-run delta metrics (`new_events_count`, Kp delta, GR `b`-value shift).
- **SVG Observatory Map, JSON Report, Live Web Server & Native Enterprise GUI (`render.iris` + `gui*.iris` + `main.iris`)**:
  - Interactive native desktop observatory (`--gui`) built on `std.ui` / `std.ui_protocol` with a cohesive dark Precision Graphite & Steel Cobalt chrome theme (`gui_theme.iris`), multi-view scientific canvas (`Global Map`, `Gutenberg-Richter AD`, `Kp & Run Archive`, `Atmospheric Probes`) in `gui_map.iris`, background telemetry worker (`spawn` + `channel` + pooled `ui_frame_pool_*` lifecycle in `gui.iris`), live epicenter selection, magnitude/depth/text filtering, mouse-wheel zoom, and on-demand Open-Meteo epicenter weather probing.
  - Global equirectangular SVG map (`std.svg`) plotting live earthquakes sized by magnitude and colored by depth regime, live ISS marker, dynamic weather pins, NOAA Kp bar chart, and AD/Bayesian panel.
  - Cooperative `async def` + `timeout_after` TLS preflight check, CLI option parsing (`args()`), `--gui` desktop launcher, and optional loopback HTTP dashboard (`--serve [port]`) exposing `/`, `/map.svg`, `/api/summary`, `/health`, and `/shutdown`.

## Usage

From the repository root (using a built `iris` binary):

```powershell
# Strict effect check across all 11 modules with SIMD kernel checks
iris check --strict-effects --simd projects/terra_pulse

# Format check
Get-ChildItem projects/terra_pulse/*.iris | ForEach-Object { iris fmt --check $_.FullName }

# Run all unit, headless GUI, and integration tests under --simd
$env:IRIS_NATIVE_TIMEOUT = "60"
Get-ChildItem projects/terra_pulse/*.iris | ForEach-Object { iris test --simd $_.FullName --no-color }

# Launch the interactive native Enterprise Desktop GUI (`--feed` / `--no-db` / `--db` supported)
iris run --simd projects/terra_pulse/main.iris -- --gui --feed 2.5_day

# Execute a live planetary run (real HTTPS + SQLite + SVG + JSON)
iris run --simd projects/terra_pulse/main.iris -- --feed 2.5_day

# Execute against the 24-hour all-magnitude feed and launch local web dashboard
iris run --simd projects/terra_pulse/main.iris -- --feed all_day --serve 8787

# Generate HTML API documentation from /// doc comments
iris docs projects/terra_pulse/main.iris -o terra_pulse_docs.html
```
