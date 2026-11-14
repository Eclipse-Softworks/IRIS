# Aegis-IRIS Real-Time Cyber Operations Center (SOC)

A high-performance security operations gateway and live web visualization center written in native IRIS. 

The SOC serves an embedded HTML5/CSS3 glassmorphic operational command dashboard over HTTP and streams real-time deep packet inspection telemetry, Shannon entropy distributions, and dynamic zero-trust firewall mitigations over full-duplex RFC 6455 WebSockets.

---

## Features

- **Dual-Protocol Serving**: Standard HTTP routes (`/`, `/health`) alongside full RFC 6455 WebSocket Upgrade negotiation (`/ws`).
- **Real-Time Telemetry Streaming**: Push structured JSON events including packet flows, JA4 transport fingerprints, Shannon entropy scores, and threat indicators directly to connected analysts.
- **Glassmorphic Web Interface**: Responsive dark-mode dashboard with real-time incident feeds, mission KPI counters, and threat matrix badges.
- **Bi-Directional Command & Control**: Allows authorized security analysts to inject synthetic packets, flush quarantine tables, and modify firewall policies over WebSocket channels.

---

## Running the SOC

Execute the standalone self-testing loopback testbench:

```powershell
.\target\release\iris.exe run projects/cyber_operations_center/main.iris
```
