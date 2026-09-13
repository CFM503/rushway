//! WSS client path for GoWay-compatible upstreams.
//!
//! This module keeps the existing plain `ws://` runtime untouched. It terminates
//! TLS before feeding the resulting stream into the same WebSocket and MUX
//! primitives. Server-side TLS is not claimed here.

use crate::crypto::XorCipher;
use crate::protocol::{write_frame_parts, MuxCommand, MuxFrame, SynPayload};
use crate::proxy::{parse_http_connect, parse_socks5_request, socks5_success_response, SocksCommand, TargetAddr, SOCKS5_CONNECT, SOCKS5_VERSION};
use crate::runtime::RuntimeConfig;
use crate::tls;
use crate::ws::{build_client_handshake_request, read_frame, read_frame_owned, read_http_headers, validate_client_handshake_response, write_frame};
use anyhow::{anyhow, bail, Context, Result};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}
type BoxTransport = Box<dyn Transport>;
type BoxReader = tokio::io::ReadHalf<BoxTransport>;
type BoxWriter = tokio::io::WriteHalf<BoxTransport>;
