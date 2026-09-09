# Integration contracts

`llm_protocol.iris`, `checked_network.iris`, `ffi_cells.iris`, and
`ros2_geometry.iris` have deterministic local checks in the learning catalog.
Geometry/QoS value calculations do not exercise DDS transport.

## Real HTTP/LLM requests

The [model gateway](../../projects/model_gateway/README.md) provides a configured
live client and a localhost HTTP fixture. Run its fixture without credentials:

```text
python projects/model_gateway/verify_local.py --iris target/release/iris.exe
```

The live client supports provider-neutral chat JSON. Token streaming and provider
formats outside that schema require an application adapter.

## Local model runtime

```powershell
$env:IRIS_ONNX_MODEL = 'C:\models\your-model.onnx'
$env:IRIS_NATIVE_ML_BACKENDS = '1'
iris run examples/10_integration/local_model.iris
```

Supply the ONNX Runtime SDK/library using the paths documented in
[ML integration](../../docs/integration/ml-integration.md). This example checks
session creation/cleanup; the model's input contract determines how to create
an MLTensor and call `local_model_run`. PyTorch and TensorFlow use their separate
backend adapters and ABI requirements. The default learning service trains in
IRIS and requires none of these external runtimes.

## ROS 2 middleware

```text
iris run examples/10_integration/ros2_topics.iris
```

Source the ROS environment first and build the bridge according to
[ros2-build.md](../../docs/ros2-build.md). The example publishes and reads a
signed fractional Vector3 using explicit QoS and a two-second receive timeout.
SDK/service entries are classified `manual`: CI checks compilation but does not
claim their external runtime or hardware was exercised.
