use crate::state::RuntimeState;
use anyhow::Context;
use std::net::SocketAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tracing::{debug, info, warn};

pub async fn run_health_server(addr: SocketAddr, state: RuntimeState) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind health endpoint {addr}"))?;

    info!(%addr, "health endpoint listening");

    loop {
        let (stream, peer) = listener.accept().await?;
        let state = state.clone();

        tokio::spawn(async move {
            if let Err(error) = handle_health_connection(stream, state).await {
                warn!(%peer, ?error, "health request failed");
            } else {
                debug!(%peer, "health request handled");
            }
        });
    }
}

async fn handle_health_connection(
    mut stream: TcpStream,
    state: RuntimeState,
) -> anyhow::Result<()> {
    let mut buf = [0_u8; 1024];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    let (status, body) = if request.starts_with("GET /health ") {
        let body = serde_json::to_string_pretty(&state.health_snapshot())?;
        ("200 OK", body)
    } else {
        ("404 Not Found", "{\"error\":\"not_found\"}".to_string())
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );

    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}
