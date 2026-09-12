# RushWay v0.0.1 — GoWay v1.8.4 Compatibility Specification

## Purpose

This document is the migration contract for RushWay. The compatibility baseline is the GoWay v1.8.4 source at commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` in `CFM503/way/goway`.

**Rule:** implementation must follow observed v1.8.4 behavior, not assumptions about how a proxy of this type normally works.

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

The v1.8.4 integration tests exercise SOCKS5 TCP CONNECT, HTTP CONNECT, SOCKS5 UDP ASSOCIATE and TCP echo forwarding. The GoWay source also contains a static successful SOCKS5 response:

`05 00 00 01 00 00 00 00 00 00`

and a static HTTP CONNECT success response:

`HTTP/1.1 200 Connection Established\r\n\r\n`.

The v1.8.4 test suite explicitly starts SOCKS5 with `[VER=5, NMETHODS=1, METHOD=0=no-auth]` and then tests CONNECT. UDP ASSOCIATE is separately exercised end-to-end against a UDP echo server. Truncated UDP-associate reads were hardened with strict `io.ReadFull` error/EOF checks in the v1.8.4 source. citeturn432file0turn418file0

The GoWay/PyWay lineage confirms the SOCKS5 no-auth greeting response is `05 00`; address types include IPv4, domain and IPv6, and the UDP datagram envelope is RFC1928-style reserved bytes + FRAG + ATYP + address + port + payload. RushWay now has isolated parsers for the SOCKS5 greeting, CONNECT/UDP ASSOCIATE request, UDP datagram envelope and HTTP CONNECT authority. These are **parser primitives, not the forwarding implementation**. citeturn434file0turn431file3

HTTP CONNECT requests are expected to be read until a complete header terminator rather than assuming one TCP read contains the complete request. The v1.8.4 changelog specifically records support for `\r\n\r\n` / `\n\n` framing with the 8192-byte header limit. The integration test uses the form `CONNECT 127.0.0.1:<port> HTTP/1.1` with a Host header. citeturn418file0turn420file4

**Important extraction status:** exact GoWay `goway.go` parser/control-flow code for every SOCKS5 error reply, UDP relay lifecycle, HTTP method/error mapping and target-resolution path is still being extracted. Do not mark this section fully complete until those exact branches are reconciled.

## DNS

Remote DNS uses a configured server and caches resolved addresses. Failed remote resolution falls back to the system resolver. The resolver has a timeout and caches successful results. Upstream dialing can use resolved IPs while preserving the configured hostname for TLS/SNI where required.

## Connection pool / MUX pool

Client mode has pooling/reuse infrastructure. MUX mode maintains multiple physical MUX sessions and assigns logical streams over those sessions. Non-MUX mode falls back to a 1:1 pooled connection model.

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

## TLS / browser profile behavior

The v1.8.4 source contains multiple browser profiles bundling User-Agent, Accept-Language, Chromium Client Hints where applicable, TLS cipher preferences, and curve preferences. WSS initialization pre-builds TLS configurations for profiles. `-fakehost` is used for Host/SNI/CDN/reverse-proxy scenarios.

RushWay compatibility implementation must first reproduce functional TLS/SNI behavior. Browser fingerprint parity is a separate compatibility item and must be implemented from observed source behavior, not invented values.

## QUIC

GoWay v1.8.4 uses `quic-go` and exposes QUIC upstream schemes. RushWay must inventory the exact QUIC listener/client/session/stream behavior from the remaining source before implementing this subsystem. QUIC is **not** considered specified merely because the CLI accepts `quic://`.

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

This file remains an **intermediate verified baseline**. WebSocket functional behavior is substantially extracted. SOCKS5/HTTP front-end wire shapes and test coverage are now documented and isolated Rust parser primitives exist, but exact GoWay control-flow/error branches and UDP relay lifecycle remain to be reconciled.

## Next action

Continue Stage 2 extraction of the exact GoWay SOCKS5/HTTP control flow and then move to exact QUIC listener/client/session/stream behavior. Do not mark proxy front-end complete until parser behavior and forwarding lifecycle are covered by actual Rust tests and later GoWay interop tests.
