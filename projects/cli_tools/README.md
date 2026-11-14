# Command-line tools

Four tools written in IRIS, each a single file run with
`iris run projects/cli_tools/<tool>.iris -- <arguments>` (or built once with
`iris --emit binary -o tool.exe <tool>.iris`):

| Tool | What it does |
| --- | --- |
| [csvq](csvq.iris) | Filter (`--where region=EU`, `ms>=500`), select, group with exact money sums (`--group-by region --sum amount`), and convert CSV to CSV or JSON. Reads RFC 4180 CSV, where quoted fields may hold commas, doubled quotes and line breaks, and JSON Lines (`--jsonl --select user,meta.ok`) |
| [logstat](logstat.iris) | Summarize a web access log (combined log format): status classes, bytes, busiest path, clients with many 404s (probing for files), the minute with the most 5xx. Lines that don't fit are counted, not guessed at |
| [pcapinspect](pcapinspect.iris) | Packets, bytes, protocols, flows, top talker and SYN scans in a libpcap capture, in either byte order |
| [tlsfp](tlsfp.iris) | JA3 and JA4 fingerprints of every TLS ClientHello in a capture; `--known fingerprints.csv` labels the ones you list |

The logic lives in [csvkit.iris](csvkit.iris), [logs.iris](logs.iris) and
[packets.iris](packets.iris), on top of `std.pcap` and `std.tls`.

```text
> iris run projects/cli_tools/csvq.iris -- projects/cli_tools/samples/orders.csv --group-by region --sum amount
region,count,sum_amount
APAC,13,18082.59
EU,13,15330.98
US,14,18843.39

> iris run projects/cli_tools/pcapinspect.iris -- projects/cli_tools/samples/capture.pcap
packets 47, bytes 4119
ipv4 46: tcp 43, udp 2, icmp 1; other 1
flows 38
top talker 10.0.0.5 (1779 bytes)
syn scan 203.0.113.9 -> 10.0.0.20: 12 ports
tls client hellos 5

> iris run projects/cli_tools/tlsfp.iris -- projects/cli_tools/samples/capture.pcap
packet 3  10.0.0.5 -> 93.184.216.34:443  sni example.com
  ja3 657a105ca838d7171613c035060a5fed
  ja4 t13d3112h2_e8f1e7e78f70_24695f2957a7
...
5 client hellos, 0 known
```

## How it was checked

[samples/make_samples.py](samples/make_samples.py) writes the inputs and
computes every expected answer with its own implementation, separate from
the IRIS code:

- **capture.pcap**, little-endian as tcpdump writes it on x86. It carries five
  ClientHellos, two from Python's OpenSSL 1.1.1k captured over loopback and
  three built by the script: GREASE in every list, an ALPN that is not
  alphanumeric with no SNI, and no extensions at all. Around them are HTTP, a
  12-port SYN scan, DNS, ICMP, ARP, and short frames with Ethernet padding.
- **access.log**: 400 requests, a client probing for files, a 503 burst, and
  3 malformed lines.
- **orders.csv** (quoted commas, quotes, line breaks; CRLF) and
  **events.jsonl** (nested fields).

JA3 follows Salesforce's definition and JA4 FoxIO's. One check is
independent of both implementations here: for the browser-like ClientHello,
JA4 part b is `8daaf6152771`, the value FoxIO publishes for that cipher list.
The JA3 MD5 of the example string in Salesforce's JA3 README,
`ada70206e40642a3e4461f35503241d5`, is one of the MD5 test vectors.

`tests/cli_tools.rs` runs `selftest.iris` and each tool with real arguments,
natively and in the interpreter. The two backends must print the same bytes,
and those must match `expected.json`. The test also parses `csvq --json`
output as JSON and checks that bad input is reported with exit code 1.
Results on 2026-10-19 (Windows): all checks pass; the self-test takes 0.5 s
natively and 3 s interpreted.

## Limits

- **pcap:** classic libpcap only (not pcapng), Ethernet link type only, IPv4
  only. Each TCP segment is examined on its own: a ClientHello split across
  segments is not seen, and streams are not reassembled.
- **csvq:** values are text; numeric filters and sums treat a value as a
  decimal with at most two fractional digits. Group output is sorted by
  byte order.
- **logstat:** only the combined (or common) log format.
- **Size:** files are read whole (`file_read_bytes` keeps one `i64` per
  byte); the samples are kilobytes, and nothing here has been tried on
  captures of hundreds of megabytes.
