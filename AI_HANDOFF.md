# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — clean-source recovery checkpoint

### Target

- Repository: `CFM503/rushway`
- GoWay baseline: v1.8.4, commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Formal release target: `v0.0.2`
- Targets: Windows x64, Debian 12 x64, KWRT/OpenWrt ARMv7
- Branch: `main`

### Recovery completed

- `main` was explicitly reset to `07675eb094f40a78655aa33ad30fc087547a013f` to discard the later malformed compiler-repair commits.
- `src/mux_pool.rs` is now back on the requested clean-source checkpoint before the accidental follow-up constructor rewrite.
- The temporary compiler diagnostic and source-repair files are intentionally still present at this checkpoint; they will be removed only after the real source compiles cleanly.

### Exact remaining source repairs

The remaining known compiler blockers are limited to five edits:

1. `src/mux_pool.rs` — `MuxSessionPool::new`: remove the stray semicolon after the `Arc::new(Self { ... })` tail expression.
2. `src/wss_client.rs` — `WssSessionPool::new`: remove the stray semicolon after the `Arc::new(Self { ... })` tail expression.
3. `src/quic.rs` — `QuicClientPool::new`: remove the stray semicolon after the `Arc::new(Self { ... })` tail expression.
4. `src/quic.rs` — `TransportConfig::stream_receive_window`: unwrap `VarInt::from_u64(8 * 1024 * 1024)` with `expect("8MiB fits QUIC VarInt")`.
5. `src/quic.rs` — `TransportConfig::receive_window`: unwrap `VarInt::from_u64(16 * 1024 * 1024)` with `expect("16MiB fits QUIC VarInt")`.

Do not run or restore `scripts/repair_compiler_issues.py` as part of the formal build. Do not treat a temporary repair workflow as permanent build logic.

### Mandatory next gate

After those five source edits:

1. Remove `.github/workflows/compiler-diagnostic.yml`.
2. Remove `.github/workflows/source-repair-once.yml`.
3. Remove `scripts/repair_compiler_issues.py`.
4. Run clean `fmt -> cargo check --all-targets --all-features -> cargo test --all-targets --all-features -> cargo build --release`.
5. Record actual executable results in this file and `PROGRESS.md`.

### Release work after the compiler gate

- c1/c8/c32 benchmark measurements.
- Windows x64, Debian 12 x64 and ARMv7/OpenWrt smoke artifacts.
- GoWay <-> RushWay bidirectional WS/WSS/QUIC TCP+UDP interoperability.
- dead-IP, connection-state and TLS-SNI interoperability cases.
- 1/100/500/1000-stream stress plus large-payload and mixed slow/fast workloads.
- Final `v0.0.2` tag and GitHub Release only after executable evidence is green.

### Release rule

`Cargo.toml` may be `0.0.2`, but RushWay is not 100% complete and `v0.0.2` is not released until the executable gate and release smoke validation are green.

### Three-file relay contract

1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.

## 2026-09-14 本地修复与互通验证更新

### 当前分支

`fix/v0.0.2-compiler-clean`

### 本地源码修复

在本地工作区直接修复了以下编译错误：

1. `src/mux_pool.rs`

   * 修复 `MuxSessionPool::new()` 中 `Arc::new(Self { ... });`
   * 删除末尾多余 `;`

2. `src/wss_client.rs`

   * 修复 `WssSessionPool::new()` 中 `Arc::new(Self { ... });`
   * 删除末尾多余 `;`

3. `src/quic.rs`

   * `VarInt::from_u64(8 * 1024 * 1024)` 增加 `.expect("8MiB fits QUIC VarInt")`
   * `VarInt::from_u64(16 * 1024 * 1024)` 增加 `.expect("16MiB fits QUIC VarInt")`
   * 修复 `QuicClientPool::new()` 中 `Arc::new(Self { ... });`
   * 删除末尾多余 `;`

### 编译与测试结果

`cargo fmt -- --check`

* PASS

`cargo check --all-targets`

* PASS
* 无 error
* 当前仍存在若干 Rust warnings，但不阻塞编译

`cargo test --all-targets --all-features`

* PASS
* 38 passed
* 0 failed
* 0 ignored

`cargo build --release`

* PASS
* Release binary 已生成

Windows Release 文件：

* `target/release/rushway.exe`
* 大小约 3,974,144 bytes

`rushway.exe --help`

* PASS
* 程序可以正常启动并显示帮助

### GoWay 基线

已确认本地 GoWay：

* 路径：`D:\SOFT\ROUTER\goflyway_windows_386\goway.exe`
* 版本：`GOWAY v1.8.4`

### 本地 GoWay ↔ RushWay WebSocket/MUX 测试

GoWay 服务端启动：

`127.0.0.1:18880`

* TCP/WebSocket 正常监听
* QUIC UDP 同端口监听
* key：`test123`

RushWay 客户端启动：

`127.0.0.1:11080`

* upstream：`ws://127.0.0.1:18880`
* 默认 4 physical MUX sessions

GoWay 日志确认：

* 4 个 Mux Session requested
* 4 个 Mux Session active
* RushWay → GoWay WebSocket/MUX 建连成功

### 当前互通测试结果

测试公网 HTTPS：

`curl.exe -v --proxy socks5h://127.0.0.1:11080 https://example.com/ -I`

结果：

* SOCKS connection 可以建立
* TLS handshake 阶段失败
* `Recv failure: Connection was aborted`
* `curl: (35)`

测试本机 HTTP 时，RushWay 默认 `block-local` 导致请求被关闭。

随后 RushWay 使用：

`--no-block-local`

重新启动。

本机 HTTP 服务：

* `python -m http.server 18080 --bind 127.0.0.1`
* `127.0.0.1:18080`

关闭 `block-local` 后重新测试：

`curl.exe -v --proxy socks5h://127.0.0.1:11080 http://127.0.0.1:18080/`

当前现象：

* curl 长时间停留在 `Trying 127.0.0.1:11080...`
* GoWay 没有出现新的业务连接日志
* GoWay 只显示 MUX Session active
* 因此 MUX 物理连接已经建立，但 SOCKS 请求到实际业务 stream 的处理仍未完成

### 当前判断

源码层面：

* `fmt` PASS
* `check` PASS
* `test` PASS
* `release build` PASS

当前未完成的是运行时互通。

目前故障范围已经缩小到：
`RushWay 本地 SOCKS 接入 → MUX stream 创建/发送 → GoWay server 业务处理`

不能把项目称为 100%。

### 下一步

下一步不要修改 GoWay，也不要继续测试 QUIC。

应该继续检查 RushWay 在收到 SOCKS 请求后的运行日志，确认：

1. 是否进入 SOCKS5 请求解析；
2. 是否成功创建 mux stream；
3. 是否发送 CONNECT/目标地址；
4. 是否等待远端 response；
5. 是否发生 channel / stream / protocol 阻塞。

重点测试仍然是：

`RushWay SOCKS5 → WebSocket MUX → GoWay v1.8.4 → TCP target`

待该链路成功后，再依次验证：

* HTTP proxy
* WSS
* QUIC TCP
* QUIC UDP
* UDP ASSOCIATE
* 多 stream
* 压力测试
* 最终 v0.0.2 release gate

### 重要说明

当前本地工作区已经不是 clean baseline：
正式源码修复已直接写入以下文件：

* `src/mux_pool.rs`
* `src/wss_client.rs`
* `src/quic.rs`

在运行时互通完全通过以前，不应宣称 100%，也不应创建 `v0.0.2` 正式 tag/release。
