use crate::{
    config::{NetworkConfig, NodeConfig},
    state::RuntimeState,
};
use anyhow::{bail, Context};
use std::net::SocketAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    task::JoinHandle,
};
use tracing::{debug, info, warn};

const UDP_PROBE_RESPONSE: &[u8] = b"xaccel-node udp listener ready\n";
const TCP_PROBE_RESPONSE: &[u8] = b"xaccel-node tcp listener ready\n";

pub async fn spawn_network_listeners(state: RuntimeState) -> anyhow::Result<Vec<JoinHandle<()>>> {
    let Some(network) = state.config().network.as_ref() else {
        warn!("network config is missing; server listener is disabled");
        return Ok(Vec::new());
    };

    let listen_addr = listen_addr(state.config(), network)?;
    let udp_socket = UdpSocket::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind UDP listener {listen_addr}"))?;
    let tcp_listener = TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind TCP listener {listen_addr}"))?;

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

fn listen_addr(_config: &NodeConfig, network: &NetworkConfig) -> anyhow::Result<SocketAddr> {
    let server_ip = network.server_ip.trim();
    if server_ip.is_empty() {
        bail!("network.server_ip is required");
    }

    let endpoint = if server_ip.contains(':') {
        format!("[{}]:{}", server_ip, network.server_port)
    } else {
        format!("{}:{}", server_ip, network.server_port)
    };

    endpoint
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid listen endpoint: {endpoint}"))
}

async fn run_udp_listener(socket: UdpSocket, state: RuntimeState) -> anyhow::Result<()> {
    let mut buf = vec![0_u8; 2048];

    loop {
        let (size, peer) = socket.recv_from(&mut buf).await?;
        state.stats().record_udp_rx(size as u64);
        debug!(%peer, size, "UDP packet received");

        match socket.send_to(UDP_PROBE_RESPONSE, peer).await {
            Ok(sent) => state.stats().record_udp_tx(sent as u64),
            Err(error) => warn!(%peer, ?error, "failed to send UDP probe response"),
        }
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

    stream.write_all(TCP_PROBE_RESPONSE).await?;
    state.stats().record_tcp_tx(TCP_PROBE_RESPONSE.len() as u64);
    stream.shutdown().await?;
    Ok(())
}
