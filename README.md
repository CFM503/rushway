# RushWay

RushWay is a high-performance forwarding proxy written in Rust, engineered for protocol-compatible connectivity with [GoWay](https://github.com/CFM503/way).

## Features

- **Multi-Transport Support**: WebSocket (`ws://`), Secure WebSocket (`wss://`), and QUIC (`quic://`).
- **Cloudflare CDN / FakeHost Alignment**:
  - Independent TCP destination IP and TLS / HTTP identity (`-fakehost`).
  - Strict GoWay-aligned TLS SNI, HTTP `Host`, and `Origin` (`https://<fakehost>`) generation.
  - Path retention (`/path`) across custom CDN WebSocket routing endpoints.
  - Automatic Cloudflare Edge fallback via DNS resolution of `fakehost` if the primary Edge IP becomes unreachable.
- **Connection Multiplexing (MUX)**:
  - 0-RTT stream multiplexing across configurable physical sessions (`-mux-sessions`).
  - High-concurrency asynchronous SYN dispatch and robust bidirectional stream lifecycle management.
- **Standard Proxying**: Supports SOCKS5 and HTTP `CONNECT` tunneling.
- **Custom DNS Resolution**: Configurable remote DNS (`-dns`) with system resolver fallback and negative/positive caching.
- **High Concurrency Baseline**: Default connection capacity of 1,500 concurrent connections.
- **CLI Compatibility**: Fully compatible with both single-dash legacy flags (`-up`, `-fakehost`, `-mux`, `-block-local`, `-max-conn`, etc.) and standard GNU/POSIX flags (`--up`, `--fakehost`, etc.).

---

## Quick Start & Usage

### 1. Cloudflare WSS Client Mode (Recommended Production Command)

In this scenario, RushWay connects to an upstream Cloudflare Edge IP over TLS/WSS, spoofing the hostname for SNI and HTTP routing through `-fakehost`, and establishes 8 parallel multiplexed sessions:

```bash
rushway.exe -k a6835181 -up wss://172.64.229.105:443/pyway -fakehost colo.4467107.xyz -p :9195 -log INFO -W 1024 --socket-buffer 4096 -block-local -tui -dns 8.8.8.8 -mux-sessions 8
```

#### Parameter Breakdown

- `-k <KEY>`: Authentication and cipher key.
- `-up <URL>`: Actual upstream WebSocket / WSS URL (e.g., `wss://172.64.229.105:443/pyway`).
- `-fakehost <HOST>`: Cloudflare / CDN domain used for:
  - TLS SNI (Server Name Indication)
  - HTTP `Host` header
  - `Origin` header (`https://colo.4467107.xyz`)
  - Automatic Cloudflare Edge fallback when the primary IP fails to connect.
- `-p <PORT>`: Local listening address for SOCKS5/HTTP clients (e.g., `:9195` or `127.0.0.1:9195`).
- `-log <LEVEL>`: Logging level (`DEBUG`, `INFO`, `WARN`, `ERROR`, `OFF`).
- `-W <KB>`: Application buffer size in KiB (e.g., `1024` for high-throughput streaming).
- `--socket-buffer <KB>`: Kernel socket buffer size in KiB (`4096` = 4 MiB).
- `-block-local`: Block LAN/loopback target addresses on the client policy boundary.
- `-dns <IP>`: Remote DNS server IP used for target hostname resolution (e.g. `8.8.8.8`).
- `-mux-sessions <N>`: Number of parallel physical MUX sessions to open to the upstream (default `4`, recommended `8` to `64` for heavy loads).
- `-tui`: Flag accepted for CLI compatibility.

---

### 2. Standard Client Modes

#### Standard WS Client (MUX Enabled)

```bash
rushway -p :1080 -up ws://gateway.example.com:8080/ws -k mypassword
```

#### Standalone Non-MUX Client

```bash
rushway -p :1080 -up wss://gateway.example.com:443/ws -k mypassword -no-mux
```

---

### 3. Server Mode

```bash
rushway -p :8080 -k mypassword
```

Or for open testing server without auth:

```bash
rushway -p :8080 --allow-open
```

---

## Compatibility Notes

- **Separation of Destination and Identity**: Using `-up wss://<ip>:443/<path>` with `-fakehost <domain>` guarantees that TCP packets travel to `<ip>`, while TLS certificates and HTTP Host headers match `<domain>`.
- **Default Max Connections**: Standardized to `1500`. Custom limits can be specified via `-max-conn <N>`.
- **Operating Systems**: Official automated releases are built for Windows x64, Debian 12 (Bookworm) x64, and OpenWrt / KWRT ARMv7 (musl).