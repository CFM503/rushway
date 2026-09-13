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

#[derive(Parser, Debug)]
#[command(about = "Identical end-to-end benchmark runner for RushWay and GoWay")]
struct Args {
    /// Implementation name printed in the benchmark result.
    #[arg(long)]
    implementation: String,

    /// Proxy executable path. The process is used for both server and client.
    #[arg(long)]
    bin: PathBuf,
}

async fn free_port() -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    Ok(listener.local_addr()?.port())
}

fn listen_arg(implementation: &str, port: u16) -> String {
    if implementation.eq_ignore_ascii_case("goway") {
        format(":{port}")
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
    cmd.arg("-p").arg(listen_arg(implementation, port));
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

async fn socks5_connect(proxy_port: u16, target_port: u16) -> io::Result<TcpStream> {
    let mut stream = TcpStream::connect(("127.0.0.1", proxy_port)).await?;
    stream.write_all(&[5, 1, 0]).await?;

    let mut method = [0u8; 2];
    stream.read_exact(&mut method).await?;
    if method != [5, 0] {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SOCKS5 no-auth was rejected",
        ));
    }

    let port = target_port.to_be_bytes();
    stream
        .write_all(&[5, 1, 0, 1, 127, 0, 0, 1, port[0], port[1]])
        .await?;
    let mut reply = [0u8; 10];
    stream.read_exact(&mut reply).await?;
    if reply[1] != 0 {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("SOCKS5 connect failed: {}", reply[1]),
        ));
    }
    Ok(stream)
}

async fn one_flow(proxy_port: u16, target_port: u16, payload: Arc<[u8]>) -> io::Result<usize> {
    let mut stream = socks5_connect(proxy_port, target_port).await?;
    stream.write_all(&payload).await?;
    let mut echoed = vec![0u8; payload.len()];
    stream.read_exact(&mut echoed).await?;
    if echoed.as_slice() != payload.as_ref() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "echo payload mismatch",
        ));
    }
    Ok(payload.len())
}

async fn run_case(
    proxy_port: u16,
    target_port: u16,
    concurrency: usize,
    payload: Arc<[u8]>,
) -> io::Result<f64> {
    // Deliberately identical timing definition to the RushWay formal e2e benchmark:
    // connection establishment + SOCKS5 + upstream handshake + data transfer + echo.
    let start = Instant::now();
    let mut tasks = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        tasks.push(tokio::spawn(one_flow(
            proxy_port,
            target_port,
            Arc::clone(&payload),
        )));
    }

    let mut total = 0usize;
    for task in tasks {
        total += task
            .await
            .map_err(|e| io::Error::other(e.to_string()))??;
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
    if !args.bin.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("proxy binary not found: {}", args.bin.display()),
        ));
    }

    let server_port = free_port().await?;
    let client_port = free_port().await?;
    let (target_port, echo_task) = start_echo().await?;

    let mut server = spawn_proxy(&args.bin, server_port, &args.implementation, None)?;
    let mut client = spawn_proxy(
        &args.bin,
        client_port,
        &args.implementation,
        Some(format!("ws://127.0.0.1:{server_port}/")),
    )?;

    let result = async {
        wait_for_port(server_port).await?;
        wait_for_port(client_port).await?;

        let mut payload = vec![0u8; PAYLOAD_SIZE];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = ((i * 31 + 17) & 0xff) as u8;
        }
        let payload: Arc<[u8]> = payload.into();

        let mut results = Vec::with_capacity(CONCURRENCIES.len());
        for &concurrency in CONCURRENCIES {
            let throughput =
                run_case(client_port, target_port, concurrency, Arc::clone(&payload)).await?;
            results.push((concurrency, throughput));
        }

        print!(
            "proxy_e2e implementation={} payload_mib={} roundtrip_echo=1",
            args.implementation,
            payload.len() / (1024 * 1024)
        );
        for (concurrency, throughput) in results {
            print!(" c{}_mib_s={:.2}", concurrency, throughput);
        }
        println!();
        Ok::<(), io::Error>(())
    }
    .await;

    kill_child(&mut client);
    kill_child(&mut server);
    echo_task.abort();
    result
}
