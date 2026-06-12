use crate::{
    config::NetworkConfig,
    session::{
        build_probe_error, build_probe_response, build_session_data_response, parse_client_message,
        ParsedClientMessage, TransportKind,
    },
    state::RuntimeState,
};
use anyhow::{anyhow, bail, Context};
use std::{io, net::SocketAddr, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    task::JoinHandle,
};
use tracing::{debug, info, warn};

const UDP_PROBE_RESPONSE: &[u8] = b"xaccel-node udp listener ready\n";
const TCP_PROBE_RESPONSE: &[u8] = b"xaccel-node tcp listener ready\n";

pub async fn spawn_network_listeners(state: RuntimeState) -> anyhow::Result<Vec<JoinHandle<()>>> {
    let Some(network) = state.effective_network() else {
        warn!("network config is missing; server listener is disabled");
        return Ok(Vec::new());
    };

    let listen_addr = listen_addr(&network)?;
    let udp_socket = bind_udp_listener(listen_addr).await?;
    let tcp_listener = bind_tcp_listener(listen_addr).await?;

    state.stats().set_udp_listening(true);
    state.stats().set_tcp_listening(true);

    info!(%listen_addr, "UDP listener started");
    info!(%listen_addr, "TCP listener started");

    let udp_state = state.clone();
    let udp_task = tokio::spawn(async move {
        if let Err(error) = run_udp_listener(udp_socket, udp_state).await {
            warn!(?error, "UDP listener stopped");
        }
    });

    let tcp_state = state.clone();
    let tcp_task = tokio::spawn(async move {
        if let Err(error) = run_tcp_listener(tcp_listener, tcp_state).await {
            warn!(?error, "TCP listener stopped");
        }
    });

    Ok(vec![udp_task, tcp_task])
}

async fn bind_udp_listener(listen_addr: SocketAddr) -> anyhow::Result<UdpSocket> {
    UdpSocket::bind(listen_addr)
        .await
        .map_err(|error| listener_bind_error("udp", listen_addr, error))
}

async fn bind_tcp_listener(listen_addr: SocketAddr) -> anyhow::Result<TcpListener> {
    TcpListener::bind(listen_addr)
        .await
        .map_err(|error| listener_bind_error("tcp", listen_addr, error))
}

fn listener_bind_error(protocol: &str, listen_addr: SocketAddr, error: io::Error) -> anyhow::Error {
    let reason = classify_bind_error(&error);
    warn!(
        protocol,
        %listen_addr,
        error_code = reason.code,
        error_kind = ?error.kind(),
        os_error = error.raw_os_error(),
        suggestion = reason.suggestion,
        %error,
        "listener bind failed"
    );
    anyhow!(
        "listener_bind_failed protocol={} listen_addr={} error_code={} error_kind={:?} os_error={:?} message=\"{}\" suggestion=\"{}\"",
        protocol,
        listen_addr,
        reason.code,
        error.kind(),
        error.raw_os_error(),
        error,
        reason.suggestion
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BindErrorReason {
    code: &'static str,
    suggestion: &'static str,
}

fn classify_bind_error(error: &io::Error) -> BindErrorReason {
    match error.kind() {
        io::ErrorKind::AddrInUse => BindErrorReason {
            code: "address_in_use",
            suggestion: "stop the process using this port or change network.server_port",
        },
        io::ErrorKind::AddrNotAvailable => BindErrorReason {
            code: "address_not_available",
            suggestion: "set network.listen_ip to 0.0.0.0 or an IP assigned to this server",
        },
        io::ErrorKind::PermissionDenied => BindErrorReason {
            code: "permission_denied",
            suggestion: "run the service with sufficient privileges or choose a higher port",
        },
        io::ErrorKind::InvalidInput => BindErrorReason {
            code: "invalid_listen_addr",
            suggestion: "check network.listen_ip and network.server_port in config.toml",
        },
        _ => BindErrorReason {
            code: "bind_failed",
            suggestion: "check listener address, firewall, and kernel socket limits",
        },
    }
}

fn listen_addr(network: &NetworkConfig) -> anyhow::Result<SocketAddr> {
    let listen_host = network.listen_host();
    if listen_host.is_empty() {
        bail!("network.listen_ip or network.server_ip is required");
    }

    let endpoint = network.listen_endpoint();

    endpoint
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid listen endpoint: {endpoint}"))
}

async fn run_udp_listener(socket: UdpSocket, state: RuntimeState) -> anyhow::Result<()> {
    let socket = Arc::new(socket);
    let mut buf = vec![0_u8; 4096];

    loop {
        let (size, peer) = socket.recv_from(&mut buf).await?;
        state.stats().record_udp_rx(size as u64);
        debug!(%peer, size, "UDP packet received");

        let payload = buf[..size].to_vec();
        let packet_state = state.clone();
        let packet_socket = Arc::clone(&socket);
        tokio::spawn(async move {
            let response = handle_client_payload(&payload, &packet_state, TransportKind::Udp, peer)
                .await
                .unwrap_or_else(|error| {
                    warn!(%peer, ?error, "failed to build UDP response");
                    UDP_PROBE_RESPONSE.to_vec()
                });

            match packet_socket.send_to(&response, peer).await {
                Ok(sent) => packet_state.stats().record_udp_tx(sent as u64),
                Err(error) => warn!(%peer, ?error, "failed to send UDP response"),
            }
        });
    }
}

async fn run_tcp_listener(listener: TcpListener, state: RuntimeState) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        state.stats().record_tcp_accept();
        debug!(%peer, "TCP connection accepted");

        let connection_state = state.clone();
        tokio::spawn(async move {
            connection_state.stats().record_tcp_open();
            if let Err(error) = handle_tcp_connection(stream, connection_state.clone()).await {
                warn!(%peer, ?error, "TCP connection failed");
            }
            connection_state.stats().record_tcp_close();
        });
    }
}

async fn handle_tcp_connection(mut stream: TcpStream, state: RuntimeState) -> anyhow::Result<()> {
    let mut buf = [0_u8; 1024];
    let size = stream.read(&mut buf).await?;
    state.stats().record_tcp_rx(size as u64);

    let response = handle_client_payload(
        &buf[..size],
        &state,
        TransportKind::Tcp,
        stream.peer_addr()?,
    )
    .await
    .unwrap_or_else(|error| {
        warn!(?error, "failed to build TCP response");
        TCP_PROBE_RESPONSE.to_vec()
    });

    stream.write_all(&response).await?;
    state.stats().record_tcp_tx(response.len() as u64);
    stream.shutdown().await?;
    Ok(())
}

async fn handle_client_payload(
    payload: &[u8],
    state: &RuntimeState,
    transport: TransportKind,
    peer: SocketAddr,
) -> anyhow::Result<Vec<u8>> {
    match parse_client_message(payload) {
        ParsedClientMessage::LegacyPing => Ok(match transport {
            TransportKind::Tcp => TCP_PROBE_RESPONSE.to_vec(),
            TransportKind::Udp => UDP_PROBE_RESPONSE.to_vec(),
        }),
        ParsedClientMessage::Probe(request) => {
            build_probe_response(state, transport, peer, request)
        }
        ParsedClientMessage::SessionData(request) => {
            build_session_data_response(state, transport, peer, request).await
        }
        ParsedClientMessage::Invalid(message) => build_probe_error(state, transport, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_address_in_use_bind_error() {
        let error = io::Error::from(io::ErrorKind::AddrInUse);
        let reason = classify_bind_error(&error);

        assert_eq!(reason.code, "address_in_use");
        assert!(reason.suggestion.contains("network.server_port"));
    }

    #[test]
    fn classifies_address_not_available_bind_error() {
        let error = io::Error::from(io::ErrorKind::AddrNotAvailable);
        let reason = classify_bind_error(&error);

        assert_eq!(reason.code, "address_not_available");
        assert!(reason.suggestion.contains("network.listen_ip"));
    }
}
