# RushWay v0.0.2 — GoWay v1.8.4 Compatibility Specification

## Purpose

RushWay targets protocol-compatible behavior with GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

**Status rule:** `Implemented` means the current source contains the path. It does not mean current-head executable interoperability has passed.

## Current implementation

- Plain WebSocket handshake, RFC6455 framing, XOR and MUX are implemented.
- Plain WS MUX physical pooling and non-MUX 1:1 forwarding are implemented.
- Plain WS UDP uses a dedicated `UDP\n` transport and is not carried through TCP MUX DATA frames.
- WSS TCP MUX/non-MUX and UDP client paths are implemented.
- QUIC/QUIC+TLS TCP and UDP client/server paths are implemented with GoWay-derived ALPN and transport values.
- SOCKS5 CONNECT, UDP ASSOCIATE and HTTP CONNECT front-ends are implemented.
- Plain-WS MUX, WSS MUX and QUIC target-failure paths now have explicit local failure responses instead of premature success/silent close.

## CLI compatibility

GoWay v1.8.4 uses single-hyphen names such as `-up`, `-fakehost`, `-mux`, `-no-mux`, `-mux-sessions`, `-W`, `-socket-buffer`, `-no-tcp-nodelay`, `-no-tcp-keepalive`, `-dns`, `-block-local`, `-no-block-local`, `-max-conn`, `-connection-timeout`, `-verify-ssl`, `-allow-open`, `-log`, `-log-file`, `-tui`, `-version`, `-cpuprofile` and `-cpuprofile-duration`.

RushWay normalizes these forms before Clap parsing, including GoWay boolean syntax such as `-mux=true` and `-block-local=false`. Ordinary `-p`, `-k`, `-W` and `-u` short forms remain valid.

`-log-file`, `-tui`, and CPU profiling arguments are implemented and covered by unit tests for their bookkeeping paths; executable TUI/dashboard and pprof sampling verification on target hardware still counts toward the release gate.

## Authentication / XOR

GoWay derives SHA-256 from the key, expands it into a repeating 256 KiB XOR key stream and resets the XOR transform offset for each operation. Empty key means no transform.

MUX compatibility requires XOR over the complete encoded 7-byte MUX frame plus payload, not payload-only.

## WebSocket

Functional support includes the HTTP upgrade, RFC6455 masking/unmasking, non-fragmented data/control frames, 64 MiB maximum frame size, 8192-byte HTTP header limit, Ping->Pong, Pong discard and Close->EOF.

Browser-profile fingerprint parity is not claimed complete.

## DNS

The shared resolver now follows the GoWay implementation shape:

- configured DNS server via `-dns` or JSON `dns` / `dnsServer`;
- 5-second timeout;
- remote UDP query;
- transaction-ID validation;
- TCP retry for truncated UDP responses;
- system DNS fallback after remote failure;
- 5-minute positive cache;
- IP literals bypass DNS.

It is integrated into server target dialing and plain WS MUX/non-MUX, WSS and QUIC hostname dialing. Source-side transaction-ID coverage exists; executable cache/fallback coverage still requires a usable Rust test environment.

## Low-level TCP policy

Runtime configuration includes max connections, local-target blocking, TCP_NODELAY, TCP keepalive and socket buffer sizing. The shared policy is now reusable by the server, pooled plain-WS MUX, WSS upstream sockets and QUIC server target sockets. Final executable socket-option smoke coverage remains required.

## SOCKS5 / HTTP

Supported:

- SOCKS5 no-auth CONNECT;
- SOCKS5 UDP ASSOCIATE;
- HTTP CONNECT;
- IPv4, domain and IPv6 target forms;
- 8192-byte HTTP header ceiling;
- SOCKS5 UDP `FRAG=0` only;
- explicit SOCKS5 General Failure response primitive (`0x05`).

Remaining work is exact malformed-request/close behavior across every transport and executable parity against GoWay.

## QUIC

GoWay-derived settings:

- ALPN `goway-quic` / `h3`;
- 60-second max idle timeout;
- 15-second keepalive;
- receive windows matching the compatibility baseline;
- TCP stream bootstrap `<key> <target>\n` or `<target>\n`;
- failure response `ERR: DIAL_FAILED\n`;
- client physical connection reuse with one retry after `open_bi` failure;
- UDP length-prefix framing with 2-byte big-endian lengths;
- keyed XOR for UDP payloads;
- upstream DNS resolution preserves the logical hostname used as QUIC TLS server name;
- server target TCP sockets receive the common TCP policy after connect.

Exact dead-IP/connection-state/TLS-SNI interoperability still requires executable evidence.

## 100% release gate

The project is not 100% complete until all of the following are true:

1. `cargo fmt -- --check` passes.
2. `cargo check --all-targets` passes.
3. `cargo test --all-targets --all-features` passes.
4. `cargo build --release` passes.
5. GoWay -> RushWay and RushWay -> GoWay WS/WSS/QUIC TCP+UDP interoperability passes.
6. SOCKS5/HTTP lifecycle/error matrix passes.
7. 1/100/500/1000 stream stress plus large and mixed slow/fast workloads passes.
8. c1/c8/c32 benchmarks are recorded.
9. Windows x64, Debian 12 x64 and ARMv7/OpenWrt artifacts build and smoke-test.
10. `v0.0.2` is tagged and smoke-tested.

Never label the project 100% complete solely from source inspection.