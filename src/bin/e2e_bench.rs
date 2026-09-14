use std::io;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};

const PAYLOAD_SIZE: usize = 4 * 1024 * 1024;
const CONCURRENCIES: &[usize] = &[1, 8, 32];
const TEST_KEY: &str = "rushway-e2e-test-key";
const FLOW_TIMEOUT: Duration = Duration::from_secs(30);
const E2E_TIMEOUT: Duration = Duration::from_secs(120);

async fn free_port() -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    Ok(listener.local_addr()?.port())
}

fn child_path() -> io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let path = exe
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
        _ => "ws",
    }
}

fn upstream_url(server_port: u16) -> String {
    match transport_mode() {
        "quic" => format!("quic://127.0.0.1:{server_port}"),
        _ => format!("ws://127.0.0.1:{server_port}/"),
    }
}

fn spawn_rushway(
    path: &std::path::Path,
    port: u16,
    upstream: Option<String>,
    key: &str,
    allow_local_targets: bool,
) -> io::Result<Child> {
    let mut cmd = Command::new(path);
    cmd.arg("-p")
        .arg(port.to_string())
        .arg("-k")
        .arg(key)
        .arg("--log")
        .arg("ERROR");
    if allow_local_targets {
        cmd.arg("--no-block-local");
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
                        "{role} RushWay child exited before opening port {port}: {status}"
                    )));
                }
                sleep(Duration::from_millis(25)).await;
            }
        }
    }
    let _ = child.try_wait()?;
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
    timeout(FLOW_TIMEOUT, async {
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
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "e2e flow timed out after 30s"))?
}

async fn run_case(
    proxy_port: u16,
    target_port: u16,
    concurrency: usize,
    payload: Arc<[u8]>,
) -> io::Result<f64> {
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
        total += task.await.map_err(|e| io::Error::other(e.to_string()))??;
    }
    let secs = start.elapsed().as_secs_f64();
    let mib = total as f64 / (1024.0 * 1024.0);
    Ok(mib / secs)
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
    let server_port = free_port().await?;
    let client_port = free_port().await?;
    let (target_port, echo_task) = start_echo().await?;
    let mode = transport_mode();

    let mut server = spawn_rushway(&rushway, server_port, None, TEST_KEY, true)?;
    let mut client = spawn_rushway(
        &rushway,
        client_port,
        Some(upstream_url(server_port)),
        TEST_KEY,
        true,
    )?;

    let result = match timeout(E2E_TIMEOUT, async {
        wait_for_port(server_port, &mut server, "server").await?;
        wait_for_port(client_port, &mut client, "client").await?;
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
            "rushway_e2e transport={} payload_mib={} roundtrip_echo=1",
            mode,
            payload.len() / (1024 * 1024)
        );
        for (concurrency, throughput) in results {
            print!(" c{}_mib_s={:.2}", concurrency, throughput);
        }
        println!();
        Ok::<(), io::Error>(())
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "e2e benchmark exceeded 120s overall timeout",
        )),
    };

    if result.is_err() {
        kill_child(&mut client);
        kill_child(&mut server);
        let server_log = child_stderr(&mut server);
        let client_log = child_stderr(&mut client);
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
    result
}