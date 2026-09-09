# IRIS RC1 projects

These projects progress from multi-file application logic to learning and native
code evolution. Their default entry points are deterministic and assertion-backed.
Run commands from the repository root. External services and physical hardware
are opt-in integrations documented in the relevant project.

| Project | Entry | Scope |
| --- | --- | --- |
| [Beginner: checked ledger](ledger/README.md) | `projects/ledger/main.iris` | No service, credentials, or files |
| [Intermediate: structured job pipeline](job_pipeline/README.md) | `projects/job_pipeline/main.iris` | No service or credentials |
| [Advanced: software-only learning service](learning_service/README.md) | `projects/learning_service/main.iris` | No hardware, datasets, or external model runtime |
| [Advanced: LLM application gateway](model_gateway/README.md) | `projects/model_gateway/main.iris` | The default run is offline |
| [Advanced: simulated actuator controller](robotic_actuator_control/README.md) | `projects/robotic_actuator_control/src/main.iris` | Default execution is simulation only: no serial connection, GPIO writes, or DDS transport |
| [Advanced: multimodal AIS pipeline](multimodal_ai_orchestrator/README.md) | `projects/multimodal_ai_orchestrator/src/main.iris` | No model SDK, ROS installation, or hardware |
| [Advanced: verified software evolution](autonomous_evolution_lab/README.md) | `projects/autonomous_evolution_lab/main.iris` | Default run needs no service |

The [example catalog](../examples/catalog.json) indexes every IRIS source as an
entry, reusable module, or explicit integration. It is checked by
`cargo test --test examples_showcase`.
