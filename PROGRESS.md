# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current status

**Overall engineering completion: ~48% (estimate).** This is migration progress, not a claim of production readiness.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 compatibility tests: `[ ]`
- Stage 5 release builds: `[ ]`
- Stage 6 GitHub Actions: `[~]`
- Stage 7 v0.0.1 release: `[ ]`

## Latest continuous-pass work

### WSS compatibility audit
- [x] Compared RushWay WSS client structure with GoWay v1.8.4's documented WSS policy and WebSocket header behavior.
- [x] Confirmed RushWay uses the effective FakeHost for TLS SNI and HTTP Host, matching the intended CDN/reverse-proxy compatibility model.
- [x] Corrected `Sec-Fetch-Site`: `same-origin` when SNI hostname equals the WebSocket Host, otherwise `cross-site`.
- [x] Kept `Origin` scheme as `https://` for WSS and derived it from the effective Host.
- [x] Preserved the GoWay-compatible binary WebSocket `MUX\\n` / `OK\\n` XOR handshake boundary.
- [ ] Verify exact browser-profile headers (User-Agent, Accept-Language and Chromium hints) against the full v1.8.4 implementation.
- [ ] Real WSS TCP interoperability test.
- [ ] WSS UDP path.

### CI verification
- [x] Linux `cargo test --all-targets` passed for UDP transport commit `7bb82a08049a4259e329c878ce74c49fd13df273` in Actions run `34702137868`.
- [x] Linux release build passed in the same run.
- [x] Windows x64 GNU build passed in the same run.
- [x] Corrected-XOR CI run `34701378943` previously passed test, release and Windows jobs.
- [x] WSS dependency compatibility was corrected for Rust 1.82: `zeroize = 1.7.0`, `jobserver = 0.1.32`.
- [x] Rustls `ServerName` import was corrected to `rustls::pki_types::ServerName` in commit `c66ed87b3cc3030ea8eaa72655413dc9f77f50c3`.
- [x] Full WSS implementation CI run `34702979646` passed Linux tests, Linux release build, and Windows x64 GNU build.
- [ ] CI verification for the `Sec-Fetch-Site` compatibility correction.

## Runtime slice currently present

- [x] Local SOCKS5 no-auth TCP CONNECT front-end.
- [x] Local HTTP CONNECT front-end.
- [x] SOCKS5 static success response.
- [x] TCP target dial with timeout and TCP_NODELAY.
- [x] Plain WebSocket client/server handshake integration.
- [x] GoWay-compatible XOR MUX hello/OK authentication boundary.
- [x] MUX SYN/DATA/FIN/RST runtime path.
- [x] Server-side bounded per-stream command queue.
- [x] Target-to-MUX DATA chunking at uint16 payload boundary.
- [x] Target EOF -> MUX FIN.
- [x] Target read error -> MUX RST.
- [x] Local EOF -> MUX FIN.
- [x] Remote MUX FIN -> local half-close.
- [x] Remote MUX RST -> local connection termination.
- [x] SOCKS5 UDP relay implementation slice.
- [x] TLS/WSS client TCP forwarding path compiles and passes Linux/Windows CI.
- [ ] WSS UDP.
- [ ] Runtime QUIC.
- [ ] Runtime connection pool/reuse/retry/dead-IP.
- [ ] Runtime non-MUX mode.

## Verification policy

No build, test, interoperability, benchmark, platform artifact or Release is marked complete without execution evidence. Repository writes alone are not verification evidence.

## Exact verification status

- Latest fully green implementation verification before the latest header correction: Actions run `34702979646` — Linux test **success**, Linux release build **success**, Windows x64 GNU build **success**.
- Latest code correction commit: `5fefef93ea5128cd6f3b470dc1f4378b8921f728`.
- Local `cargo test`: not run; no local Rust toolchain execution used.
- Local `cargo build --release`: not run.
- Actual GoWay ↔ RushWay interoperability: **not yet executed**, therefore not claimed.

## Next continuous sequence

1. Run CI for the WSS `Sec-Fetch-Site` correction and fix any Rust 1.82 issues.
2. Finish exact browser-profile header comparison against GoWay v1.8.4.
3. Add targeted WSS unit/integration coverage for certificate modes and handshake framing.
4. Add WSS UDP only after WSS TCP behavior is reconciled.
5. Reconcile UDP FRAG/error/close behavior against GoWay v1.8.4.
6. Implement QUIC and exact pool/retry/dead-IP behavior.
7. Add non-MUX and remaining CLI/config/DNS behavior.
8. Build real bidirectional GoWay/RushWay interop and high-concurrency tests.
9. Add Debian/KWRT builds and standalone packaging.
10. Only after all required evidence is green, create v0.0.1 release.

## Handoff rule

Any future/relay AI **must read `PROGRESS.md` and `SPEC.md` first**, inspect current `main`, continue from the first unchecked item, update this file after every meaningful step, record exact evidence, and never claim completion without execution evidence.
