use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::sleep;

const PAYLOAD_SIZE: usize = 4 * 1024 * 1024;
const CONCURRENCIES: &[usize] = &[1, 8, 32];
const TEST_KEY: &str = "rushway-proxy-bench-test-key";

#[derive(Parser, Debug)]
#[command(about = "Identical end-to-end benchmark runner for RushWay and GoWay")]
struct Args {
    /// Implementation name printed in the benchmark result.
    #[arg(long)]
    implementation: String,

    /// Proxy executable path. The process is used for both server and client.
    /// Omit with --external: the orchestrator already started the proxy chain.
    #[arg(long)]
    bin: Option<PathBuf>,

    /// Warm one flow before timing to exclude initial upstream connection setup.
    #[arg(long)]
    steady_state: bool,

    /// Do not spawn proxies; send load to an already-running SOCKS5 client port.
    /// Echo target stays internal to this process.
    #[arg(long, default_value_t = false)]
    external: bool,

    /// SOCKS5 client port of the externally started chain (requires --external).
    #[arg(long)]
    client_port: Option<u16>,

    /// IPv4 address sent in the SOCKS5 CONNECT request (must be reachable from
    /// the proxy server process; use a non-loopback address for cross-namespace runs).
    #[arg(long, default_value = "127.0.0.1")]
    target_ip: String,

    /// Bind address for the local echo server (e.g. 0.0.0.0 to accept
    /// connections coming from another network namespace).
    #[arg(long, default_value = "127.0.0.1")]
    echo_bind: String,
}

fn parse_ipv4(s: &str) -> io::Result<[u8; 4]> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("bad ipv4: {s}")));
    }
    let mut out = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p
            .parse::<u8>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("bad ipv4: {s}")))?;
    }
    Ok(out)
}

async fn free_port() -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    Ok(listener.local_addr()?.port())
}

fn listen_arg(implementation: &str, port: u16) -> String {
    if implementation.eq_ignore_ascii_case("goway") {
        format!(":{port}")
    } else {
        port.to_string()
    }
}

fn spawn_proxy(
    path: &Path,
    port: u16,
    implementation: &str,
    upstream: Option<String>,
) -> io::Result<Child> {
    let mut cmd = Command::new(path);
    cmd.arg("-p")
        .arg(listen_arg(implementation, port))
        .arg("-k")
        .arg(TEST_KEY)
        .arg("--no-block-local")
        .arg("--log")
        .arg("ERROR");
    if let Some(upstream) = upstream {
        cmd.arg("--up").arg(upstream);
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn()
}

async fn wait_for_port(port: u16) -> io::Result<()> {
    for _ in 0..120 {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(_) => return Ok(()),
            Err(_) => sleep(Duration::from_millis(25)).await,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("port {port} did not open"),
    ))
}

async fn start_echo(bind: &str) -> io::Result<(u16, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind((bind, 0)).await?;
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

fn stage<T>(r: io::Result<T>, stage: &str) -> io::Result<T> {
    r.map_err(|e| io::Error::new(e.kind(), format!("{stage}: {e}")))
}

async fn socks5_connect(
    proxy_port: u16,
    target_ip: [u8; 4],
    target_port: u16,
) -> io::Result<TcpStream> {
    let mut stream = stage(
        TcpStream::connect(("127.0.0.1", proxy_port)).await,
        "socks_tcp_connect",
    )?;
    stage(stream.write_all(&[5, 1, 0]).await, "socks_greeting_write")?;

    let mut method = [0u8; 2];
    stage(stream.read_exact(&mut method).await, "socks_greeting_reply")?;
    if method != [5, 0] {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SOCKS5 no-auth was rejected",
        ));
    }

    let port = target_port.to_be_bytes();
    stage(
        stream
            .write_all(&[5, 1, 0, 1, target_ip[0], target_ip[1], target_ip[2], target_ip[3], port[0], port[1]])
            .await,
        "socks_connect_write",
    )?;
    let mut reply = [0u8; 10];
    stage(stream.read_exact(&mut reply).await, "socks_connect_reply")?;
    if reply[1] != 0 {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("SOCKS5 connect failed: {}", reply[1]),
        ));
    }
    Ok(stream)
}

async fn one_flow(
    flow_id: usize,
    proxy_port: u16,
    target_ip: [u8; 4],
    target_port: u16,
    payload: Arc<[u8]>,
) -> io::Result<usize> {
    let mut stream = socks5_connect(proxy_port, target_ip, target_port).await?;
    stage(stream.write_all(&payload).await, "payload_write")?;
    let mut echoed = vec![0u8; payload.len()];
    let mut got = 0usize;
    while got < echoed.len() {
        match stream.read(&mut echoed[got..]).await {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("payload_echo: early eof after {got}/{} bytes", echoed.len()),
                ))
            }
            Ok(n) => got += n,
            Err(e) => return Err(io::Error::new(e.kind(), format!("payload_echo_read: {e}"))),
        }
    }
    if echoed.as_slice() != payload.as_ref() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("echo payload mismatch at flow {flow_id}"),
        ));
    }
    let _ = flow_id;
    Ok(payload.len())
}

async fn run_case(
    proxy_port: u16,
    target_ip: [u8; 4],
    target_port: u16,
    concurrency: usize,
    payload: Arc<[u8]>,
) -> io::Result<f64> {
    let start = Instant::now();
    let mut tasks = Vec::with_capacity(concurrency);
    for flow_id in 0..concurrency {
        tasks.push(tokio::spawn(one_flow(
            flow_id,
            proxy_port,
            target_ip,
            target_port,
            Arc::clone(&payload),
        )));
    }

    let mut total = 0usize;
    for task in tasks {
        let n = task
            .await
            .map_err(|e| io::Error::other(e.to_string()))?
            .map_err(|e| io::Error::new(e.kind(), format!("case_c{concurrency}: {e}")))?;
        total += n;
    }

    let secs = start.elapsed().as_secs_f64();
    let mib = total as f64 / (1024.0 * 1024.0);
    Ok(mib / secs)
}

fn kill_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> io::Result<()> {
    let args = Args::parse();

    let mut server: Option<Child> = None;
    let mut client: Option<Child> = None;
    let (client_port, server_port) = if args.external {
        let port = args.client_port.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "--external requires --client-port",
            )
        })?;
        (port, 0u16)
    } else {
        let bin = args.bin.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "--bin is required without --external")
        })?;
        if !bin.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("proxy binary not found: {}", bin.display()),
            ));
        }
        let server_port = free_port().await?;
        let client_port = free_port().await?;
        server = Some(spawn_proxy(bin, server_port, &args.implementation, None)?);
        client = Some(spawn_proxy(
            bin,
            client_port,
            &args.implementation,
            Some(format!("ws://127.0.0.1:{server_port}/")),
        )?);
        (client_port, server_port)
    };

    let (target_port, echo_task) = start_echo(&args.echo_bind).await?;
    let target_ip = parse_ipv4(&args.target_ip)?;

    let result = async {
        if server.is_some() {
            wait_for_port(server_port).await?;
        }
        wait_for_port(client_port).await?;

        let mut payload = vec![0u8; PAYLOAD_SIZE];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = ((i * 31 + 17) & 0xff) as u8;
        }
        let payload: Arc<[u8]> = payload.into();

        if args.steady_state {
            // Prime the physical upstream connection/session once. The timed cases
            // then measure new SOCKS/MUX streams plus data transfer, without the
            // initial WebSocket handshake dominating the result.
            let _ = one_flow(0, client_port, target_ip, target_port, Arc::clone(&payload)).await?;
        }

        let mut results = Vec::with_capacity(CONCURRENCIES.len());
        for &concurrency in CONCURRENCIES {
            let throughput = run_case(
                client_port,
                target_ip,
                target_port,
                concurrency,
                Arc::clone(&payload),
            )
            .await?;
            results.push((concurrency, throughput));
        }

        let mode = if args.steady_state {
            "steady_state"
        } else {
            "setup_inclusive"
        };
        print!(
            "proxy_e2e implementation={} mode={} payload_mib={} roundtrip_echo=1",
            args.implementation,
            mode,
            payload.len() / (1024 * 1024)
        );
        for (concurrency, throughput) in results {
            print!(" c{}_mib_s={:.2}", concurrency, throughput);
        }
        println!();
        Ok::<(), io::Error>(())
    }
    .await;

    if let Some(mut client) = client {
        kill_child(&mut client);
    }
    if let Some(mut server) = server {
        kill_child(&mut server);
    }
    echo_task.abort();
    result
}
