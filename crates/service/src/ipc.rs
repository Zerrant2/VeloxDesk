//! Local IPC server and client for controlling a running VeloxDesk host daemon/service
//! (`Status`, `RotateOtp`, `DisconnectAll`, `Shutdown`).
//!
//! Uses a loopback TCP socket (`127.0.0.1`, default port [`DEFAULT_IPC_PORT`]) framed as
//! `u32 LE length | UTF-8 TOML envelope` with optional shared-secret bearer token so both
//! Windows and Unix work identically with zero extra dependencies.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::timeout;
use veloxdesk_core_traits::{CoreError, Result};

/// Default loopback TCP port for local VeloxDesk daemon IPC.
pub const DEFAULT_IPC_PORT: u16 = 21119;
const MAX_IPC_FRAME: usize = 64 * 1024;
const IPC_IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Commands sent from a local CLI/UI client to the running host service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IpcRequest {
    Status,
    RotateOtp,
    DisconnectAll,
    Shutdown,
}

/// Responses returned by the host service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcResponse {
    Status {
        device_id: String,
        active_otp: Option<String>,
        active_sessions: usize,
        desktop: String,
    },
    Otp {
        otp: String,
    },
    Ok {
        message: String,
    },
    Error {
        message: String,
    },
}

#[derive(Serialize, Deserialize)]
struct WireReq {
    #[serde(default)]
    token: Option<String>,
    req: IpcRequest,
}

pub type IpcHandler = Arc<dyn Fn(IpcRequest) -> IpcResponse + Send + Sync>;

/// Local loopback IPC server for querying and controlling a running host.
pub struct IpcServer {
    listener: TcpListener,
    local_addr: SocketAddr,
    token: Option<String>,
    handler: IpcHandler,
}

impl IpcServer {
    /// Bind an IPC server on `addr` (must be loopback; e.g. `127.0.0.1:0` or `127.0.0.1:21119`).
    pub async fn bind(addr: SocketAddr, handler: IpcHandler) -> Result<Self> {
        Self::bind_with_token(addr, None, handler).await
    }

    /// Bind an IPC server requiring a shared bearer `token` on every request.
    pub async fn bind_with_token(
        addr: SocketAddr,
        token: Option<String>,
        handler: IpcHandler,
    ) -> Result<Self> {
        if !addr.ip().is_loopback() {
            return Err(CoreError::invalid("IPC server must bind to a loopback address"));
        }
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        Ok(Self { listener, local_addr, token, handler })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Serve IPC requests until `shutdown` becomes `true` (or its sender drops).
    pub async fn run_until(self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        loop {
            if *shutdown.borrow() {
                break;
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                accepted = self.listener.accept() => {
                    match accepted {
                        Ok((stream, _peer)) => {
                            let handler = self.handler.clone();
                            let token = self.token.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_conn(stream, token.as_deref(), handler).await {
                                    tracing::debug!("ipc client error: {e}");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!("ipc accept error: {e}");
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

async fn handle_conn(mut stream: TcpStream, expected_token: Option<&str>, handler: IpcHandler) -> Result<()> {
    let raw = read_frame(&mut stream).await?;
    let text = std::str::from_utf8(&raw).map_err(|_| CoreError::invalid("ipc request is not UTF-8"))?;
    let wire: WireReq = toml::from_str(text).map_err(|e| CoreError::invalid(format!("ipc parse: {e}")))?;
    let resp = if let Some(exp) = expected_token
        && wire.token.as_deref() != Some(exp)
    {
        IpcResponse::Error { message: "unauthorized IPC token".into() }
    } else {
        (handler)(wire.req)
    };
    let encoded = toml::to_string(&resp).map_err(|e| CoreError::platform(format!("ipc encode: {e}")))?;
    write_frame(&mut stream, encoded.as_bytes()).await
}

/// Client for sending one-shot commands to a local [`IpcServer`].
pub struct IpcClient;

impl IpcClient {
    pub async fn request(addr: SocketAddr, req: &IpcRequest) -> Result<IpcResponse> {
        Self::request_with_token(addr, None, req).await
    }

    pub async fn request_with_token(
        addr: SocketAddr,
        token: Option<&str>,
        req: &IpcRequest,
    ) -> Result<IpcResponse> {
        let mut stream = timeout(IPC_IO_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| CoreError::Timeout)??;
        let wire = WireReq { token: token.map(str::to_string), req: req.clone() };
        let encoded = toml::to_string(&wire).map_err(|e| CoreError::platform(format!("ipc encode: {e}")))?;
        write_frame(&mut stream, encoded.as_bytes()).await?;
        let raw = read_frame(&mut stream).await?;
        let text = std::str::from_utf8(&raw).map_err(|_| CoreError::invalid("ipc response is not UTF-8"))?;
        toml::from_str(text).map_err(|e| CoreError::invalid(format!("ipc response parse: {e}")))
    }
}

async fn write_frame(stream: &mut TcpStream, body: &[u8]) -> Result<()> {
    if body.len() > MAX_IPC_FRAME {
        return Err(CoreError::invalid("ipc frame too large"));
    }
    let len = (body.len() as u32).to_le_bytes();
    timeout(IPC_IO_TIMEOUT, async {
        stream.write_all(&len).await?;
        stream.write_all(body).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| CoreError::Timeout)??;
    Ok(())
}

async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>> {
    timeout(IPC_IO_TIMEOUT, async {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf) as usize;
        if len > MAX_IPC_FRAME {
            return Err(CoreError::invalid("ipc frame exceeds limit"));
        }
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await?;
        Ok(buf)
    })
    .await
    .map_err(|_| CoreError::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{HostAuthConfig, HostAuthState};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ipc_status_rotate_otp_and_token_check() {
        let auth_state = HostAuthState::from_config(&HostAuthConfig::with_otp("INIT1111"));
        let auth_c = auth_state.clone();
        let (stop_tx, stop_rx) = watch::channel(false);
        let stop_c = stop_tx.clone();

        let srv = IpcServer::bind_with_token(
            "127.0.0.1:0".parse().unwrap(),
            Some("tok-secret".into()),
            Arc::new(move |req| match req {
                IpcRequest::Status => IpcResponse::Status {
                    device_id: "123456789".into(),
                    active_otp: auth_c.current_otp(),
                    active_sessions: 1,
                    desktop: "Interactive(Default)".into(),
                },
                IpcRequest::RotateOtp => IpcResponse::Otp { otp: auth_c.rotate_otp() },
                IpcRequest::DisconnectAll => IpcResponse::Ok { message: "disconnected".into() },
                IpcRequest::Shutdown => {
                    let _ = stop_c.send(true);
                    IpcResponse::Ok { message: "shutting down".into() }
                }
            }),
        )
        .await
        .unwrap();
        let addr = srv.local_addr();
        let task = tokio::spawn(srv.run_until(stop_rx));

        // Bad token rejected.
        let bad = IpcClient::request_with_token(addr, Some("wrong"), &IpcRequest::Status).await.unwrap();
        assert!(matches!(bad, IpcResponse::Error { .. }));

        // Valid Status.
        let st = IpcClient::request_with_token(addr, Some("tok-secret"), &IpcRequest::Status).await.unwrap();
        assert_eq!(
            st,
            IpcResponse::Status {
                device_id: "123456789".into(),
                active_otp: Some("INIT1111".into()),
                active_sessions: 1,
                desktop: "Interactive(Default)".into(),
            }
        );

        // RotateOtp changes OTP.
        let rot = IpcClient::request_with_token(addr, Some("tok-secret"), &IpcRequest::RotateOtp).await.unwrap();
        let IpcResponse::Otp { otp } = rot else { panic!("expected Otp") };
        assert_ne!(otp, "INIT1111");
        assert_eq!(auth_state.current_otp().as_deref(), Some(otp.as_str()));

        // Shutdown cleanly terminates server.
        let bye = IpcClient::request_with_token(addr, Some("tok-secret"), &IpcRequest::Shutdown).await.unwrap();
        assert!(matches!(bye, IpcResponse::Ok { .. }));
        tokio::time::timeout(Duration::from_secs(3), task).await.unwrap().unwrap().unwrap();
    }
}
