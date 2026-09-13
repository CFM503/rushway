# RushWay v0.0.1 — GoWay v1.8.4 Compatibility Specification

## Purpose

This document is the migration contract for RushWay. The compatibility baseline is the GoWay v1.8.4 source at commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` in `CFM503/way/goway`.

**Rule:** implementation must follow observed v1.8.4 behavior, not assumptions about how a proxy of this type normally works.

## Compatibility status at 2026-09-13

This is a living specification, not a statement that all listed features are implemented. Use the following status vocabulary:

- **Implemented** — current RushWay code contains the path, but it still needs current-head executable evidence before being called production-ready or interop-complete.
- **Partially implemented** — an important slice exists, but parity or lifecycle behavior is incomplete.
- **Not implemented** — no validated runtime implementation exists yet.

Current high-level status:

| Area | Status |
|---|---|
| Plain WS handshake | Implemented |
| Plain WS MUX TCP | Implemented |
| Plain WS physical MUX pooling | Implemented |
| Plain WS SOCKS5 UDP slice | Implemented, interop evidence pending |
| WSS TCP client path | Implemented, physical pooling now present; execution evidence pending |
| WSS physical pooling | Implemented in current head; execution evidence pending |
| WSS UDP | Not implemented/validated |
| Non-MUX parity | Not implemented |
| QUIC / QUIC+TLS runtime | Not implemented |
| Full GoWay interoperability | Not validated |
| Release artifacts on current head | Not validated |

## Confirmed protocol constants

- Version: `1.8.4`
- WebSocket maximum frame size: 64 MiB
- HTTP header hard limit: 8192 bytes
- CRLF: `\r\n`
- HTTP header terminator: `\r\n\r\n`
- MUX header length: 7 bytes
- MUX header layout: `uint32 StreamID` + `byte Command` + `uint16 PayloadLen`, big-endian integer encoding

### MUX commands

| Command | Value | Meaning | Payload |
|---|---:|---|---|
| SYN | `0x01` | Open stream | `uint16 targetLen` + target address + optional initial data |
| DATA | `0x02` | Stream data | raw data |
| FIN | `0x03` | Half-close / EOF | empty |
| RST | `0x04` | Abrupt reset / error | empty |

Confirmed source behavior: MUX DATA is limited by the 16-bit payload length and larger writes are chunked into multiple DATA frames. Client and server stream queues implement bounded buffering/backpressure. RST/close must unblock pending stream operations.

## CLI compatibility

Required flags observed in v1.8.4:

- `-p` listen address; required
- `-up` upstream URL; omitted = Server mode, present = Client mode
- `-k` authentication/XOR key
- `-fakehost` spoof Hostname / SNI
- `-mux` enable MUX; default true
- `-no-mux` disable MUX and use 1:1 pool behavior
- `-mux-sessions` physical MUX sessions; default 4; maximum 64
- `-W` application buffer size in KiB; default 128 KiB
- `-socket-buffer` kernel socket buffer in KiB; default 0 / OS autotuning
- `-no-tcp-nodelay` disable TCP_NODELAY
- `-no-tcp-keepalive` disable TCP KeepAlive probes
- `-dns` remote DNS server IP
- `-block-local` block local/LAN targets in Client mode; default true
- `-no-block-local` disable that protection
- `-verify-ssl` strict certificate verification for WSS upstream; default false
- `-allow-open` allow Server mode without key
- `-max-conn` concurrent connection limit; default 1000; accepted range 1..1,000,000
- `-connection-timeout` connection/idle timeout in seconds; default 60
- `-log` DEBUG/INFO/WARN/ERROR; default INFO
- `-log-file` save the last 10 log entries
- `-tui` terminal dashboard
- `-version` print version and exit
- `-cpuprofile` CPU profile path
- `-cpuprofile-duration` stop CPU profiling after N seconds

Invalid or missing required arguments must fail rather than silently selecting a different mode. Server mode without a key requires `-allow-open`.

## Upstream schemes

The v1.8.4 client accepts:

- `ws://`
- `wss://`
- `quic://`
- `quic+tls://`

The upstream URL is parsed once during startup. Host and scheme are validated before serving traffic.

## Configuration model

Observed core fields include:

- ProxyHost / ProxyPort
- Upstream
- FakeHost
- Key
- BufferSize
- NoTcpNoDelay
- NoTcpKeepAlive
- SocketBuffer
- ConnTimeout
- VerifySSL
- MaxConns
- BlockLocal
- AllowOpen
- TUI
- Mux
- MuxSessions

Derived runtime state includes Crypto, remote Resolver, buffer pools, header buffer pool, base TLS configuration, MUX client pool, QUIC client pool, and an upstream-QUIC indicator.

Buffer initialization is derived from `-W`: the runtime buffer is at least 64 KiB plus WebSocket/MUX framing overhead and capped at approximately 12 MiB plus overhead. Header buffers are pooled at MaxHeaderSize.

## Authentication / crypto

The v1.8.4 implementation derives a SHA-256 hash from the configured key and expands it into a 256 KiB repeating XOR key buffer. Payload transformation is XOR and symmetric. Empty key means no Crypto object.

**Compatibility warning:** this is protocol compatibility behavior, not a claim of modern cryptographic security. Do not silently replace it with a different cipher in the compatibility path.

## WebSocket behavior

### Client request

The observed GoWay v1.8.4 client sends an HTTP/1.1 GET upgrade request with these functional fields:

- `Host`
- `Connection: Upgrade`
- `Upgrade: websocket`
- `Sec-WebSocket-Version: 13`
- `Sec-WebSocket-Key: <16 random bytes, Base64>`
- `Sec-Fetch-Dest: websocket`
- `Sec-Fetch-Mode: websocket`
- `Sec-Fetch-Site: cross-site` or `same-origin`
- `Origin` based on the selected HTTP/HTTPS scheme and SNI hostname

It also sends browser-profile-dependent User-Agent, Accept-Language and Chromium client-hint headers; the order of the non-fixed header group is randomized. RushWay currently implements the functional handshake fields, but browser-profile fingerprint parity is still pending.

### Server response

GoWay server requires an `Upgrade: websocket` header and extracts `Sec-WebSocket-Key`; the successful response is:

`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: <computed>\r\n\r\n`

The accept value is SHA-1(client key + RFC6455 GUID), Base64 encoded. GoWay emits HTTP 400 when the required upgrade is missing.

### Handshake response validation

The observed GoWay client strictly expects status `101`. It distinguishes common failures including `200`, `301/302/307/308`, `400`, `403`, `404`, `502`, `503`, and `504`.

### Framing

Client-to-server WebSocket frames are masked. v1.8.4 uses a per-session/per-goroutine pooled xorshift-based mask PRNG seeded once with cryptographic randomness. Masking is an RFC 6455 framing requirement and is not the authentication primitive.

Header parsing must enforce the 8192-byte hard limit. Oversized headers must return `header too large` behavior rather than being accepted indefinitely.

RushWay's frame codec currently supports complete non-fragmented data frames, 64 MiB maximum frame size, RFC6455 control-frame size/fragmentation checks, masking/unmasking, Ping→Pong, Pong discard, and Close→EOF.

## Proxy front-end

The v1.8.4 integration tests exercise SOCKS5 TCP CONNECT, HTTP CONNECT, SOCKS5 UDP ASSOCIATE and TCP echo forwarding. The GoWay source contains preallocated responses:

- SOCKS5 success: `05 00 00 01 00 00 00 00 00 00`
- SOCKS5 general failure used by the UDP-associate local bind failure path: `05 01 00 01 00 00 00 00 00 00`
- HTTP CONNECT success: `HTTP/1.1 200 Connection Established\r\n\r\n`
- HTTP malformed/header-read failure: `HTTP/1.1 400 Bad Request\r\n\r\n`

The v1.8.4 test suite explicitly starts SOCKS5 with `[VER=5, NMETHODS=1, METHOD=0=no-auth]` and then tests CONNECT. UDP ASSOCIATE is separately exercised end-to-end against a UDP echo server. Truncated UDP-associate reads were hardened with strict `io.ReadFull` error/EOF checks in the v1.8.4 source.

### Exact control-flow findings extracted so far

- SOCKS5 UDP ASSOCIATE creates a local UDP listener with `net.ListenUDP("udp", &net.UDPAddr{IP: localIP, Port: 0})`, i.e. an ephemeral local UDP port. If this bind fails, GoWay sends the SOCKS5 general-failure reply (`REP=0x01`) and terminates the local connection.
- The UDP-associate response therefore advertises the dynamically bound relay endpoint rather than a fixed port.
- The source explicitly checks errors/EOF for the fixed-size `io.ReadFull` reads used while parsing UDP ASSOCIATE address fields; truncated IPv4/domain/IPv6 inputs must not proceed with partially initialized addresses.
- SOCKS5 UDP relay frames are parsed as `[RSV(2), FRAG(1), ATYP(1), ADDR..., PORT(2), PAYLOAD...]`. The source path inspects `ATYP` and computes the payload offset before dialing/relaying the destination.
- The static SOCKS5/HTTP responses are preallocated in v1.8.4 to avoid per-connection allocations.
- HTTP CONNECT header acquisition is a looped read until `\r\n\r\n` or `\n\n`, with the 8192-byte hard limit; malformed/incomplete header reads map to HTTP 400 rather than being silently accepted.

### RushWay implementation status for the proxy front-end

- Plain WS SOCKS5 TCP and HTTP CONNECT forwarding are implemented.
- Plain WS SOCKS5 UDP forwarding has an implementation slice: the control path uses a dedicated raw `UDP\n` WebSocket session and UDP payloads are carried as raw WebSocket binary frames.
- That UDP implementation must not be described as full GoWay parity until FRAG/error/reply/close behavior is exercised in an executable compatibility test matrix.
- WSS UDP is not implemented/validated yet.
- Exact SOCKS5/HTTP REP/error lifecycle parity is still a Stage 2/4 compatibility item.

## DNS

Remote DNS uses a configured server and caches resolved addresses. Failed remote resolution falls back to the system resolver. The resolver has a timeout and caches successful results. Upstream dialing can use resolved IPs while preserving the configured hostname for TLS/SNI where required.

RushWay still needs to reconcile this behavior against the exact GoWay v1.8.4 source and wire it into the complete runtime before marking DNS compatibility complete.

## Connection pool / MUX pool

Client mode has pooling/reuse infrastructure. MUX mode maintains multiple physical MUX sessions and assigns logical streams over those sessions. Plain `ws://` physical MUX pooling and WSS physical TLS+WebSocket+MUX pooling are implemented in the current RushWay head.

The current pools expose configurable physical session count, least-active selection, 256 logical streams per session and session retirement when the physical reader terminates. Heavy concurrent acquire/stream reservation still needs stress validation.

Non-MUX mode is intended to fall back to a 1:1 pooled connection model, but RushWay's runtime implementation is not complete. Do not mark non-MUX parity complete until executable tests prove it.

Do not change pool/session selection semantics merely for performance until interoperability tests prove equivalence.

## Buffering and lifecycle

The v1.8.4 code explicitly optimizes allocations with sync.Pool-style buffer reuse. Logical stream queues have bounded memory and must provide backpressure without allowing one slow stream to stall unrelated streams. Close/RST must release pooled buffers and target connections.

Observed v1.8.4 regression tests specifically cover:

- MUX client PRNG lifecycle and concurrent SendFrame/Close
- MUX server target-connection lifecycle under concurrency
- slow stream backpressure while a fast stream continues
- RST unblocking a blocked PushDataFrame
- 1000 stream creation/close
- concurrent log-ring save
- hard HTTP header limit

RushWay has implemented important pieces of this behavior, but current-head execution evidence is still required before declaring lifecycle compatibility complete.

## TLS / browser profile behavior

The v1.8.4 source contains multiple browser profiles bundling User-Agent, Accept-Language, Chromium Client Hints where applicable, TLS cipher preferences, and curve preferences. WSS initialization pre-builds TLS configurations for profiles. `-fakehost` is used for Host/SNI/CDN/reverse-proxy scenarios.

RushWay currently preserves functional TLS verification/ALPN behavior and caches verified/insecure rustls client configurations with `OnceLock`. WSS TCP forwarding and WSS physical-session pooling are implemented in code. Browser fingerprint parity remains a separate compatibility item, and WSS pooled execution/interop evidence is still required.

## QUIC

GoWay v1.8.4 uses `quic-go`. Exact source extraction has established these concrete wire/runtime facts:

- Server listener is created with `quic.ListenAddr(listenAddr, tlsConf, defaultQUICConfig())`.
- Server TLS configuration advertises ALPN protocols `goway-quic` and `h3`.
- `defaultQUICConfig()` sets `MaxIdleTimeout` to 60 seconds and `KeepAlivePeriod` to 15 seconds. Remaining QUIC config fields still need exact extraction.
- Server accepts streams in a loop with `AcceptStream(context.Background())`. If stream acceptance fails, the connection handler returns.
- The connection handler defers `CloseWithError(0, "connection closed")` for normal handler teardown.
- Client dialing uses `quic.DialAddr(ctx, actualAddr, tlsConf, defaultQUICConfig())`.
- The QUIC client pool deliberately performs DNS resolution, `quic.DialAddr`, and `OpenStreamSync` outside the pool mutex. A single-flight `dialing` barrier prevents a burst of concurrent stream requests from creating a connection storm.
- If `OpenStreamSync` fails on an existing pooled connection, v1.8.4 closes that QUIC connection with application error `0x01` (`"stream open failed"`) and removes the failed connection from the pool.

This constrains the RushWay QUIC architecture, but **does not constitute a complete QUIC implementation**. Remaining extraction items are: full `defaultQUICConfig`, TLS certificate/client verification behavior, exact `quic+tls` vs `quic` handling, server stream target/bootstrap framing, connection close/reset mapping, pool capacity/selection, retry/dead-IP behavior, and how QUIC streams attach to the MUX/non-MUX forwarding paths.

## Required interoperability matrix

1. GoWay v1.8.4 Client -> RushWay Server
2. RushWay Client -> GoWay v1.8.4 Server
3. WebSocket authenticated
4. WebSocket open mode where supported
5. MUX enabled
6. MUX disabled / 1:1 mode
7. TLS/WSS
8. QUIC
9. SOCKS5 TCP
10. HTTP CONNECT
11. SOCKS5 UDP
12. EOF / FIN
13. RST
14. disconnect/reconnect
15. 1 / 100 / 500 / 1000 logical streams
16. large payloads and sustained transfer
17. slow and fast streams concurrently
18. invalid configuration and argument handling

## Specification status

This file remains an **intermediate, source-derived compatibility contract**. It intentionally distinguishes extracted GoWay behavior from RushWay implementation status. A constructor compiling, a unit test passing or a parser existing is not sufficient to mark a transport or protocol feature complete.

## Next action

Use the following order unless newer execution evidence identifies a stronger blocker:

1. Finish executable CI/current-head build evidence for the WSS pooling head.
2. Run current-head WSS pooled TCP and plain MUX benchmarks.
3. Finish exact SOCKS5 TCP/UDP control flow, including all REP mappings, UDP relay reply path, FRAG behavior, target dial and close lifecycle.
4. Finish exact HTTP CONNECT dial/error/close behavior.
5. Finish exact QUIC config/TLS/stream bootstrap/close semantics and pool retry/dead-IP behavior.
6. Implement/validate WSS UDP.
7. Complete runtime non-MUX 1:1 behavior.
8. Add executable GoWay <-> RushWay interoperability tests.
9. Validate release builds/artifacts and only then cut v0.0.1.

Do not mark a subsystem complete merely because its parser or constructor compiles.
