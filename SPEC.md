# RushWay v0.0.1 — GoWay v1.8.4 Compatibility Specification

## Purpose

RushWay targets protocol-compatible behavior with GoWay v1.8.4 at commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

**Status rule:** `Implemented` means the current source contains the path. It does not mean current-head executable interoperability has passed.

## Current status — 2026-09-13

| Area | Status |
|---|---|
| Plain WS handshake/auth/XOR | Implemented |
| Plain WS MUX TCP/pooling | Implemented; runtime evidence pending |
| Plain WS non-MUX | Implemented; runtime evidence pending |
| Plain WS UDP | Implemented; runtime evidence pending |
| WSS MUX/non-MUX/UDP client paths | Implemented; runtime evidence pending |
| QUIC / QUIC+TLS TCP/UDP paths | Implemented; runtime evidence pending |
| Full GoWay interoperability | Not validated |
| Current release artifacts | Not validated |

## Protocol constants

- GoWay version baseline: `1.8.4`
- WebSocket maximum frame: 64 MiB
- HTTP header hard limit: 8192 bytes
- MUX header: 7 bytes: `uint32 StreamID` + `byte Command` + `uint16 PayloadLen`, big-endian
- Commands: SYN `0x01`, DATA `0x02`, FIN `0x03`, RST `0x04`
- DATA is chunked to `uint16::MAX`

## CLI compatibility

GoWay uses single-hyphen multi-character options such as `-up`, `-fakehost`, `-mux`, `-no-mux`, `-mux-sessions`, `-W`, `-socket-buffer`, `-no-tcp-nodelay`, `-no-tcp-keepalive`, `-dns`, `-block-local`, `-no-block-local`, `-verify-ssl`, `-allow-open`, `-max-conn`, `-connection-timeout`, `-log`, `-log-file`, `-tui`, `-version`, `-cpuprofile`, and `-cpuprofile-duration`.

RushWay now normalizes these legacy single-hyphen forms before Clap parsing, preserves `-p`, `-k`, `-u`, `-W`, and handles GoWay boolean assignments such as `-mux=true` and `-block-local=false`. Parser tests cover these cases.

`-log` changes the RushWay tracing level. `-log-file`, `-tui`, and CPU profiling options are accepted for CLI compatibility but are explicitly still stubs; they are not claimed as feature-complete parity.

## Authentication / XOR

GoWay derives SHA-256 from the configured key, expands it to a 256 KiB repeating XOR buffer, and resets the transform offset for each operation. Empty key means no transform. MUX framing requires XOR over the complete encoded frame including the 7-byte header.

## Proxy front-end

Supported code paths:

- SOCKS5 no-auth CONNECT
- HTTP CONNECT
- SOCKS5 UDP ASSOCIATE

UDP uses a dedicated `UDP\n` transport and is not routed through MUX DATA frames.

## Non-MUX

The GoWay wire shape is implemented:

1. WebSocket handshake.
2. First binary payload is `host:port\n`, XOR transformed when keyed.
3. Server returns `OK\n` after successful target dialing.
4. Later binary frames carry raw TCP bytes.

## WSS

Client-side WSS TCP MUX, pooled MUX, non-MUX and UDP code paths exist. HTTP/1.1 ALPN and certificate verification/insecure modes are represented. Browser-profile fingerprint parity and runtime interop remain unverified.

## QUIC

GoWay-derived behavior represented in source:

- ALPN `goway-quic` and `h3`
- 60-second max idle timeout
- 15-second keepalive
- stream/connection receive windows matching the current source-derived configuration
- TCP bootstrap `<key> <target>\n` or `<target>\n`
- `OK\n` / `ERR: DIAL_FAILED\n`
- pooled physical connections with reconnect handling
- UDP control `UDP\n` or `<key> UDP\n`
- 2-byte big-endian UDP length framing

Exact dead-IP/retry semantics, TLS/SNI edge cases and executable interoperability remain pending.

## DNS

The current RushWay resolver implements the source-derived GoWay strategy:

- configured remote DNS server via `-dns` / JSON `dns`;
- 5-second resolve timeout;
- remote DNS first;
- UDP query with TCP retry when truncated;
- system DNS fallback after remote failure;
- 5-minute positive cache;
- IP literals bypass DNS.

Currently wired into server target resolution and plain non-MUX upstream/target resolution. Plain MUX upstream, WSS upstream and QUIC upstream hostname dialing still need integration.

## Socket policy

Runtime and non-MUX currently apply TCP_NODELAY, keepalive and optional send/receive buffer sizing. MUX/WSS upstream paths still need final propagation audit.

## Release gate

100% completion requires executable evidence for:

1. `cargo fmt -- --check`
2. `cargo check --all-targets`
3. `cargo test --all-targets --all-features`
4. release build
5. GoWay client -> RushWay server
6. RushWay client -> GoWay server
7. WS MUX/non-MUX TCP + UDP
8. WSS TCP + UDP
9. QUIC TCP + UDP
10. SOCKS5 TCP/UDP and HTTP CONNECT
11. EOF/FIN/RST/disconnect/reconnect
12. 1/100/500/1000 streams and sustained/slow-fast workloads
13. current-head benchmark evidence
14. Windows x64, Debian 12 x64 and ARMv7/OpenWrt artifacts
15. v0.0.1 smoke test/tag

Never convert source inspection into a runtime-pass claim.

## Three-file relay contract

Only these files are canonical handoff state:

- `AI_HANDOFF.md`
- `PROGRESS.md`
- `SPEC.md`
