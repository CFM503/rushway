use anyhow::{anyhow, bail, Result};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::cipher::{configured_cipher, transform_payload};
use crate::config::{RuntimeConfig, TargetAddr};
use crate::mux::{MuxCommand, MuxFrame, SynPayload};
use crate::protocol::{parse_socks5_request, parse_socks5_udp_datagram, SocksCommand};
use crate::ws::{build_client_handshake_request, read_frame, read_http_headers, validate_client_handshake_response, write_frame};

async fn handle_local_udp_proxy(mut control: TcpStream, cfg: RuntimeConfig, bind_hint: TargetAddr) -> Result<()> {
    let bind_ip = if bind_hint.host == "0.0.0.0" || bind_hint.host == "" { "0.0.0.0" } else { bind_hint.host.as_str() };
    let udp = Arc::new(UdpSocket::bind(format!("{}:0", bind_ip)).await?);
    let bound = udp.local_addr()?;
    let mut resp = [0u8; 10]; resp[0]=5; resp[1]=0; resp[2]=0; resp[3]=1;
    if let IpAddr::V4(ip) = bound.ip() { resp[4..8].copy_from_slice(&ip.octets()); }
    resp[8..10].copy_from_slice(&bound.port().to_be_bytes());
    control.write_all(&resp).await?;
    let upstream = cfg.upstream.clone().ok_or_else(|| anyhow!("client mode requires upstream"))?;
    let (host, path) = parse_ws_url(&upstream)?;
    let actual_host = cfg.fakehost.clone().unwrap_or_else(|| host.clone());
    let socket = timeout(Duration::from_secs(cfg.connection_timeout.max(1)), TcpStream::connect(&host)).await??;
    let (mut rd, mut wr) = tokio::io::split(socket);
    let origin = Some(format!("https://{}", actual_host));
    let sec_fetch_site = if host.eq_ignore_ascii_case(&actual_host) { "same-origin" } else { "cross-site" };
    let (request, key) = build_client_handshake_request(&actual_host, &path, origin.as_deref(), Some(sec_fetch_site));
    wr.write_all(&request).await?; wr.flush().await?;
    let response = read_http_headers(&mut rd).await?;
    validate_client_handshake_response(&response, &key)?;
    let writer = Arc::new(Mutex::new(wr));
    let cipher = configured_cipher(&cfg.key);
    let mut hello = b"UDP\n".to_vec(); transform_payload(&cipher, &mut hello);
    { let mut w = writer.lock().await; write_frame(&mut *w, &hello, 2, true).await?; }
    let mut frame_buf=Vec::with_capacity(64*1024);
    let Some((opcode,mut ok))=read_frame(&mut rd,Option::<&mut WriteHalf<TcpStream>>::None,&mut frame_buf).await? else { bail!("upstream closed during UDP handshake") };
    if opcode!=2 { bail!("invalid UDP handshake response opcode") }
    transform_payload(&cipher,&mut ok); if ok!=b"OK\n" { bail!("upstream rejected UDP handshake") }
    let latest_client: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    let udp_send=udp.clone(); let writer_send=writer.clone(); let cipher_send=configured_cipher(&cfg.key); let latest_send=latest_client.clone();
    let upload=tokio::spawn(async move {
        let mut buf=vec![0u8;64*1024];
        loop { let (n,peer)=udp_send.recv_from(&mut buf).await?; *latest_send.lock().await=Some(peer); let mut data=buf[..n].to_vec(); transform_payload(&cipher_send,&mut data); let mut w=writer_send.lock().await; write_frame(&mut *w,&data,2,true).await?; }
        Result::<()>::Ok(())
    });
    loop {
        let Some((opcode,mut packet))=read_frame(&mut rd,Option::<&mut WriteHalf<TcpStream>>::None,&mut frame_buf).await? else { break };
        if opcode!=2 { continue; }
        transform_payload(&cipher,&mut packet);
        let (_src,payload)=parse_socks5_udp_datagram(&packet).map_err(|e|anyhow!(e.to_string()))?;
        if let Some(peer)=*latest_client.lock().await { let _=udp.send_to(payload,peer).await; }
    }
    upload.abort(); control.shutdown().await.ok(); Ok(())
}

async fn handle_local_proxy(mut local: TcpStream, cfg: RuntimeConfig, next_id: u32) -> Result<()> {
    let (command, target, is_socks5) = read_proxy_request(&mut local).await?;
    if command == SocksCommand::UdpAssociate { return handle_local_udp_proxy(local, cfg, target).await; }
    let upstream = cfg.upstream.clone().ok_or_else(|| anyhow!("client mode requires upstream"))?;
    let (host, path) = parse_ws_url(&upstream)?;
    let actual_host = cfg.fakehost.clone().unwrap_or_else(|| host.clone());
    let socket = timeout(Duration::from_secs(cfg.connection_timeout.max(1)), TcpStream::connect(&host)).await??;
    let (mut rd, mut wr) = tokio::io::split(socket);
    let origin = Some(format!("https://{}", actual_host));
    let sec_fetch_site = if host.eq_ignore_ascii_case(&actual_host) { "same-origin" } else { "cross-site" };
    let (request, key) = build_client_handshake_request(&actual_host, &path, origin.as_deref(), Some(sec_fetch_site));
    wr.write_all(&request).await?; wr.flush().await?;
    let response = read_http_headers(&mut rd).await?; validate_client_handshake_response(&response, &key)?;
    let writer=Arc::new(Mutex::new(wr)); let cipher=configured_cipher(&cfg.key);
    if is_socks5 { local.write_all(&socks5_success_response()).await?; } else { local.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?; }
    let mut hello=b"MUX\n".to_vec(); transform_payload(&cipher,&mut hello); { let mut w=writer.lock().await; write_frame(&mut *w,&hello,2,true).await?; }
    let mut frame_buf=Vec::with_capacity(64*1024);
    let Some((opcode,mut ok))=read_frame(&mut rd,Option::<&mut WriteHalf<TcpStream>>::None,&mut frame_buf).await? else { bail!("upstream closed during MUX handshake") };
    if opcode!=2 { bail!("invalid MUX handshake response opcode") }
    transform_payload(&cipher,&mut ok); if ok!=b"OK\n" { bail!("upstream rejected MUX handshake") }
    let id=next_id.max(1); let target_text=format!("{}:{}",target.host,target.port); let syn=SynPayload{target:target_text.into_bytes(),initial_data:Vec::new()}; let syn_frame=MuxFrame::new(id,MuxCommand::Syn,syn.encode().map_err(|e|anyhow!(e.to_string()))?).map_err(|e|anyhow!(e.to_string()))?; send_frame(&writer,&syn_frame).await?;
    let (mut local_rd,mut local_wr)=tokio::io::split(local); let writer_up=writer.clone(); let upload=tokio::spawn(async move { let mut buf=vec![0u8;cfg.buffer_size.clamp(16*1024,1024*1024)]; loop { let n=local_rd.read(&mut buf).await?; if n==0 { let _=send_frame(&writer_up,&MuxFrame::new(id,MuxCommand::Fin,Vec::new()).unwrap()).await; break } let mut off=0; while off<n { let end=(off+u16::MAX as usize).min(n); let f=MuxFrame::new(id,MuxCommand::Data,buf[off..end].to_vec()).unwrap(); send_frame(&writer_up,&f).await?; off=end; } } Result::<()>::Ok(()) });
    loop { let Some((opcode,payload))=read_frame(&mut rd,Option::<&mut WriteHalf<TcpStream>>::None,&mut frame_buf).await? else { break }; if opcode!=2 { continue } let frame=MuxFrame::decode(&payload).map_err(|e|anyhow!(e.to_string()))?; if frame.stream_id!=id { continue } match frame.command { MuxCommand::Data=>local_wr.write_all(&frame.payload).await?, MuxCommand::Fin=>{local_wr.shutdown().await?;break}, MuxCommand::Rst=>break, MuxCommand::Syn=>{} } }
    upload.abort(); Ok(())
}

fn parse_ws_url(input: &str) -> Result<(String,String)> {