# Changelog

All notable changes to this project will be documented in this file.

## [v0.0.7] - 2026-09-15

### Fixed
- **Unified WSS Transport for MUX Physical Sessions**:
  - Exported authoritative crate-level `connect_wss_upstream` transport helper in `src/wss_client.rs`.
  - Clarified execution boundaries: verified that `wss://` with `-mux` routes through `WssSessionPool`, with all parallel physical sessions established via the Cloudflare-aware pipeline.
  - Ensured `-fakehost` applies consistently to TLS SNI and HTTP `Host` for all MUX physical sessions.
  - Ensured Cloudflare Edge fallback via DNS resolution of `fakehost` is available across each physical session dial.
  - Fixed plain `ws://` origin scheme in `src/mux_pool.rs` to use `http://` instead of `https://`.
- **Workflow Cleanup**:
  - Removed completed one-shot workflows (`.github/workflows/cli-align-once.yml` and `.github/workflows/stress-gate-once.yml`).
- **Regression Coverage**:
  - Added in-process mock WSS server integration test verifying TLS SNI, HTTP `Host`, `Origin`, and encrypted `MUX\n` / `OK\n` handshake.

## [v0.0.6] - 2026-09-15

### Added
- **Cloudflare CDN / FakeHost Alignment**:
  - Full separation of TCP connection address and TLS SNI / HTTP Host identity.
  - Aligned TLS ServerName, HTTP `Host`, and `Origin` (`https://<fakehost>`) with GoWay's production specification.
  - Path retention (`/path`) across custom CDN WebSocket routing endpoints.
  - Automatic Cloudflare Edge fallback via DNS resolution of `fakehost` when primary IP connection fails.
  - Multi-candidate IPv4 fallback dialing skipping the already-attempted primary IP.
- **Unified WSS Dialing Pipeline**:
  - Unified `open_upstream` pipeline across MUX physical sessions, non-MUX connections, and UDP-over-WSS relays.
  - Informative logging of WSS connection target, TLS ServerName, HTTP Host, path, and fallback attempts without sensitive key leakage.
- **Automated Tests**:
  - Unit tests for WSS URL parsing with IP addresses and custom endpoints (`/pyway`).
  - Unit tests verifying header construction (`Host`, `Origin`, `Upgrade`, `Sec-WebSocket-Version`).
  - Unit tests confirming no raw IP leakage into `Host` or `Origin` headers when `fakehost` is configured.
  - Unit tests for multiple IPv4 DNS answer parsing and Cloudflare fallback eligibility rules.
  - CLI parser regression test for the full Cloudflare FakeHost command with 8 MUX sessions.

### Changed
- **Default Maximum Connections**: Set runtime default `max_connections` to 1,500 (aligning with v0.0.5/v0.0.6 requirements).
- **CLI Help & Compatibility**:
  - Updated `--fakehost` and `--max-conn` help text.
  - Ensured legacy flags `-help`, `-fakehost`, and related variants remain fully functional.
- **Version**: Bumped version to `0.0.6`.
