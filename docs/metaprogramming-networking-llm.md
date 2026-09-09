# Metaprogramming, Networking, and LLM Integration

This document describes the RC1 working-tree APIs that let IRIS inspect and
rewrite typed programs, communicate with services, call language models, and
run local ML backends. These capabilities are independent of hardware.

## Explicit returns

`return` is a reserved keyword and can return from any point in a function.
Both forms below are valid; public examples prefer the explicit form at process
boundaries because the exit value is immediately visible:

```iris
def main() -> i64 {
    return 0
}

def add_one(value: i64) -> i64 {
    return value + 1
}
```

A trailing semicolon after a return is optional.

## Typed metaprogramming without self-hosting

`iris meta program.iris` invokes the same Rust-hosted lexer, parser, type and
borrow checkers, effect analysis, SSA lowerer, optimizer, and ABI fingerprinting
pipeline used for normal compilation. It emits an `iris-meta-analysis/1` JSON
document containing typed functions and parameters, declared effects and type
parameters, public records and choices, ABI SHA-256 values, and optimized CFG
sizes. `--emit-ir` emits the verified IR.

`std.meta` exposes `MetaProgram`, `MetaEdit`, `meta_analyze`, `meta_emit_ir`, and
`meta_apply`. Edits use UTF-8 byte boundaries and are transactional: new source
is returned only if the complete compiler pipeline accepts the result. This is
compiler-hosted metaprogramming, so it does not require an IRIS-written compiler
or a duplicate type system. Interpreter/compiler-host execution provides the
service. Standalone native programs currently report `meta_available() ==
false`; they do not perform an unchecked local rewrite.

## Typed networking

`std.net` exposes public `TcpStream` and `TcpListener` handles with result-based
connect, listen, and accept operations; read/write timeouts; exact writes;
bounded reads (hard-capped at 16 MiB); half/full shutdown; and explicit close.
Transport failures are returned as `result<_, str>`.

`std.http` adds typed `HttpRequest` and `HttpResponse` records. `http_send`
supports GET, POST, PUT, PATCH, DELETE, arbitrary HTTP methods, caller-supplied
headers, request bodies, positive timeouts, response status codes, and a 64 MiB
response ceiling. A local native integration test verifies custom headers, JSON
body, HTTP 201 status, and response parsing end to end.

HTTPS uses the operating system WinHTTP stack on the validated Windows target,
including its normal certificate validation. `http_tls_available()` reports the
capability. Non-Windows and freestanding runtimes currently fail HTTPS closed;
cross-platform system-TLS adapters remain release work.

## Remote LLMs and embeddings

`std.llm` is provider-neutral at the transport layer and directly supports the
common OpenAI-compatible chat-completions and embeddings JSON shapes:

- `LlmClient` stores endpoints, model name, API-key environment variable, and
  timeout.
- `LlmMessage` and `LlmTool` build escaped JSON payloads, including tool schemas.
- `llm_chat`, `llm_chat_with_tools`, and `llm_embed` return typed results with
  status, content/tool calls or embedding JSON, and the raw provider response.
- Secrets are read from an environment variable at call time and are not stored
  in source or the client record.

See `projects/model_gateway/live.iris`. Endpoint differences beyond
the documented JSON shape can be handled with `HttpRequest` and `json_query`
directly.

## Local and custom models

`std.ml` provides `ModelSession`, `ModelHealth`, and `ModelRegistry` for versioned
model discovery, ONNX/PyTorch/TensorFlow loading, single or batch inference,
PyTorch training steps, close, failure-rate tracking, mean latency, and degraded
health. `std.llm.LocalModel` provides a smaller inference-oriented wrapper.
Actual backend use requires the corresponding SDK/runtime and a compatible
model artifact; pure lifecycle and failure logic is tested without those SDKs.

`std.ais.AgentRuntime` and `AgentStep` combine lifecycle state, observation and
reward histories, consecutive-failure degradation, homeostatic viability, and
active-inference action selection. When viability fails or the runtime degrades,
the production step selects the caller-provided emergency action.

These pieces allow software-only IRIS services to build model-backed autonomous
systems. They do not imply that an arbitrary model is safe, accurate, or ready
for unattended deployment; those properties still require application-specific
evaluation, monitoring, and rollback policy.
