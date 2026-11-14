"""Write the sample inputs for the IRIS command-line tools, and the answers
an independent implementation (this file) computes for them.

    python make_samples.py

Outputs, next to this file:
  capture.pcap      libpcap, little-endian (as tcpdump writes on x86): TLS
                    ClientHellos, HTTP, a SYN scan, DNS, ICMP and ARP
  access.log        combined log format, with a scanner and an error burst
  orders.csv        RFC 4180: quoted fields with commas, quotes and newlines
  events.jsonl      JSON Lines
  expected.json     what the tools must report

Two of the ClientHellos come from this Python's OpenSSL, captured over
loopback; the others are built here (GREASE values, an ALPN that is not
alphanumeric, no extensions). JA3 follows Salesforce's definition and JA4
FoxIO's; both are implemented below from those specifications, separately
from the IRIS code they check.
"""

import csv
import hashlib
import io
import json
import random
import socket
import ssl
import struct
import threading
from collections import Counter, defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
GREASE = {0x0A0A + 0x1010 * i for i in range(16)}

# ── TLS ClientHellos ─────────────────────────────────────────────────────────


def captured_client_hello(server_name, configure):
    """The first TLS record this Python's OpenSSL sends to a local listener."""
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    port = listener.getsockname()[1]

    def client():
        context = ssl.create_default_context()
        configure(context)
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=5) as raw:
                context.wrap_socket(raw, server_hostname=server_name)
        except (OSError, ssl.SSLError):
            pass

    thread = threading.Thread(target=client)
    thread.start()
    conn, _ = listener.accept()
    conn.settimeout(5)
    data = b""
    while len(data) < 5 or len(data) < 5 + struct.unpack(">H", data[3:5])[0]:
        chunk = conn.recv(65536)
        if not chunk:
            break
        data += chunk
    conn.close()
    listener.close()
    thread.join()
    return data[: 5 + struct.unpack(">H", data[3:5])[0]]


def u16(value):
    return struct.pack(">H", value)


def u16_list(values, length_bytes=2):
    body = b"".join(u16(v) for v in values)
    return (len(body).to_bytes(length_bytes, "big")) + body


def ext_sni(name):
    entry = b"\x00" + u16(len(name)) + name.encode()
    return u16(len(entry)) + entry


def ext_alpn(protocols):
    body = b"".join(bytes([len(p)]) + p for p in protocols)
    return u16(len(body)) + body


def built_client_hello(version, ciphers, extensions):
    body = u16(version) + bytes(range(32)) + b"\x00" + u16_list(ciphers) + b"\x01\x00"
    ext = b"".join(u16(kind) + u16(len(data)) + data for kind, data in extensions)
    body += u16(len(ext)) + ext
    handshake = b"\x01" + len(body).to_bytes(3, "big") + body
    return b"\x16\x03\x01" + u16(len(handshake)) + handshake


def browser_like():
    """GREASE in every list, TLS 1.3 in supported_versions, ALPN h2."""
    return built_client_hello(
        0x0303,
        [0x0A0A, 0x1301, 0x1302, 0x1303, 0xC02B, 0xC02F, 0xC02C, 0xC030, 0xCCA9,
         0xCCA8, 0xC013, 0xC014, 0x009C, 0x009D, 0x002F, 0x0035],
        [
            (0x1A1A, b""),
            (0, ext_sni("intranet.example")),
            (23, b""),
            (65281, b"\x00"),
            (10, u16_list([0x2A2A, 29, 23, 24])),
            (11, b"\x01\x00"),
            (35, b""),
            (16, ext_alpn([b"h2", b"http/1.1"])),
            (5, b"\x01\x00\x00\x00\x00"),
            (13, u16_list([0x0403, 0x0804, 0x0401, 0x0503, 0x0805, 0x0501, 0x0806, 0x0601])),
            (18, b""),
            (51, u16(36) + u16(29) + u16(32) + bytes(32)),
            (45, b"\x01\x01"),
            (43, bytes([6]) + u16(0x3A3A) + u16(0x0304) + u16(0x0303)),
            (27, b"\x02\x00\x02"),
            (0x4A4A, b"\x00"),
        ],
    )


def odd_alpn_no_sni():
    """No SNI (so 'i'), an ALPN whose first byte is not alphanumeric, no
    signature algorithms (so JA4_c has no '_' part)."""
    return built_client_hello(
        0x0303,
        [0x002F, 0x0035, 0x000A],
        [(16, ext_alpn([b"@x#"])), (10, u16_list([23])), (11, b"\x03\x00\x01\x02")],
    )


def legacy_no_extensions():
    return built_client_hello(0x0301, [0x002F, 0x0035], [])


def parse_client_hello(record):
    hs = record[5:]
    assert hs[0] == 1
    body = hs[4:]
    version = struct.unpack(">H", body[0:2])[0]
    at = 34
    at += 1 + body[at]
    cs_len = struct.unpack(">H", body[at:at + 2])[0]
    ciphers = [struct.unpack(">H", body[at + 2 + i:at + 4 + i])[0] for i in range(0, cs_len, 2)]
    at += 2 + cs_len
    at += 1 + body[at]
    hello = {"version": version, "ciphers": [c for c in ciphers if c not in GREASE],
             "extensions": [], "sni": "", "alpn": None, "groups": [], "formats": [],
             "sigalgs": [], "versions": []}
    if at + 2 > len(body):
        return hello
    end = at + 2 + struct.unpack(">H", body[at:at + 2])[0]
    at += 2
    while at + 4 <= end:
        kind, length = struct.unpack(">HH", body[at:at + 4])
        data = body[at + 4:at + 4 + length]
        at += 4 + length
        if kind not in GREASE:
            hello["extensions"].append(kind)
        if kind == 0:
            name_len = struct.unpack(">H", data[3:5])[0]
            hello["sni"] = data[5:5 + name_len].decode()
        elif kind == 16:
            hello["alpn"] = data[3:3 + data[2]]
        elif kind == 10:
            hello["groups"] = [g for g in (struct.unpack(">H", data[i:i + 2])[0]
                                           for i in range(2, 2 + struct.unpack(">H", data[:2])[0], 2)) if g not in GREASE]
        elif kind == 11:
            hello["formats"] = list(data[1:1 + data[0]])
        elif kind == 13:
            hello["sigalgs"] = [s for s in (struct.unpack(">H", data[i:i + 2])[0]
                                            for i in range(2, 2 + struct.unpack(">H", data[:2])[0], 2)) if s not in GREASE]
        elif kind == 43:
            hello["versions"] = [v for v in (struct.unpack(">H", data[i:i + 2])[0]
                                             for i in range(1, 1 + data[0], 2)) if v not in GREASE]
    return hello


def ja3(hello):
    fields = [str(hello["version"])] + [
        "-".join(str(v) for v in hello[key]) for key in ("ciphers", "extensions", "groups", "formats")
    ]
    text = ",".join(fields)
    return text, hashlib.md5(text.encode()).hexdigest()


def ja4(hello):
    version = max(hello["versions"]) if hello["versions"] else hello["version"]
    names = {0x0304: "13", 0x0303: "12", 0x0302: "11", 0x0301: "10", 0x0300: "s3",
             0x0002: "s2", 0xFEFF: "d1", 0xFEFD: "d2", 0xFEFC: "d3"}
    sni = "d" if 0 in hello["extensions"] else "i"
    alpn = hello["alpn"]
    if not alpn:
        alpn_pair = "00"
    elif chr(alpn[0]).isalnum() and chr(alpn[-1]).isalnum() and alpn[0] < 128 and alpn[-1] < 128:
        alpn_pair = chr(alpn[0]) + chr(alpn[-1])
    else:
        hexed = alpn.hex()
        alpn_pair = hexed[0] + hexed[-1]
    part_a = "t" + names.get(version, "00") + sni + "%02d" % min(len(hello["ciphers"]), 99) \
        + "%02d" % min(len(hello["extensions"]), 99) + alpn_pair
    ciphers = sorted(hello["ciphers"])
    part_b = hashlib.sha256(",".join("%04x" % c for c in ciphers).encode()).hexdigest()[:12] \
        if ciphers else "000000000000"
    extensions = sorted(e for e in hello["extensions"] if e not in (0, 16))
    text = ",".join("%04x" % e for e in extensions)
    if hello["sigalgs"]:
        text += "_" + ",".join("%04x" % s for s in hello["sigalgs"])
    part_c = hashlib.sha256(text.encode()).hexdigest()[:12] if extensions else "000000000000"
    return part_a + "_" + part_b + "_" + part_c


# ── Packets ──────────────────────────────────────────────────────────────────


def ip_bytes(text):
    return bytes(int(part) for part in text.split("."))


def checksum(data):
    if len(data) % 2:
        data += b"\x00"
    total = sum(struct.unpack(">%dH" % (len(data) // 2), data))
    while total > 0xFFFF:
        total = (total & 0xFFFF) + (total >> 16)
    return ~total & 0xFFFF


def ethernet(payload, ethertype=0x0800, src=b"\x02\x00\x00\x00\x00\x01", dst=b"\x02\x00\x00\x00\x00\x02"):
    frame = dst + src + u16(ethertype) + payload
    return frame + bytes(max(0, 60 - len(frame)))  # minimum frame: padding


def ipv4(src, dst, proto, payload, ident):
    header = struct.pack(">BBHHHBBH4s4s", 0x45, 0, 20 + len(payload), ident, 0x4000, 64, proto, 0,
                         ip_bytes(src), ip_bytes(dst))
    header = header[:10] + u16(checksum(header)) + header[12:]
    return header + payload


def tcp(sport, dport, flags, payload=b"", seq=1000, ack=0):
    return struct.pack(">HHIIBBHHH", sport, dport, seq, ack, 5 << 4, flags, 64240, 0, 0) + payload


def udp(sport, dport, payload):
    return struct.pack(">HHHH", sport, dport, 8 + len(payload), 0) + payload


SYN, ACK, PSH, RST = 0x02, 0x10, 0x08, 0x04


def build_capture(hellos):
    packets = []
    ident = 1

    def add(src, dst, proto, payload, ethertype=0x0800):
        nonlocal ident
        if ethertype == 0x0800:
            packets.append(ethernet(ipv4(src, dst, proto, payload, ident)))
        else:
            packets.append(ethernet(payload, ethertype))
        ident += 1

    servers = ["93.184.216.34", "93.184.216.34", "10.0.0.20", "10.0.0.20", "192.0.2.80"]
    for index, record in enumerate(hellos):
        client_port = 50100 + index
        server = servers[index % len(servers)]
        add("10.0.0.5", server, 6, tcp(client_port, 443, SYN))
        add(server, "10.0.0.5", 6, tcp(443, client_port, SYN | ACK, ack=1001))
        add("10.0.0.5", server, 6, tcp(client_port, 443, PSH | ACK, record, seq=1001))
    for path in ("/", "/api/orders?page=2"):
        request = ("GET %s HTTP/1.1\r\nHost: shop.internal\r\n\r\n" % path).encode()
        add("10.0.0.7", "10.0.0.20", 6, tcp(51000, 80, PSH | ACK, request))
        add("10.0.0.20", "10.0.0.7", 6, tcp(80, 51000, PSH | ACK, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"))
    for port in range(20, 32):
        add("203.0.113.9", "10.0.0.20", 6, tcp(40000, port, SYN))
        add("10.0.0.20", "203.0.113.9", 6, tcp(port, 40000, RST | ACK))
    query = b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x04shop\x08internal\x00\x00\x01\x00\x01"
    add("10.0.0.5", "10.0.0.1", 17, udp(53001, 53, query))
    add("10.0.0.1", "10.0.0.5", 17, udp(53, 53001, query + b"\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x3c\x00\x04\x0a\x00\x00\x14"))
    add("10.0.0.7", "10.0.0.1", 1, b"\x08\x00\xf7\xff\x00\x01\x00\x01")
    add("", "", 0, bytes(28), ethertype=0x0806)

    out = struct.pack("<IHHiIII", 0xA1B2C3D4, 2, 4, 0, 0, 65535, 1)
    for number, frame in enumerate(packets):
        out += struct.pack("<IIII", 1792400000 + number, 1000 * number, len(frame), len(frame)) + frame
    return out, packets


def summarize(frames):
    protocols = Counter()
    talkers = Counter()
    syn_ports = defaultdict(set)
    flows = set()
    ipv4_packets = 0
    for frame in frames:
        if struct.unpack(">H", frame[12:14])[0] != 0x0800:
            protocols["other"] += 1
            continue
        ipv4_packets += 1
        ip = frame[14:]
        total = struct.unpack(">H", ip[2:4])[0]
        proto = ip[9]
        src = ".".join(str(b) for b in ip[12:16])
        dst = ".".join(str(b) for b in ip[16:20])
        talkers[src] += len(frame)
        payload = ip[20:total]
        name = {6: "tcp", 17: "udp", 1: "icmp"}.get(proto, "other")
        protocols[name] += 1
        if proto in (6, 17):
            sport, dport = struct.unpack(">HH", payload[:4])
            flows.add((name, src, sport, dst, dport))
        if proto == 6:
            flags = payload[13]
            if flags & SYN and not flags & ACK:
                syn_ports[(src, dst)].add(struct.unpack(">H", payload[2:4])[0])
    scans = sorted("%s -> %s: %d ports" % (s, d, len(p)) for (s, d), p in syn_ports.items() if len(p) >= 10)
    return {
        "packets": len(frames),
        "bytes": sum(len(f) for f in frames),
        "ipv4": ipv4_packets,
        "tcp": protocols["tcp"],
        "udp": protocols["udp"],
        "icmp": protocols["icmp"],
        "other": protocols["other"],
        "flows": len(flows),
        "top_talker": talkers.most_common(1)[0][0],
        "top_talker_bytes": talkers.most_common(1)[0][1],
        "scans": scans,
    }


# ── Logs ─────────────────────────────────────────────────────────────────────


def build_log():
    rng = random.Random(7)
    clients = ["198.51.100.%d" % i for i in range(2, 14)]
    pages = ["/", "/index.html", "/products", "/products/42", "/cart", "/api/orders", "/static/app.js"]
    probes = ["/wp-admin", "/.env", "/phpmyadmin", "/admin.php", "/.git/config", "/backup.zip"]
    lines = []
    for second in range(400):
        minute, sec = divmod(second, 60)
        stamp = "18/Oct/2026:09:%02d:%02d +0000" % (minute, sec)
        if second % 7 == 3:
            ip, path, status = "203.0.113.66", probes[second % len(probes)], 404
        elif 200 <= second < 230:
            ip, path, status = rng.choice(clients), "/api/orders", 503
        else:
            ip, path = rng.choice(clients), rng.choice(pages)
            status = 200 if rng.random() < 0.9 else 304
        size = 0 if status in (304, 503) else rng.randint(200, 9000)
        agent = "curl/8.4" if ip.startswith("203.") else "Mozilla/5.0 (X11; Linux x86_64)"
        lines.append('%s - - [%s] "GET %s HTTP/1.1" %d %d "-" "%s"' % (ip, stamp, path, status, size, agent))
        if second in (50, 51, 300):
            lines.append("garbage line without the expected fields")
    return "\n".join(lines) + "\n"


def log_stats(text):
    parsed = []
    unparsed = 0
    for line in text.splitlines():
        try:
            ip = line.split(" ")[0]
            stamp = line.split("[", 1)[1].split("]", 1)[0]
            request = line.split('"', 2)[1]
            rest = line.split('"', 2)[2].split()
            status, size = int(rest[0]), int(rest[1])
            path = request.split(" ")[1]
        except (IndexError, ValueError):
            unparsed += 1
            continue
        parsed.append((ip, stamp[:17], path, status, size))
    classes = Counter("%dxx" % (s // 100) for _, _, _, s, _ in parsed)
    paths = Counter(p for _, _, p, _, _ in parsed)
    not_found = Counter(ip for ip, _, _, s, _ in parsed if s == 404)
    errors_by_minute = Counter(m for _, m, _, s, _ in parsed if s >= 500)
    busiest = max(sorted(errors_by_minute), key=lambda m: errors_by_minute[m])
    return {
        "lines": len(text.splitlines()),
        "parsed": len(parsed),
        "unparsed": unparsed,
        "2xx": classes["2xx"],
        "3xx": classes["3xx"],
        "4xx": classes["4xx"],
        "5xx": classes["5xx"],
        "bytes": sum(s for *_, s in parsed),
        "top_path": paths.most_common(1)[0][0],
        "top_path_hits": paths.most_common(1)[0][1],
        "scanners": sorted("%s %d" % (ip, n) for ip, n in not_found.items() if n >= 20),
        "worst_minute": busiest,
        "worst_minute_5xx": errors_by_minute[busiest],
    }


# ── CSV and JSON Lines ───────────────────────────────────────────────────────


def build_orders():
    rng = random.Random(11)
    regions = ["EU", "US", "APAC"]
    notes = ["", "Rush, please", 'He said "ship it"', "Leave at door\nside gate", "gift"]
    rows = [["order_id", "customer", "region", "amount", "note"]]
    for n in range(1, 41):
        cents = rng.randint(500, 250000)
        rows.append(["A%03d" % n, "cust-%d" % rng.randint(1, 9), regions[n % 3],
                     "%d.%02d" % divmod(cents, 100), notes[n % len(notes)]])
    buffer = io.StringIO()
    csv.writer(buffer, lineterminator="\r\n").writerows(rows)
    return buffer.getvalue(), rows


def orders_answers(rows):
    header, body = rows[0], rows[1:]
    region = header.index("region")
    amount = header.index("amount")
    groups = defaultdict(lambda: [0, 0])
    for row in body:
        dollars, cents = row[amount].split(".")
        groups[row[region]][0] += 1
        groups[row[region]][1] += int(dollars) * 100 + int(cents)
    grouped = ["%s,%d,%d.%02d" % (r, groups[r][0], *divmod(groups[r][1], 100)) for r in sorted(groups)]
    eu = [row for row in body if row[region] == "EU"]
    return {
        "rows": len(body),
        "group_by_region_sum_amount": ["region,count,sum_amount"] + grouped,
        "eu_rows": len(eu),
        "eu_first": "%s,%s" % (eu[0][0], eu[0][amount]),
        "quoted_note": body[1][4],
    }


def build_events():
    rng = random.Random(3)
    lines = []
    for n in range(30):
        lines.append(json.dumps({"user": "user%d" % (n % 6), "action": rng.choice(["login", "view", "buy"]),
                                 "ms": rng.randint(5, 900), "meta": {"ok": n % 5 != 0}}))
    return "\n".join(lines) + "\n"


def events_answers(text):
    events = [json.loads(line) for line in text.splitlines()]
    slow = [e for e in events if e["ms"] >= 500]
    return {"events": len(events), "slow": len(slow),
            "first_slow": "%s,%s,%d" % (slow[0]["user"], slow[0]["action"], slow[0]["ms"]),
            "failed": sum(1 for e in events if not e["meta"]["ok"])}


def main():
    hellos = [
        ("openssl-default", captured_client_hello("example.com", lambda c: c.set_alpn_protocols(["h2", "http/1.1"]))),
        ("openssl-tls12", captured_client_hello("shop.example", lambda c: setattr(c, "maximum_version", ssl.TLSVersion.TLSv1_2))),
        ("browser-like", browser_like()),
        ("odd-alpn-no-sni", odd_alpn_no_sni()),
        ("legacy-no-extensions", legacy_no_extensions()),
    ]
    capture, frames = build_capture([record for _, record in hellos])
    (HERE / "capture.pcap").write_bytes(capture)
    expected_hellos = []
    for name, record in hellos:
        hello = parse_client_hello(record)
        text, digest = ja3(hello)
        expected_hellos.append({"name": name, "sni": hello["sni"], "ja3": text, "ja3_md5": digest,
                                "ja4": ja4(hello)})

    log = build_log()
    (HERE / "access.log").write_bytes(log.encode())
    orders_text, rows = build_orders()
    (HERE / "orders.csv").write_bytes(orders_text.encode())
    events = build_events()
    (HERE / "events.jsonl").write_bytes(events.encode())

    expected = {
        "python_openssl": ssl.OPENSSL_VERSION,
        "hellos": expected_hellos,
        "capture": summarize(frames),
        "log": log_stats(log),
        "orders": orders_answers(rows),
        "events": events_answers(events),
    }
    (HERE / "expected.json").write_bytes((json.dumps(expected, indent=2) + "\n").encode())
    print(json.dumps(expected, indent=2))


if __name__ == "__main__":
    main()
