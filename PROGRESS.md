# RushWay v0.0.1 — Continuous Handoff Progress

> Purpose: allow any AI/developer to resume work without repeating completed stages.
>
> Compatibility baseline: GoWay v1.8.4, stable source commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
>
> Release target: **v0.0.1**
>
> Release artifact requirement: each target platform must produce a standalone executable. Source may be modular; the published runtime artifact must be a single executable file.

## Status legend

- `[ ]` Not started
- `[~]` In progress
- `[x]` Completed and committed
- `[!]` Completed but not independently verified

## Current overall status

**Stage 1 — Project bootstrap: `[~]`**

Last known commits in `CFM503/rushway`:
- `beeb5af64a16a4af806d476b37b7c25716196c0d`
- `9645a5be092a1889a563bef39f36defca9aa8a12`

The repository currently contains the initial Rust project skeleton. Do **not** claim v0.0.1 is releasable yet.

## Stages

### Stage 1 — Repository/bootstrap
- [x] Confirm `CFM503/rushway` exists and is writable.
- [x] Initialize Rust/Cargo project.
- [x] Set package version to `0.0.1`.
- [x] Add initial CLI/config skeleton.
- [x] Add this progress/handoff document.
- [ ] Add final README and compatibility documentation.
- [ ] Add LICENSE if required.

### Stage 2 — GoWay v1.8.4 specification extraction
- [ ] Read the complete GoWay v1.8.4 `goway.go`.
- [ ] Extract every CLI option and preserve compatible invocation syntax.
- [ ] Extract JSON/config schema and defaults.
- [ ] Inventory client/server modes.
- [ ] Inventory transport implementations.
- [ ] Inventory MUX/frame/session semantics.
- [ ] Inventory forwarding and lifecycle behavior.
- [ ] Inventory TLS/QUIC behavior.
- [ ] Inventory connection pool/buffer behavior.
- [ ] Map all relevant v1.8.4 tests.

### Stage 3 — Rust core implementation
- [ ] Implement CLI/config compatibility.
- [ ] Implement protocol constants/frame encoding/decoding.
- [ ] Implement MUX stream/session management.
- [ ] Implement WebSocket transport.
- [ ] Implement TCP forwarding/listeners.
- [ ] Implement TLS support.
- [ ] Implement QUIC support where present in the v1.8.4 baseline.
- [ ] Implement connection pooling/reuse.
- [ ] Implement buffering/backpressure without changing wire semantics.
- [ ] Implement cancellation/EOF/RST/error handling.
- [ ] Implement reconnect/lifecycle behavior.

### Stage 4 — Compatibility and tests
- [ ] Rust unit tests pass.
- [ ] Rust integration tests pass.
- [ ] GoWay client ↔ RushWay server interoperability.
- [ ] RushWay client ↔ GoWay server interoperability.
- [ ] Single stream test.
- [ ] Multi-stream test.
- [ ] 100-stream test.
- [ ] 500-stream test.
- [ ] 1000-stream test.
- [ ] Large payload/file test.
- [ ] Slow/fast concurrent stream test.
- [ ] EOF/RST test.
- [ ] Disconnect/reconnect test.
- [ ] TLS test.
- [ ] QUIC test.
- [ ] Invalid configuration/argument tests.

### Stage 5 — Single-file release builds
- [ ] Windows x86_64 executable builds successfully.
- [ ] Debian 12 x86_64 executable builds successfully.
- [ ] KWRT/OpenWrt ARMv7 executable builds successfully.
- [ ] Verify release artifacts are standalone executables.
- [ ] Verify no unnecessary runtime dependency is bundled.
- [ ] Verify `rushway --version` reports `0.0.1`.
- [ ] Verify `rushway --help` is usable.

### Stage 6 — GitHub Actions
- [ ] Add CI checks.
- [ ] Add release workflow.
- [ ] Build Windows x86_64.
- [ ] Build Debian 12 x86_64.
- [ ] Build KWRT/OpenWrt ARMv7.
- [ ] Package artifacts with stable names.
- [ ] Run tests before publishing.
- [ ] Publish only after all required builds/tests pass.

### Stage 7 — v0.0.1 release
- [ ] Create git tag `v0.0.1`.
- [ ] GitHub Actions release workflow succeeds.
- [ ] Release exists and is not draft.
- [ ] Windows artifact downloadable.
- [ ] Debian 12 artifact downloadable.
- [ ] KWRT ARMv7 artifact downloadable.
- [ ] Final release notes document compatibility and known limitations.
- [ ] Final end-to-end verification completed.

## Handoff rules

1. **Never mark a stage `[x]` merely because code was written.** Compilation/test evidence is required for implementation and release stages.
2. **Never create a fake/placeholder release.** `v0.0.1` is only releasable after required builds and tests succeed.
3. **Do not change the GoWay v1.8.4 wire protocol for optimization unless interoperability tests prove compatibility.**
4. **Do not assume Rust is faster.** Performance changes must be benchmarked against GoWay v1.8.4.
5. **Preserve CLI/config compatibility wherever technically possible.**
6. **Source may use multiple Rust modules, but each release target must produce a single executable runtime artifact.**
7. At the end of every work session, update this file with:
   - completed checklist items;
   - current commit SHA;
   - exact test/build commands actually run;
   - exact failures still outstanding;
   - the single recommended next stage.

## Resume instruction

When another AI takes over this repository, **read this file first**, inspect the current tree/commits, then continue from the first unchecked item. Do not restart completed stages and do not infer completion without evidence.

## Next recommended action

**Stage 2:** systematically extract the complete GoWay v1.8.4 CLI, configuration, protocol, transport, MUX, forwarding, TLS/QUIC, lifecycle, and test specification before expanding the Rust implementation.
