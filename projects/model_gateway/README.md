# Advanced: LLM application gateway

The default program constructs provider-neutral chat requests and checks response parsing, error handling, and an application action allowlist. Model output is treated as data. live.iris sends a real request only when invoked explicitly.

## Run

```text
iris run projects/model_gateway/main.iris
```

The default run is offline. Live mode needs an HTTP chat endpoint, a model name, and optional credentials. Native HTTPS currently requires Windows.

## Verify

```text
python tools/verify_learning.py --filter projects/model_gateway/
```

The catalog runner checks the native and interpreter paths separately. Every
assertion is part of the program, so incorrect results fail the run.

## Extend

Connect your local model server. Preserve the action allowlist when changing prompts or providers.

## Live model service

```powershell
$env:IRIS_CHAT_URL = 'http://127.0.0.1:8080/v1/chat/completions'
$env:IRIS_MODEL = 'your-local-model'
# Optional: set IRIS_LLM_KEY in the environment without committing it.
iris run projects/model_gateway/live.iris
```

The server must accept the chat-completions JSON schema used by std.llm.
The client uses a five-second timeout and accepts only status/help actions.
It exits with an error on a provider failure or unsupported proposal.
Run the deterministic local HTTP integration fixture with:

```text
python projects/model_gateway/verify_local.py --iris target/release/iris.exe
```
