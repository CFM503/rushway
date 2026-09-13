# RushWay v0.0.1 — GoWay v1.8.4 Compatibility Specification

## Purpose

RushWay targets protocol-compatible behavior with GoWay v1.8.4 at commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

**Status rule:** `Implemented` means the current code contains the path; it does not mean current-head executable interop has passed.

## High-level implementation status — 2026-09-13

| Area | Current status |
|---|---|
| Plain WS handshake | Implemented |
| Plain WS MUX TCP | Implemented; execution evidence pending |
| Plain WS physical MUX pooling | Implemented; stress evidence pending |
| Plain WS SOCKS5 UDP | Implemented; execution evidence pending |
| WSS TCP client | Implemented |
| WSS physical MUX pooling | Implemented; execution evidence pending |
| WSS non-MUX client | Implemented |
| WSS UDP client | Implemented in code; execution evidence pending |
| Non-MUX 1:1 TCP | Implemented in code; exact lifecycle/pool parity pending |
| QUIC / QUIC+TLS TCP | Implemented in code; execution evidence pending |
| QUIC UDP | Implemented in code; execution evidence pending |
| Full GoWay interoperability | Not validated |
| Current-head release artifacts | Not validated |

## Confirmed protocol constants

- Version: `1.8.4`
- WebSocket maximum frame: 64 MiB
- HTTP header hard limit: 8192 bytes
- MUX header: 7 bytes: `uint32 StreamID` + `byte Command` + `uint16 PayloadLen`, big-endian
- SYN `0x01`, DATA `0x02`, FIN `0x03`, RST `0x04`
- DATA payload is chunked to `uint16::MAX`

## Authentication / XOR

GoWay derives SHA-256 from the configured key, expands the digest to a 256 KiB repeating XOR buffer and resets the transform offset for each operation. Empty key means no transform.

A compatibility audit established that MUX transforms apply to the **complete encoded MUX frame**, not only the payload. RushWay's plain and WSS MUX paths were updated toward that framing rule.

## WebSocket

RushWay supports functional RFC6455 non-fragmented data/control frames, client masking, server unmasked frames, Ping->Pong, Pong discard, Close->EOF, 64 MiB frame limit and 8192-byte HTTP header limit.

Browser-profile fingerprint parity remains incomplete.

## Proxy front-end

Supported control paths in code:

- SOCKS5 no-auth CONNECT
- HTTP CONNECT
- SOCKS5 UDP ASSOCIATE

UDP uses a dedicated transport (`UDP\n`) and must never be routed through TCP MUX DATA frames.

## Non-MUX

The GoWay v1.8.4 non-MUX shape is implemented:

1. WebSocket handshake.
2. First binary payload is `host:port\n`, XOR transformed when a key exists.
3. Server replies `OK\n` after successful target dialing.
4. Later binary frames carry raw TCP data in both directions.

Non-MUX physical connection pool/lifecycle parity and executable interop remain to be proven.

## WSS / TLS

RushWay caches verified and insecure rustls client configurations separately with `OnceLock`, preserves HTTP/1.1 ALPN and supports `wss://` TCP MUX, pooled MUX and non-MUX client paths. WSS UDP has a dedicated raw UDP WebSocket path.

Server-side WSS listener behavior is not currently claimed as part of the GoWay origin compatibility contract until the source confirms that listener role is required; CDN/TLS-termination deployments may keep the origin at plain WS.

## QUIC

GoWay v1.8.4 uses `quic-go` with:

- ALPN `goway-quic` and `h3`
- 60-second max idle timeout
- 15-second keepalive
- client physical connection pooling with a single-flight dialing barrier
- TCP stream bootstrap `<key> <target>\n` or `<target>\n`
- target failure response `ERR: DIAL_FAILED\n`

RushWay now contains:

- QUIC / QUIC+TLS client and server runtime
- GoWay-derived TLS/ALPN/transport configuration
- single-flight QUIC connection reuse
- per-local-TCP bidirectional QUIC streams
- QUIC UDP control line `UDP\n` or `<key> UDP\n`
- 2-byte big-endian UDP frame length prefix
- XOR protection of QUIC UDP packet payloads
- SOCKS5 UDP envelope on return packets

Exact retry/dead-IP behavior, full close/reset semantics and executable GoWay interop remain pending.

## Low-level TCP options

Current runtime configuration includes code paths for:

- max concurrent connections
- block-local target policy
- TCP_NODELAY
- TCP keepalive
- socket send/receive buffer sizing

Exact GoWay flag semantics and all CLI/config wiring still require final executable verification.

## DNS

RushWay currently uses Tokio/system DNS resolution in the active runtime. Exact GoWay remote-DNS server selection, cache, timeout and fallback behavior is still a compatibility gap.

## Final compatibility matrix

Must execute before release:

1. GoWay client -> RushWay server
2. RushWay client -> GoWay server
3. Authenticated WS
4. Open WS where supported
5. MUX enabled
6. MUX disabled / 1:1
7. WSS
8. QUIC / QUIC+TLS
9. SOCKS5 TCP
10. HTTP CONNECT
11. SOCKS5 UDP
12. EOF / FIN
13. RST
14. disconnect/reconnect
15. 1/100/500/1000 streams
16. sustained large payloads
17. mixed slow/fast streams
18. invalid configuration/arguments

## Release gate

The implementation is not considered 100% complete until:

- current-head `cargo fmt -- --check` passes,
- `cargo check --all-targets` passes,
- `cargo test --all-targets --all-features` passes,
- release build passes,
- the full GoWay interoperability matrix passes,
- stream stress and performance evidence is recorded,
- Windows x64, Debian 12 x64 and ARMv7/OpenWrt artifacts are built and smoke-tested,
- v0.0.1 is tagged and smoke-tested.
