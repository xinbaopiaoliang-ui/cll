use crate::{
    session::{
        build_probe_error, build_probe_response, build_raw_udp_tunnel_response,
        build_session_data_response, parse_client_message, ParsedClientMessage, TransportKind,
    },
    state::RuntimeState,
};
use anyhow::Context;
use quinn::{Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::{net::SocketAddr, sync::Arc};
use tokio::task::JoinHandle;
use tracing::{info, warn};

const MAX_QUIC_FRAME_BYTES: usize = 64 * 1024;

pub async fn spawn_quic_tunnel(state: RuntimeState) -> anyhow::Result<Option<JoinHandle<()>>> {
    let Some(network) = state.effective_network() else {
        warn!("QUIC tunnel disabled: network config is missing");
        return Ok(None);
    };
    let Some(endpoint) = network.quic_listen_endpoint() else {
        if network.disable_quic {
            info!("QUIC tunnel disabled by network.disable_quic");
        } else {
            info!("QUIC tunnel disabled: network.relay_server_port is not configured");
        }
        return Ok(None);
    };
    let listen_addr = endpoint
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid QUIC listen endpoint: {endpoint}"))?;
    let server_config = build_server_config()?;
    let endpoint = Endpoint::server(server_config, listen_addr)
        .with_context(|| format!("failed to bind QUIC endpoint {listen_addr}"))?;
    info!(%listen_addr, "QUIC tunnel listener started");

    let task = tokio::spawn(async move {
        if let Err(error) = run_quic_endpoint(endpoint, state).await {
            warn!(?error, "QUIC tunnel listener stopped");
        }
    });
    Ok(Some(task))
}

fn build_server_config() -> anyhow::Result<ServerConfig> {
    let cert = rcgen::generate_simple_self_signed(vec!["xaccel-node".to_string()])
        .context("failed to generate QUIC self-signed certificate")?;
    let cert_der = CertificateDer::from(
        cert.serialize_der()
            .context("failed to encode QUIC certificate")?,
    );
    let key_der = PrivatePkcs8KeyDer::from(cert.serialize_private_key_der());
    let mut server_config = ServerConfig::with_single_cert(vec![cert_der], key_der.into())
        .context("failed to build QUIC server config")?;
    Arc::get_mut(&mut server_config.transport)
        .expect("server config transport is not shared yet")
        .max_concurrent_bidi_streams(128_u32.into());
    Ok(server_config)
}

async fn run_quic_endpoint(endpoint: Endpoint, state: RuntimeState) -> anyhow::Result<()> {
    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(connection) => handle_quic_connection(connection, state).await,
                Err(error) => warn!(?error, "failed to accept QUIC connection"),
            }
        });
    }
    Ok(())
}

async fn handle_quic_connection(connection: Connection, state: RuntimeState) {
    let peer = connection.remote_address();
    loop {
        match connection.accept_bi().await {
            Ok((send, recv)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_quic_stream(send, recv, state, peer).await {
                        warn!(%peer, ?error, "QUIC tunnel stream failed");
                    }
                });
            }
            Err(quinn::ConnectionError::ApplicationClosed(_))
            | Err(quinn::ConnectionError::LocallyClosed)
            | Err(quinn::ConnectionError::ConnectionClosed(_)) => break,
            Err(error) => {
                warn!(%peer, ?error, "QUIC tunnel connection failed");
                break;
            }
        }
    }
}

async fn handle_quic_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    state: RuntimeState,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    let payload = recv
        .read_to_end(MAX_QUIC_FRAME_BYTES)
        .await
        .context("failed to read QUIC tunnel frame")?;
    let response = handle_quic_payload(&payload, &state, peer).await?;
    send.write_all(&response)
        .await
        .context("failed to write QUIC tunnel response")?;
    send.finish()
        .context("failed to finish QUIC tunnel response")?;
    Ok(())
}

async fn handle_quic_payload(
    payload: &[u8],
    state: &RuntimeState,
    peer: SocketAddr,
) -> anyhow::Result<Vec<u8>> {
    match parse_client_message(payload) {
        ParsedClientMessage::LegacyPing => Ok(b"xaccel-node quic listener ready\n".to_vec()),
        ParsedClientMessage::Probe(request) => {
            build_probe_response(state, TransportKind::Quic, peer, request)
        }
        ParsedClientMessage::SessionData(request) => {
            build_session_data_response(state, TransportKind::Quic, peer, request).await
        }
        ParsedClientMessage::RawUdpTunnel(request) => {
            build_raw_udp_tunnel_response(state, TransportKind::Quic, peer, request).await
        }
        ParsedClientMessage::Invalid(message) => {
            build_probe_error(state, TransportKind::Quic, message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_quic_server_config() {
        build_server_config().expect("QUIC server config builds");
    }
}
