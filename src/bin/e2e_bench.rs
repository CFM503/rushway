use std::io;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::sleep;

const PAYLOAD_SIZE: usize = 4 * 1024 * 1024;
const CONCURRENCY: usize = 8;

async fn free_port() -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    Ok(listener.local_addr()?.port())
}

fn child_path() -> io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let path = exe
        .parent()
        .map(|p| p.join(if cfg!(windows) { "rushway.exe" } else { "rushway" }))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot locate sibling rushway binary"))?;
    if !path.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("missing {}", path.display())));
    }
    Ok(path)
}

fn spawn_rushway(path: &std::path::Path, port: u16, upstream: Option<String>) -> io::Result<Child> {
    let mut cmd = Command::new(path);
    cmd.arg("-p").arg(port.to_string());
    if let Some(upstream) = upstream {
        cmd.arg("--up").arg(upstream);
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn()
}

async fn wait_for_port(port: u16) -> io::Result<()> {
    for _ in 0..100 {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(_) => return Ok(()),
            Err(_) => sleep(Duration::from_millis(25)).await,
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, format!("port {} did not open", port)))
}

async fn start_echo() -> io::Result<(u16, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { break };
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
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "SOCKS5 no-auth was rejected"));
    }

    let port = target_port.to_be_bytes();
    stream
        .write_all(&[5, 1, 0, 1, 127, 0, 0, 1, port[0], port[1]])
        .await?;
    let mut reply = [0u8; 10];
    stream.read_exact(&mut reply).await?;
    if reply[1] != 0 {
        return Err(io::Error::new(io::ErrorKind::ConnectionRefused, format!("SOCKS5 connect failed: {}", reply[1])));
    }
    Ok(stream)
}

async fn one_flow(proxy_port: u16, target_port: u16, payload: Vec<u8>) -> io::Result<usize> {
    let mut stream = socks5_connect(proxy_port, target_port).await?;
    stream.write_all(&payload).await?;
    let mut echoed = vec![0u8; payload.len()];
    stream.read_exact(&mut echoed).await?;
    if echoed != payload {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "echo payload mismatch"));
    }
    Ok(payload.len())
}

async fn run_case(proxy_port: u16, target_port: u16, concurrency: usize, payload: &[u8]) -> io::Result<f64> {
    let start = Instant::now();
    let mut tasks = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        tasks.push(tokio::spawn(one_flow(proxy_port, target_port, payload.to_vec())));
    }
    let mut total = 0usize;
    for task in tasks {
        total += task.await.map_err(|e| io::Error::other(e.to_string()))??;
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
    let rushway = child_path()?;
    let server_port = free_port().await?;
    let client_port = free_port().await?;
    let (target_port, echo_task) = start_echo().await?;

    let mut server = spawn_rushway(&rushway, server_port, None)?;
    let mut client = spawn_rushway(&rushway, client_port, Some(format!("ws://127.0.0.1:{}/", server_port)))?;

    let result = async {
        wait_for_port(server_port).await?;
        wait_for_port(client_port).await?;
        let mut payload = vec![0u8; PAYLOAD_SIZE];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = ((i * 31 + 17) & 0xff) as u8;
        }

        let single = run_case(client_port, target_port, 1, &payload).await?;
        let concurrent = run_case(client_port, target_port, CONCURRENCY, &payload).await?;
        println!(
            "rushway_e2e payload_mib={} single_mib_s={:.2} concurrency={} concurrent_total_mib_s={:.2}",
            payload.len() / (1024 * 1024),
            single,
            CONCURRENCY,
            concurrent
        );
        Ok::<(), io::Error>(())
    }
    .await;

    kill_child(&mut client);
    kill_child(&mut server);
    echo_task.abort();
    result
}
