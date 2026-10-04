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
- `-verify-ssl` / `--no-verify-ssl`: Upstream TLS certificate verification is **on by default**. Verification uses the bundled webpki (Mozilla) root set — not the OS trust store — so a private- or self-signed upstream requires `--no-verify-ssl`. With verification off, anyone on path can impersonate the upstream and read the `-k` key.
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

#### Serving a certificate clients can verify

Without extra flags the server generates a self-signed certificate at startup,
which verifying clients reject. Supply a real certificate and key to make
server mode usable with verification left on:

```bash
rushway -p :443 -k mypassword --cert fullchain.pem --key privkey.pem
```

Both flags must be given together; a missing partner is a startup error rather
than a silent fall back to self-signed. Accepted key formats: `PRIVATE KEY`
(PKCS#8), `EC PRIVATE KEY` (SEC1) and `RSA PRIVATE KEY` (PKCS#1).

---

## Compatibility Notes

- **Separation of Destination and Identity**: Using `-up wss://<ip>:443/<path>` with `-fakehost <domain>` guarantees that TCP packets travel to `<ip>`, while TLS certificates and HTTP Host headers match `<domain>`.
- **TLS verification default**: upstream certificate verification is on by default (it was opt-in before). Documented setups — `-up wss://<ip>:443/...` plus `-fakehost <domain>` — keep working, because the edge presents a valid certificate for `<domain>`. Setups that previously relied on the implicit opt-out must now pass `--no-verify-ssl` explicitly.
- **Default Max Connections**: Standardized to `1500`. Custom limits can be specified via `-max-conn <N>`.
- **Operating Systems**: Official automated releases are built for Windows x64, Linux x64 (static `musl`, no glibc dependency — runs on Debian 10+, Ubuntu 18.04+, Alpine and other musl/glibc distributions alike), and OpenWrt / KWRT ARMv7 (musl).

---

## Security Notes

- **XorCipher (`-k`) is obfuscation, not encryption**: the `-k` XOR cipher exists for GoWay v1.8.4 wire compatibility. It applies a fixed keystream derived from the key, with no nonce and no message authentication, so authenticated frames can be replayed. Do not expect it to protect against an active attacker.
- **Confidentiality depends on the transport, not the proxy protocol**: real confidentiality comes from the `wss://` / `quic://` transport-layer TLS with certificate verification left on (the default). With `--no-verify-ssl`, anyone on the path can impersonate the upstream and steal the `-k` key.
- **QUIC authentication line sends a key digest (behavior change)**: in QUIC mode the authentication line now carries the SHA-256 digest of the key instead of the key itself. The server still accepts the legacy plaintext format and logs a WARN when it receives one.
- **Server UDP relay locks onto one peer (behavior change)**: the server-mode UDP relay now pins the source peer of the first legitimate datagram it accepts; later datagrams from other sources are dropped.