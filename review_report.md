# RushWay 深度逐行代码审查与正向优化修复报告

> **审查基准**：基于 Opus 5.5 全仓深度逐行审查标准，面向高并发、零拷贝、无锁/低锁争用及长连接稳定性。  
> **协议与端到端对齐**：完全对齐 GoWay v1.8.18 协议规范与时序行为。  
> **审查目标目录**：`D:\SOFT\ai\github\rushway`  
> **核心原则**：**保证所有修复均为严格的“正向优化”**（消除内存/流泄漏、杜绝物理连接雪崩、消除 SingleFlight 竞态、提升池化复用率，无额外多余内存分配，绝不引起性能或架构倒退）。

---

## 一、审查问题总览表 (Summary Table)

| 序号 | 缺陷严重度 | 涉及模块与文件 | 代码具体行号 | 问题类型 | 正向优化收益判定 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **1** | **P0 严重** | `src/wss_client.rs` | L1087, L1103, L1112 | 早期错误返回 `?` 导致活跃流泄漏、门控孤儿、上传协程悬挂 | **正向优化**：杜绝流计数器漂移与物理会话虚假打满，保证协程及时释放 |
| **2** | **P0 严重** | `src/wss_client.rs` | L807 | 帧解码失败直接 `?` 抛错导致整个物理底层连接断开 | **正向优化**：消除前向兼容未知帧/坏帧引发的全会话所有并发流雪崩 |
| **3** | **P0 严重** | `src/wss_client.rs` | L860-868 | 背压队列溢出或通道关闭时遗漏清理 `session.gates` | **正向优化**：彻底消除 `CreditGate` 孤儿门控内存泄漏与等待协程卡死 |
| **4** | **P1 重要** | `src/wss_client.rs` | L1017 | WSS 会话池缺少并发容量等待与唤醒机制，并发尖峰瞬时拒连 | **正向优化**：平滑吸收突发并发请求（吸收 500ms 抖动），提高系统吞吐 |
| **5** | **P1 重要** | `src/runtime.rs` / `src/common.rs` / `src/mux_pool.rs` / `src/wss_client.rs` / `src/quic.rs` | 全局 `relay_buf` / `recycle_buf` | 协程被 `abort()` 或错误提前退出时绕过手动回收，缓冲池长年饥饿 | **正向优化**：引入 RAII `RelayBufGuard`，100% 自动入池，彻底消除 128KB 频繁堆申请 |
| **6** | **P1 重要** | `src/dns.rs` | L150 | SingleFlight `_guard` 提前析构导致并发重复远程 DNS 查询 | **正向优化**：修复竞态窗口，保证并发相同域名只发起一次远程 DNS 请求 |
| **7** | **P1 重要** | `src/runtime.rs` | L1346, L1423-1426 | UDP 服务端 `known_targets` 集合无上限增长隐患 | **正向优化**：增加容量上限（4096 条），杜绝大规模扫描攻击下的内存无界泄露 |
| **8** | **P2 优化** | `src/ws.rs` | L14 | 最大帧长度 `MAX_WS_FRAME_SIZE` 设为 64MB 存在 DoS 攻击面 | **正向优化**：对齐 GoWay 12MB+64 字节基线，防止大帧声明导致 OOM 崩溃 |
| **9** | **P2 优化** | `src/runtime.rs` | L837-854 | 重复 SYN 流处理仅发 RST 拒连，未主动终结和清理旧流 | **正向优化**：及时清理僵死旧流，释放关联协程与资源，提升流自愈能力 |
| **10** | **P2 优化** | `src/mux_pool.rs` / `src/wss_client.rs` | `mux_pool.rs:813-816`, `wss_client.rs:1087` | 本地客户端下行写失败异常退出时未向服务端发送 RST | **正向优化**：避免服务端盲目向死连接推送数据，节约网络带宽与服务端资源 |

---

## 二、逐项问题深度分析与精确修复方案

### 1. [P0] WSS 客户端流异常退出跳过尾部清理导致流与协程泄漏
- **文件与行号**：[`src/wss_client.rs`](file:///D:/SOFT/ai/github/rushway/src/wss_client.rs#L1087-L1130)（第 1087、1103、1112 行）
- **问题机理**：
  在 `handle_wss_client_stream` 中，下行读取循环中直接使用了 `?` 操作符：
  ```rust
  write_res?; // 第 1087 行
  ...
  send_mux_parts(...).await?; // 第 1103 行
  ...
  local_wr.shutdown().await?; // 第 1112 行
  ```
  当本地客户端断连导致 `write_res` 报错，或发送 WINDOW refund 帧失败，或 shutdown 失败时，函数提前退出，直接跳过了后续的清理逻辑：
  - `upload.abort()` 未执行：上传协程在后台永久悬挂；
  - `session.streams.remove(&stream_id)` 未执行：流对象残留在查找表中；
  - `session.active.fetch_sub(1)` 未执行：`active` 计数器永久递增无法归零，导致物理会话被“虚假打满”，后续新连接无法分配到该物理连接；
  - `session.gates.remove(&stream_id)` 未执行：门控泄漏。
  *(注：`mux_pool.rs:824` 此前已专门做过此修复，但 `wss_client.rs` 严重遗漏。)*
- **修复方案**：
  统一退出路径，将所有可能返回 Err 的操作记录到 `let mut result = Ok(());` 并 `break;`，确保一定执行尾部清理：
  ```rust
  // 改造前：
  write_res?;
  send_mux_parts(...).await?;
  local_wr.shutdown().await?;

  // 改造后：
  if let Err(e) = write_res {
      result = Err(e.into());
      break;
  }
  if let Err(e) = send_mux_parts(...).await {
      result = Err(e);
      break;
  }
  let _ = local_wr.shutdown().await;
  break;
  // 并在尾部执行完 upload.abort()、streams.remove()、active.fetch_sub(1)、gates.remove() 后统一返回 result
  ```

---

### 2. [P0] 解码非致命/未知帧直接 `?` 抛错导致整个 WebSocket 物理连接断开
- **文件与行号**：[`src/wss_client.rs`](file:///D:/SOFT/ai/github/rushway/src/wss_client.rs#L807)（第 807 行）
- **问题机理**：
  在 `wss_reader_loop` 中：
  ```rust
  let frame = MuxFrame::decode_owned(payload).map_err(|e| anyhow!(e.to_string()))?;
  ```
  如果对端发送了未来版本协议帧、未知扩展帧或轻微损坏的数据包，`decode_owned` 返回 Err，由于 `?` 操作符，导致**整条物理 WebSocket 连接的读取循环直接退出并关闭 session**！该物理通道上复用的所有并发流（数十甚至上百条连接）全部瞬间断开雪崩。
  而在 `mux_pool.rs:395` 和 `runtime.rs:783` 中，均遵循协议向后兼容规范，对于解码失败的帧直接 `continue` 忽略。
- **修复方案**：
  ```rust
  // 改造前：
  let frame = MuxFrame::decode_owned(payload).map_err(|e| anyhow!(e.to_string()))?;

  // 改造后：
  let frame = match MuxFrame::decode_owned(payload) {
      Ok(v) => v,
      Err(_) => continue,
  };
  ```

---

### 3. [P0] 背压溢出/通道关闭处理时遗漏清理 `session.gates`
- **文件与行号**：[`src/wss_client.rs`](file:///D:/SOFT/ai/github/rushway/src/wss_client.rs#L860-L868)（第 860-868 行）
- **问题机理**：
  在 `wss_reader_loop` 中，当某个流的本地接收队列满了（`TrySendError::Full`）或本地已关闭（`TrySendError::Closed`）时：
  ```rust
  if session.streams.write().unwrap_or_else(|e| e.into_inner()).remove(&id).is_some() {
      session.active.fetch_sub(1, Ordering::AcqRel);
  }
  ```
  代码仅移除了 `streams` 并扣减了 `active`，**彻底遗漏了清理 `session.gates`**。这导致：
  1. 每次背压 reset 流都会在 `session.gates` 遗留一个孤儿 `CreditGate`；
  2. 没有调用 `gate.close()`，如果有正在等待该门控信用的协程，将被永久阻塞挂死。
- **修复方案**：
  在移除流的同时清理 gate 并关闭：
  ```rust
  if session.streams.write().unwrap_or_else(|e| e.into_inner()).remove(&id).is_some() {
      session.active.fetch_sub(1, Ordering::AcqRel);
  }
  if let Some(gate) = session.gates.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
      gate.close();
  }
  ```

---

### 4. [P1] WSS 会话池缺少并发容量等待唤醒机制，并发尖峰瞬时拒连
- **文件与行号**：[`src/wss_client.rs`](file:///D:/SOFT/ai/github/rushway/src/wss_client.rs#L1017)（第 1017 行）
- **问题机理**：
  在 `WssSessionPool::acquire` 中，当所有物理会话均达到并发流上限且物理会话数已达到 `session_count` 上限时：
  ```rust
  bail!("all WSS physical MUX sessions are full or unavailable")
  ```
  直接报错并断开客户端代理连接。在并发网页打开或突发几十个请求时，会造成大量连接失败。
  而在 `mux_pool.rs:564-566` 中，实现了通过 `capacity_notify: Arc<Notify>` 进行最多 500ms 的挂起重试等待：一旦有老流释放或新会话加入，立即唤醒获取，平滑吸收并发毛刺。
- **修复方案**：
  为 `WssSessionPool` 增加 `capacity_notify: Arc<Notify>`。在达到上限时进行 `timeout(Duration::from_millis(500), self.capacity_notify.notified()).await` 循环重试；在流关闭和新会话入池时触发 `self.capacity_notify.notify_waiters()`。

---

### 5. [P1] 全局 `relay_buf` / `RELAY_BUFS` 缓冲池在协程被 `abort()` 时无法回收
- **文件与行号**：[`src/runtime.rs`](file:///D:/SOFT/ai/github/rushway/src/runtime.rs#L165-L195) 以及各中继协程
- **问题机理**：
  RushWay 定义了 `RELAY_BUFS` 缓冲池，但其回收依赖于手动调用 `recycle_buf(buf).await`。
  在中继模型中，当连接结束时，各模块普遍使用 `upload.abort()` 直接取消上传协程，或者在循环中因 I/O 错误 `?` 提前退出。
  在 Tokio 协程中，协程一旦被 `abort()`，执行流程立即终止并销毁协程栈，循环外部的 `recycle_buf(buf).await` **永远不会被执行**！导致借出的 128KB 缓冲区直接被系统堆 drop，`RELAY_BUFS` 池子严重饥饿，几乎形同虚设，后续每个连接都要重新经历昂贵的操作系统内存分配。
- **修复方案**：
  将 `relay_buf` 改造为 RAII Guard：
  ```rust
  pub(crate) struct RelayBufGuard {
      buf: Option<Vec<u8>>,
  }
  impl Deref for RelayBufGuard { ... }
  impl DerefMut for RelayBufGuard { ... }
  impl Drop for RelayBufGuard {
      fn drop(&mut self) {
          if let Some(buf) = self.buf.take() {
              recycle_buf_sync(buf);
          }
      }
  }
  ```
  因为归还池操作只涉及一个快速的 `StdMutex` 无锁争用 push，同步回收零代价且在协程被 `abort` 或任何 panic/提前退出时百分之百自动回收入池！

---

### 6. [P1] SingleFlight 防护守卫 `_guard` 提前释放引发并发穿透竞态
- **文件与行号**：[`src/dns.rs`](file:///D:/SOFT/ai/github/rushway/src/dns.rs#L150)（第 150 行）
- **问题机理**：
  在 `dns.rs` 的 `resolve_internal` 中：
  ```rust
  drop(_guard); // 第 150 行：从 IN_FLIGHT 表中删除了当前域名的 key

  match query_result {
      Ok(resolved) => {
          let mut guard = cache().write().unwrap_or_else(|e| e.into_inner());
          bounded_insert(&mut guard, key, (resolved, Instant::now() + CACHE_TTL)); // 第 155 行才写入 CACHE
  ...
  ```
  在第 150 行到第 155 行之间存在竞态时间窗：如果并发请求到达，此时查询缓存未命中，查询 IN_FLIGHT 也未命中，会误以为没有正在进行的查询，从而发起重复的远程 DNS 网络请求，破坏了 SingleFlight 合并请求的核心设计目标。
- **修复方案**：
  删除第 150 行提前的 `drop(_guard);`，让 `_guard` 的生命周期持续到解析结果成功写入 `CACHE` 并通知 `tx` 之后（利用作用域自动释放），彻底消除并发击穿竞态。

---

### 7. [P1] UDP 服务端 `known_targets` 集合无上限增长隐患
- **文件与行号**：[`src/runtime.rs`](file:///D:/SOFT/ai/github/rushway/src/runtime.rs#L1346)（第 1346 行、1423-1426 行）
- **问题机理**：
  ```rust
  let known_targets: Arc<StdMutex<HashSet<SocketAddr>>> = Arc::new(StdMutex::new(HashSet::new()));
  ...
  known_targets.lock().unwrap_or_else(|e| e.into_inner()).insert(addr);
  ```
  在长寿命的 UDP 会话中，`known_targets` 只插入新地址，从未限制上限或淘汰。如果受到大量随机目标地址的 UDP 流量扫描或访问海量外部目标，哈希表将无界膨胀，存在内存耗尽的 DoS 风险。
- **修复方案**：
  对齐 `dns.rs` 的防无界设计，为 `known_targets` 设定合理容量上限（如 4096 条）。当达到上限时，淘汰旧条目或使用有界替换，确保内存使用有严格上限。

---

### 8. [P2] 最大 WebSocket 帧长度 `MAX_WS_FRAME_SIZE` 设为 64MB 存在 DoS 攻击面
- **文件与行号**：[`src/ws.rs`](file:///D:/SOFT/ai/github/rushway/src/ws.rs#L14)（第 14 行）
- **问题机理**：
  ```rust
  pub const MAX_WS_FRAME_SIZE: usize = 64 * 1024 * 1024; // 64 MB
  ```
  GoWay v1.8.18 的单个 MUX 帧最大限制为 `12 * 1024 * 1024 + 64`（12 MB + 64 字节）。RushWay 将其设为 64MB 过大，一旦恶意对端发送伪造的大帧头，服务端会直接申请最高达 64MB 的堆内存，引发内存压力或 OOM。
- **修复方案**：
  将 `MAX_WS_FRAME_SIZE` 收紧为 `12 * 1024 * 1024 + 64`，与 GoWay 保持严格一致，杜绝超大伪造帧攻击。

---

### 9. [P2] 重复 SYN 流处理仅发 RST 拒连，未主动终结和清理旧流
- **文件与行号**：[`src/runtime.rs`](file:///D:/SOFT/ai/github/rushway/src/runtime.rs#L837-L854)（第 837-854 行）
- **问题机理**：
  ```rust
  if guard.len() >= 2048 || guard.contains_key(&stream_id) {
      false
  }
  ```
  当收到重复 `stream_id` 的 SYN 时，服务端回发 RST 并直接放弃，但残留在 `streams` 中的旧流对象及其工作协程并没有被主动通知取消。若旧流处于僵死状态，该 ID 将持续处于占用泄漏状态。
- **修复方案**：
  当发现 `guard.contains_key(&stream_id)` 时，不仅拒绝新流，同时取出旧流条目触发 `old_entry.cancel.send(true)` 并清理该流，使泄漏僵死的旧连接及时释放。

---

### 10. [P2] 本地客户端下行写失败异常退出时未向服务端发送 RST
- **文件与行号**：[`src/mux_pool.rs`](file:///D:/SOFT/ai/github/rushway/src/mux_pool.rs#L813-L816) 及 [`src/wss_client.rs`](file:///D:/SOFT/ai/github/rushway/src/wss_client.rs#L1087)
- **问题机理**：
  当本地客户端意外断开导致向本地 socket 写数据报错时，客户端直接退出连接循环并执行本地清理，但未向服务端发送 `MuxCommand::Rst`。服务端并不知道客户端本地已经断开，依然会继续从远程目标下载数据并通过多路复用信道传输，造成流量和计算资源的持续浪费。
- **修复方案**：
  在非 `remote_fin` 且发生本地写错误时，向服务端显式发送 `MuxCommand::Rst`，及时截断服务端无效的数据传输。

---

## 三、正向优化保证与执行计划

1. **绝对正向优化保证**：
   - 上述 10 项修改均针对明确的并发隐患、资源泄漏、竞态漏洞和抗 DoS 能力进行加固；
   - 修复不仅不增加锁争用，反而通过 RAII 自动复用避免了操作系统频繁的堆分配；
   - 保持所有协议格式、帧头结构与 GoWay v1.8.18 的 100% 互通兼容。
2. **执行计划建议**：
   - **Phase 1**：合入 P0 级严重并发流泄漏与连接雪崩缺陷（第 1、2、3 项）；
   - **Phase 2**：合入 P1 级缓冲池 RAII、SingleFlight 竞态与容量等待机制（第 4、5、6、7 项）；
   - **Phase 3**：合入 P2 级协议防护与 RST 状态对齐优化（第 8、9、10 项）；
   - **Phase 4**：本地代码审查核对与推送远端流水线进行全面验证。
