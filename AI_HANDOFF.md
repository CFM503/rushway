# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-13 — v0.0.2 formal-release checkpoint

### Target

- Repository: `CFM503/rushway`
- GoWay baseline: v1.8.4, commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Formal release target: `v0.0.2`
- Targets: Windows x64, Debian 12 x64, KWRT/OpenWrt ARMv7
- Branch: `main`
- Current CI hardening commit: `32cc8df6236055e2b85f70b34c7326baf2e941bb`

### Newly completed in this continuation

- Runtime/protocol hardening commits through `ad88a9d218af7f841f74d0231864e56723fff840` are retained as the current source baseline.
- `32cc8df6236055e2b85f70b34c7326baf2e941bb` — consolidated CI, removed redundant `build-smoke.yml`, pinned runners to `ubuntu-22.04`, enabled branch concurrency cancellation, and added explicit format/check/all-features test gates.

### CI recovery finding

The public repository's GitHub Actions runner issue is now resolved. Run `34765507084` successfully provisioned an Ubuntu 22.04 runner, checked out `main`, installed Rust 1.82.0 and executed workflow steps. The first real failure was not infrastructure: `cargo fmt --all -- --check` failed because the Rust 1.82.0 installation uses the minimal profile and does not include the `rustfmt` component.

The exact runner output says:

- `cargo-fmt is not installed for the toolchain 1.82.0-x86_64-unknown-linux-gnu`
- `help: run rustup component add rustfmt to install it`

Therefore Actions is operational again; the next blocker is a CI configuration issue.

### Required next CI fix

The `test` job should change the Rust toolchain setup from:

`toolchain: 1.82.0`

to:

`toolchain: 1.82.0`
`components: rustfmt`

The direct workflow write for `.github/workflows/ci.yml` was attempted after the failure was diagnosed but was blocked by the current tool safety layer. Do not claim that rustfmt installation has already landed.

### Important interoperability findings

GoWay's own integration tests use forms such as `-up ...`, `-log ERROR`, `-mux=true`, and `-block-local=false`; RushWay's parser normalizes these into Clap-compatible arguments.

The shared DNS resolver covers server target resolution plus plain WS MUX, plain WS non-MUX, WSS and QUIC hostname dialing/target resolution. IP literals bypass DNS, WSS/QUIC preserve the original logical hostname for TLS/HTTP identity, and remote DNS replies are transaction-ID checked.

The local CONNECT paths for plain-WS MUX and WSS MUX consume the first upstream frame so `RST` becomes SOCKS5 General Failure / HTTP 502, while `DATA` is forwarded after success and `FIN` produces clean EOF after success.

QUIC client connection reuse retries after failed `open_bi`; QUIC target-dial failure maps to local SOCKS5/HTTP failure; QUIC server target TCP sockets receive the common TCP policy.

### Current verification boundary

Current-head executable verification is not yet passed, but Actions is now demonstrably executing real steps. The first executable gate currently failing is `cargo fmt` due to the missing `rustfmt` component, not a runner failure.

### Remaining work before the 100% gate

1. Fix the Rust 1.82 CI toolchain to install `rustfmt` and rerun.
2. Continue through `cargo check --all-targets --all-features`, tests, release build, benchmarks and artifact builds; fix actual failures as they appear.
3. Run GoWay ↔ RushWay TCP/UDP interop for WS/WSS/QUIC, including dead-IP/connection-state/TLS-SNI cases.
4. Run 1/100/500/1000-stream stress, large payload and mixed slow/fast workloads.
5. Run c1/c8/c32 benchmarks and Windows x64 / Debian 12 / ARMv7 smoke artifacts.
6. Create and smoke-test `v0.0.2` only after executable evidence is complete.

### Release/tag constraint

`Cargo.toml` is `0.0.2`. `v0.0.2` must not be claimed as released until the executable gate is green and a tag is verified.

### Three-file relay contract

Only these three files are canonical AI handoff state:

1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because the source paths exist. The 100% gate requires executable evidence and release validation.
