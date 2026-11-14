# Async quote gateway

An HTTP service where every request is an `async def` task on one thread.
A quote fans out to three suppliers at once and waits for them until a
150 ms deadline; calls still running at the deadline are cancelled. A race
endpoint takes the first answer and cancels the rest. Hundreds of requests
wait on their suppliers at the same time without a thread each.

| File | Role |
| --- | --- |
| [gateway.iris](gateway.iris) | Routing and the async handlers: `quote` (deadline with `timeout_after`, cancellation), `fastest` (`async_any`, `cancel_all`), `slow` (an upstream that never answers), stats |
| [main.iris](main.iris) | The HTTP/1.1 front end on 127.0.0.1, on the same thread: tasks accept connections (`tcp_accept_async`), read whole requests (`tcp_read_async`, 2 s limit) and answer them |
| [selftest.iris](selftest.iris) | The handlers without the network: each kind of request, then 300 at once |

| Request | Answer |
| --- | --- |
| `GET /quote/<sku>` | Best price among the suppliers that answered within 150 ms, with `answered` and `cancelled` counts |
| `GET /fastest/<sku>` | The first supplier to answer; the other calls are cancelled |
| `GET /slow` | 504 after 150 ms: its upstream never answers |
| `GET /stats` | `served`, `timeouts`, `cancelled_calls`, `peak_in_flight` |
| `POST /shutdown` | Finishes the requests in flight and exits |

The suppliers are simulated with timers. Each SKU fixes which supplier is
fastest (they answer 25 ms apart, 20–79 ms in all) and what each charges.
Supplier 2 never answers for a SKU starting with `rare`.

## Run

```powershell
$env:IRIS_GATEWAY_PORT = 8088
iris run projects/async_gateway/main.iris
curl.exe -s localhost:8088/quote/bolt
curl.exe -s localhost:8088/quote/rare-gear
curl.exe -s localhost:8088/slow
curl.exe -s -X POST localhost:8088/shutdown
```

## What was observed (2026-10-19, Windows, 4 cores)

`selftest.iris`: 300 quotes started together on one thread all answered in
85–95 ms natively (1.2 s interpreted). Each quote waits for its slowest
supplier (70–79 ms), so one after another they would take about 21 s.
Native run:

```text
{"sku":"bolt","best":116,"supplier":2,"answered":3,"cancelled":0,"elapsed_ms":89}
{"sku":"rare-gear","best":105,"supplier":1,"answered":2,"cancelled":1,"elapsed_ms":157} after 157 ms
{"sku":"bolt","price":116,"supplier":2,"cancelled":2}
300 concurrent quotes: 300 answered in 95 ms; peak in flight 300
{"served":305,"timeouts":1,"cancelled_calls":4,"peak_in_flight":300}
async gateway: deadlines, cancellation and 300 concurrent requests checked
```

`tests/async_gateway.rs` runs `main.iris` on both backends and sends it 64
requests from 64 threads at once: quotes, rare quotes, races and `/slow`.
Every status and count is checked, including that the cancelled calls are
exactly the hung ones plus each race's losers. Before 2026-10-20, over
three runs, with a worker thread accepting and reading requests:

| Backend | 64 requests | Peak in flight |
| --- | --- | --- |
| native | 170–192 ms | 64 |
| interpreter | 358–538 ms | 24–34 |

With everything on one thread (awaitable accepts and reads), one run:
native 177 ms, peak 64; interpreter 403 ms, peak 25.

## Limits

- **Writes block.** Accepting and reading wait on socket readiness, so a
  slow client holds up nothing but its own task; writing a reply blocks the
  thread until the kernel takes it. Replies are small, and the kernel
  buffers them.
- **Supplier calls are timers**, not network calls. There is no async HTTP
  client: connects and HTTP calls still block
  ([known issue #72](../../docs/known-issues.md)).
- **Task handles are kept until exit** natively (about 200 bytes per request,
  [known issue #72](../../docs/known-issues.md)): this gateway is a
  demonstration, not a long-running server.
- **One request per connection**, `Content-Length` bodies only, no TLS.
