use std::io;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};

const PAYLOAD_SIZE: usize = 4 * 1024 * 1024;
const CONCURRENCIES: &[usize] = &[1, 8, 32];
const STRESS_PAYLOAD_SIZE: usize = 64 * 1024;
const STRESS_CONCURRENCIES: &[usize] = &[1, 100, 500];
const STRESS_DIAGNOSTIC_CONCURRENCY: usize = 1000;
const TEST_KEY: &str = "rushway-e2e-test-key";
const FLOW_TIMEOUT: Duration = Duration::from_secs(30);
const E2E_TIMEOUT: Duration = Duration::from_secs(120);
const STRESS_TIMEOUT: Duration = Duration::from_secs(180);

fn diagnostic_mode() -> bool {
    matches!(
        std::env::var("RUSHWAY_E2E_DIAGNOSTIC").ok().as_deref(),
        Some("1" | "true" | "yes")
    )
}

fn include_1000_stress() -> bool {
    matches!(
        std::env::var("RUSHWAY_E2E_INCLUDE_1000").ok().as_deref(),
        Some("1" | "true" | "yes")
    )
}

fn configured_mux_sessions() -> Option<usize> {
    std::env::var("RUSHWAY_MUX_SESSIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| (1..=64).contains(v))
}

#[allow(dead_code)]
fn free_port() -> io::Result<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()
        .map(|a| a.port())
}

async fn free_port_async() -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    Ok(listener.local_addr()?.port())
}

fn child_path() -> io::Result<std::path::PathBuf> {
    let path = std::env::current_exe()?
        .parent()
        .map(|p| {
            p.join(if cfg!(windows) {
                "rushway.exe"
            } else {
                "rushway"
            })
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "cannot locate sibling rushway binary",
            )
        })?;
    if !path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("missing {}", path.display()),
        ));
    }
    Ok(path)
}

fn transport_mode() -> &'static str {
    match std::env::var("RUSHWAY_E2E_TRANSPORT").ok().as_deref() {
        Some("quic") => "quic",
        Some("wss") => "wss",
        _ => "ws",
    }
}
fn stress_mode() -> bool {
    matches!(
        std::env::var("RUSHWAY_E2E_STRESS").ok().as_deref(),
        Some("1" | "true" | "yes")
    )
}
fn stress_pattern() -> &'static str {
    match std::env::var("RUSHWAY_E2E_STRESS_PATTERN").ok().as_deref() {
        Some("staged") => "staged",
        _ => "burst",
    }
}
fn upstream_url(port: u16) -> String {
    match transport_mode() {
        "quic" => format!("quic://127.0.0.1:{port}"),
        "wss" => format!("wss://127.0.0.1:{port}/"),
        _ => format!("ws://127.0.0.1:{port}/"),
    }
}

fn spawn_rushway(
    path: &std::path::Path,
    port: u16,
    upstream: Option<String>,
    key: &str,
    allow_local_targets: bool,
    wss_server: bool,
) -> io::Result<Child> {
    let mut cmd = Command::new(path);
    let log_level = if diagnostic_mode() { "DEBUG" } else { "ERROR" };
    cmd.arg("-p")
        .arg(port.to_string())
        .arg("-k")
        .arg(key)
        .arg("--log")
        .arg(log_level);
    if let Some(sessions) = configured_mux_sessions() {
        cmd.arg("--mux-sessions").arg(sessions.to_string());
    }
    if allow_local_targets {
        cmd.arg("--no-block-local");
    }
    if wss_server {
        cmd.arg("--wss-server");
    }
    if let Some(upstream) = upstream {
        cmd.arg("--up").arg(upstream);
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::piped()).spawn()
}

async fn wait_for_port(port: u16, child: &mut Child, role: &str) -> io::Result<()> {
    for _ in 0..100 {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(_) => return Ok(()),
            Err(_) => {
                if let Some(status) = child.try_wait()? {
                    return Err(io::Error::other(format!(
                        "{role} RushWay exited before opening port {port}: {status}"
                    )));
                }
                sleep(Duration::from_millis(25)).await;
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("{role} RushWay port {port} did not open"),
    ))
}

async fn start_echo() -> io::Result<(u16, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if socket.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    Ok((port, task))
}

async fn socks5_stage<T>(stage: &'static str, future: T) -> io::Result<T::Output>
where
    T: std::future::Future,
{
    timeout(Duration::from_secs(10), future).await.map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("SOCKS5 stage {stage} timed out after 10s"),
        )
    })
}

async fn socks5_connect(proxy_port: u16, target_port: u16) -> io::Result<TcpStream> {
    let mut stream =
        socks5_stage("tcp_connect", TcpStream::connect(("127.0.0.1", proxy_port))).await??;
    socks5_stage("greeting_write", stream.write_all(&[5, 1, 0])).await??;
    let mut method = [0u8; 2];
    socks5_stage("method_read", stream.read_exact(&mut method)).await??;
    if method != [5, 0] {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SOCKS5 no-auth was rejected",
        ));
    }
    let port = target_port.to_be_bytes();
    socks5_stage(
        "connect_write",
        stream.write_all(&[5, 1, 0, 1, 127, 0, 0, 1, port[0], port[1]]),
    )
    .await??;
    let mut reply = [0u8; 10];
    socks5_stage("connect_reply_read", stream.read_exact(&mut reply)).await??;
    if reply[1] != 0 {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("SOCKS5 connect failed: {}", reply[1]),
        ));
    }
    Ok(stream)
}

async fn one_flow(
    index: usize,
    proxy_port: u16,
    target_port: u16,
    payload: Arc<[u8]>,
) -> io::Result<usize> {
    let result = timeout(FLOW_TIMEOUT, async {
        let mut stream = socks5_connect(proxy_port, target_port)
            .await
            .map_err(|e| io::Error::other(format!("flow {index} connect: {e}")))?;
        stream
            .write_all(&payload)
            .await
            .map_err(|e| io::Error::other(format!("flow {index} write: {e}")))?;
        let mut echoed = vec![0u8; payload.len()];
        stream
            .read_exact(&mut echoed)
            .await
            .map_err(|e| io::Error::other(format!("flow {index} read echo: {e}")))?;
        if echoed.as_slice() != payload.as_ref() {
            return Err(io::Error::other(format!(
                "flow {index} echo payload mismatch"
            )));
        }
        Ok(payload.len())
    })
    .await;
    match result {
        Ok(value) => value,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("flow {index} timed out after 30s"),
        )),
    }
}

async fn run_case(
    proxy_port: u16,
    target_port: u16,
    concurrency: usize,
    payload: Arc<[u8]>,
) -> io::Result<f64> {
    let start = Instant::now();
    let tasks = (0..concurrency)
        .map(|i| tokio::spawn(one_flow(i, proxy_port, target_port, Arc::clone(&payload))))
        .collect::<Vec<_>>();
    let mut total = 0usize;
    for task in tasks {
        total += task.await.map_err(|e| io::Error::other(e.to_string()))??;
    }
    Ok((total as f64 / (1024.0 * 1024.0)) / start.elapsed().as_secs_f64())
}

async fn run_stress_case(
    proxy_port: u16,
    target_port: u16,
    concurrency: usize,
    payload: Arc<[u8]>,
) -> io::Result<Duration> {
    let start = Instant::now();
    let pattern = stress_pattern();
    let mut tasks = Vec::with_capacity(concurrency);
    if pattern == "staged" {
        for batch_start in (0..concurrency).step_by(100) {
            let batch_end = (batch_start + 100).min(concurrency);
            eprintln!(
                "stress launch batch start={batch_start} end={batch_end} total={concurrency}"
            );
            for index in batch_start..batch_end {
                tasks.push(tokio::spawn(one_flow(
                    index,
                    proxy_port,
                    target_port,
                    Arc::clone(&payload),
                )));
            }
            sleep(Duration::from_millis(50)).await;
        }
    } else {
        for index in 0..concurrency {
            tasks.push(tokio::spawn(one_flow(
                index,
                proxy_port,
                target_port,
                Arc::clone(&payload),
            )));
        }
    }
    for (index, task) in tasks.into_iter().enumerate() {
        task.await
            .map_err(|e| io::Error::other(format!("stress task {index} join: {e}")))??;
    }
    Ok(start.elapsed())
}

fn process_fd_count(pid: u32) -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir(format!("/proc/{pid}/fd"))
            .ok()
            .map(|it| it.count())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}
fn child_stderr(child: &mut Child) -> String {
    child
        .stderr
        .take()
        .map(|mut stderr| {
            use std::io::Read;
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        })
        .unwrap_or_default()
}
fn kill_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> io::Result<()> {
    let rushway = child_path()?;
    let server_port = free_port_async().await?;
    let client_port = free_port_async().await?;
    let (target_port, echo_task) = start_echo().await?;
    let mode = transport_mode();
    let stress = stress_mode();
    let pattern = stress_pattern();
    let mut server = spawn_rushway(&rushway, server_port, None, TEST_KEY, true, mode == "wss")?;
    let mut client = spawn_rushway(
        &rushway,
        client_port,
        Some(upstream_url(server_port)),
        TEST_KEY,
        true,
        false,
    )?;
    let server_pid = server.id();
    let client_pid = client.id();
    let timeout_limit = if stress { STRESS_TIMEOUT } else { E2E_TIMEOUT };
    let result = match timeout(timeout_limit, async {
        wait_for_port(server_port, &mut server, "server").await?;
        wait_for_port(client_port, &mut client, "client").await?;
        if stress {
            let mut payload = vec![0u8; STRESS_PAYLOAD_SIZE];
            for (i, b) in payload.iter_mut().enumerate() { *b = ((i * 31 + 17) & 0xff) as u8; }
            let payload: Arc<[u8]> = payload.into();
            for &concurrency in STRESS_CONCURRENCIES {
                let elapsed = run_stress_case(client_port, target_port, concurrency, Arc::clone(&payload)).await?;
                println!("rushway_stream_stress transport={} pattern={} payload_kib={} streams={} completed={} elapsed_ms={}", mode, pattern, payload.len() / 1024, concurrency, concurrency, elapsed.as_millis());
            }
            if include_1000_stress() {
                let concurrency = STRESS_DIAGNOSTIC_CONCURRENCY;
                eprintln!("stress diagnostic: running non-blocking {}-stream case", concurrency);
                let elapsed = run_stress_case(client_port, target_port, concurrency, Arc::clone(&payload)).await?;
                println!("rushway_stream_stress_diagnostic transport={} pattern={} payload_kib={} streams={} completed={} elapsed_ms={}", mode, pattern, payload.len() / 1024, concurrency, concurrency, elapsed.as_millis());
            }
        } else {
            let mut payload = vec![0u8; PAYLOAD_SIZE];
            for (i, b) in payload.iter_mut().enumerate() { *b = ((i * 31 + 17) & 0xff) as u8; }
            let payload: Arc<[u8]> = payload.into();
            let mut results = Vec::with_capacity(CONCURRENCIES.len());
            for &concurrency in CONCURRENCIES { results.push((concurrency, run_case(client_port, target_port, concurrency, Arc::clone(&payload)).await?)); }
            print!("rushway_e2e transport={} payload_mib={} roundtrip_echo=1", mode, payload.len() / (1024 * 1024));
            for (concurrency, throughput) in results { print!(" c{}_mib_s={:.2}", concurrency, throughput); }
            println!();
        }
        Ok::<(), io::Error>(())
    }).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, format!("e2e benchmark exceeded {}s overall timeout", timeout_limit.as_secs())))
    };
    if result.is_err() {
        eprintln!(
            "stress trap: server_pid={} server_fd={:?} client_pid={} client_fd={:?}",
            server_pid,
            process_fd_count(server_pid),
            client_pid,
            process_fd_count(client_pid)
        );
        kill_child(&mut client);
        kill_child(&mut server);
        let client_log = child_stderr(&mut client);
        let server_log = child_stderr(&mut server);
        if !server_log.is_empty() {
            eprintln!("e2e server stderr:\n{server_log}");
        }
        if !client_log.is_empty() {
            eprintln!("e2e client stderr:\n{client_log}");
        }
        if let Err(ref e) = result {
            eprintln!("e2e benchmark failed: {e}");
        }
    } else {
        kill_child(&mut client);
        kill_child(&mut server);
    }
    echo_task.abort();
    if diagnostic_mode() {
        eprintln!(
            "stress trap: diagnostic_mode=1; server_fd={:?} client_fd={:?}",
            process_fd_count(server_pid),
            process_fd_count(client_pid)
        );
    }
    result
}
