# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current status

**Overall engineering completion: ~50% (estimate).** This is migration progress, not a claim of production readiness.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 compatibility tests: `[ ]`
- Stage 5 release builds: `[~]`
- Stage 6 GitHub Actions: `[x]`
- Stage 7 v0.0.1 release: `[ ]`

## Stable verified baseline

### Current main
- Current verified commit: `f36b75767531c05e83d0cdd9a336b4d1ebc50a64` — `perf: make default WebSocket receive path ownership based`.
- This commit is the current optimization baseline after the latest green CI.

### GitHub Actions Run 56
- Workflow: `RushWay CI`
- Run: `34746698462`
- Head: `f36b75767531c05e83d0cdd9a336b4d1ebc50a64`
- Linux test: **success**
- Linux release build: **success**
- MUX benchmark: **success**
- Windows x64 GNU build: **success**
- Rust toolchain actually executed: `rustc 1.82.0 (f6e511eec 2024-10-15)`.
- Unit tests recorded by CI: mux_bench target **6/6 passed**, main target **28/28 passed**.

### MUX benchmark evidence
CI executed:
`cargo run --release --bin mux_bench`

Measured output on the GitHub runner:
`rushway_mux_decode_owned_reused iters=200000 payload=32768 ns/op=1.55 MB/s=20206592.20`

Interpretation:
- The owned MUX header/decode operation itself is extremely cheap in release mode.
- Do **not** spend the next optimization pass on micro-optimizing `MuxHeader::parse` unless a real end-to-end profile identifies it.
- The more important remaining cost is moving/queueing the decoded frame through runtime paths without copying payload bytes.

## Latest continuous-pass work

### WebSocket receive ownership
- [x] `read_frame` no longer clones data-frame payloads before returning.
- [x] Existing runtime call sites keep the same signature, so this optimization was low-risk.
- [x] Unit test proves owned binary frame storage is transferred without clone.
- [x] Run 56 verified masked and unmasked WebSocket round trips still pass.

### MUX owned decode infrastructure
- [x] `OwnedMuxFrame` exists in `src/protocol.rs`.
- [x] `MuxFrame::decode_owned(Vec<u8>)` validates the exact 7-byte MUX header and retains original storage.
- [x] `OwnedMuxFrame::payload()` exposes the payload slice without copying.
- [x] `OwnedMuxFrame::into_storage()` allows storage reuse in the microbenchmark.
- [x] Run 56 verifies `owned_decode_keeps_original_payload_storage`.
- [ ] Runtime server MUX DATA path must switch from `MuxFrame::decode()` to `decode_owned()` and carry the owned frame through the stream queue.
- [ ] Runtime client MUX DATA path must switch from `MuxFrame::decode()` to `decode_owned()` and write `frame.payload()` directly.
- [ ] Remove now-unneeded owned-frame dead-code warnings after runtime integration.

### CI benchmark history
- Run 54 `34746269466` on `ade6df82d4f0a62b4b8be8e3baf7b4d966fd6ee0`: **success**; added the MUX benchmark to CI.
- Run 55 `34746666507` on `a9cd1f414762762514d5bed6102d339aa1f854a3`: superseded by Run 56; ownership receive path was introduced.
- Run 56 `34746698462` on `f36b75767531c05e83d0cdd9a336b4d1ebc50a64`: **success** and is the current verified baseline.

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
- [ ] True GoWay <-> RushWay interoperability evidence.

## Known warnings / cleanup backlog

Run 56 passed, but warnings remain. They are not current correctness failures.

- `OwnedMuxFrame` was still unused by the main runtime at Run 56.
- Two UDP sender functions have unreachable `Result::<()>::Ok(())` after infinite loops.
- Some parser helpers/constants are currently unused because the runtime uses integrated parsing paths.
- These warnings should be cleaned after the owned MUX runtime integration, not by suppressing warnings globally.

## Compatibility / correctness policy

No build, test, interoperability, benchmark, platform artifact or Release is marked complete without execution evidence. Repository writes alone are not verification evidence.

Functional compatibility has priority over speculative performance changes. Every optimization must preserve:
- exact 7-byte MUX framing;
- WebSocket masking/framing rules;
- XOR handshake/data transform boundaries;
- stream FIN/RST semantics;
- bounded queue behavior;
- Rust 1.82 compatibility.

## Next continuous sequence

1. **Integrate `OwnedMuxFrame` into runtime server/client MUX DATA paths** without changing wire behavior.
2. Run CI immediately; revert/fix any ownership or async lifetime errors.
3. Add direct end-to-end forwarding benchmark so microbenchmark numbers are not mistaken for proxy throughput.
4. Measure lock contention around the shared WebSocket writer before changing queue/session architecture.
5. Add explicit backpressure accounting against the GoWay observed queue limits before increasing channel capacity.
6. Finish exact WSS/browser-profile interoperability and WSS UDP behavior.
7. Extract/implement QUIC, pool/reuse/retry/dead-IP and non-MUX behavior.
8. Execute the full GoWay v1.8.4 ↔ RushWay interoperability matrix.
9. Add Debian 12 x64 and KWRT/OpenWrt ARMv7 build jobs and artifact packaging.
10. Only after all required evidence is green, create v0.0.1 release.

## AI handoff / relay rule

Any future or relay AI must:

1. Read `PROGRESS.md` and `SPEC.md` before changing code.
2. Start from the verified current commit recorded above; do not assume local state.
3. Check the latest GitHub Actions run before trusting a performance or compatibility claim.
4. Make one logical optimization at a time, then execute CI.
5. Record the exact commit SHA, workflow run ID, test results and benchmark output here immediately after verification.
6. Never mark GoWay interoperability, WSS UDP, QUIC, non-MUX, production readiness or release readiness complete without actual execution evidence.
7. Preserve this file as the canonical relay memory so another AI can continue without reconstructing the entire session.
