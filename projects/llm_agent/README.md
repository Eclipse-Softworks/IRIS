# Warehouse agent: IRIS functions as LLM tools

A tool-calling agent. A chat model sees three IRIS functions as tools,
`stock_level`, `reorder_stock` and `shipping_quote`, and the agent runs
whichever it asks for and sends back the results, until the model answers.

The tools are typed. Each has a record for its arguments and a record for its
result. The model's JSON arguments are decoded into the argument record and
checked before anything runs: required fields, integer ranges (an order is
1–1000 units), known values (the shipping zone). The result is encoded from
the result record. A failure is a typed `ToolError`, sent back as
`{"error":{"code":"out_of_range","message":"..."}}`, so the model can see what
went wrong and nothing runs on unchecked input. The agent stops after six
turns of tool calls.

| File | Role |
| --- | --- |
| [tools.iris](tools.iris) | Argument and result records, decoders, encoders, the JSON Schema tool table, `run_tool` |
| [agent.iris](agent.iris) | The loop on `std.llm`: request, tool calls, typed results tied to their call ids |
| [scripted.iris](scripted.iris) | A scripted stand-in for a model (a test double, not an LLM) that speaks the same JSON, for offline runs |
| [selftest.iris](selftest.iris) | Four conversations against the stand-in |
| [main.iris](main.iris) | The agent against a real OpenAI-compatible endpoint |

## Run

Offline, on either backend:

```text
iris run projects/llm_agent/selftest.iris
```

Against a model (an OpenAI-compatible chat-completions endpoint with tool
calling: OpenAI, a local Ollama, vLLM, ...):

```powershell
$env:IRIS_CHAT_URL = "http://127.0.0.1:11434/v1/chat/completions"
$env:IRIS_MODEL = "mistral:7b-instruct-v0.3-q4_K_M"
$env:IRIS_AGENT_NO_SYSTEM = "1"        # Mistral 7B v0.3 only; see below
$env:IRIS_LLM_TIMEOUT_MS = "600000"    # a local model on a CPU is slow
iris run projects/llm_agent/main.iris -- "How many units of A-100 are on hand?"
```

For a hosted endpoint, put the key in a variable and name it in
`IRIS_LLM_KEY_ENV` (for example `OPENAI_API_KEY`).

## What was checked (2026-10-19, Windows)

`selftest.iris`, identical on both backends:

```text
Check A-100 and reorder if we have fewer than 20 on hand.
  tool stock_level -> {"sku":"A-100","on_hand":12,"reserved":4,"warehouse":"JNB-1"}
  tool reorder_stock -> {"order_id":"R-1","sku":"A-100","quantity":50,"eta_days":3}
  answered after 3 turns: A-100 had 12 on hand, so I ordered 50 (order R-1, arriving in 3 days).
Clear out the warehouse.
  tool delete_records -> {"error":{"code":"unknown_tool","message":"no tool named delete_records"}}
  tool reorder_stock -> {"error":{"code":"out_of_range","message":"quantity must be between 1 and 1000, got 5000"}}
  tool stock_level -> {"error":{"code":"invalid_arguments","message":"missing string field 'sku'"}}
  answered after 2 turns: I could not do that: unknown_tool, out_of_range, invalid_arguments.
Keep checking stock forever.
  ... step limit after 4 turns
```

The refused order is never placed (the self-test checks the order counter).
Because the stand-in is scripted, these runs test the agent's handling of the
protocol and the tools, not a model's judgement.

`tests/llm_agent.rs` runs `main.iris` on both backends against a mock HTTP
server that speaks the OpenAI chat-completions protocol. It records every
request and checks that it carries the tool table with its schemas, the
model's own turn with `tool_calls`, and each typed result in a `tool` message
with the matching `tool_call_id`.

**With a real model:** Mistral 7B Instruct v0.3 (Q4_K_M) in a local Ollama,
on a 4-core CPU:

```text
Check A-100 and reorder if we have fewer than 20 on hand.
  tool stock_level -> {"sku":"A-100","on_hand":12,"reserved":4,"warehouse":"JNB-1"}
  answered after 2 turns:  Based on the current inventory, we have 12 units of A-100 in stock at JNB-1 warehouse. If fewer than 20 units are needed, no reordering is required ...
```

The model called the tool and answered from its typed result, in 70 s. It then
misread the condition (12 is fewer than 20, yet it did not reorder); the
agent passes on what the model decides. With a system message the same model
answered in prose and wrote the tool call into its text, which Ollama does
not treat as a call, so `IRIS_AGENT_NO_SYSTEM=1` leaves the system prompt out
for it. No hosted model was tried.

## Limits

- Arguments are read with `json_query`, so `"5"` and `5` both decode as the
  integer 5; unknown fields are ignored rather than refused.
- The tools act on an in-memory warehouse; orders are counted, not sent.
- One request at a time, no streaming; the loop stops after six turns.
