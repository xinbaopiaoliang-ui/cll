use crate::{
    config::{BandwidthQuality, NetworkConfig, NodeConfig},
    identity::IdentityState,
    session_store::SessionStore,
};
use serde::Serialize;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct RuntimeState {
    inner: Arc<RuntimeStateInner>,
}

struct RuntimeStateInner {
    config: NodeConfig,
    identity: IdentityState,
    started_at: u64,
    status: NodeStatus,
    stats: Arc<RuntimeStats>,
    sessions: Arc<SessionStore>,
    runtime_config: Mutex<RuntimeConfigState>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Ready,
    Registered,
}

#[derive(Debug, Serialize)]
pub struct HealthSnapshot {
    pub status: NodeStatus,
    pub version: &'static str,
    pub node_id: Option<u64>,
    pub panel_url: Option<String>,
    pub uptime_sec: u64,
    pub config: HealthConfigSnapshot,
    pub listeners: ListenerSnapshot,
    pub traffic: TrafficSnapshot,
    pub sessions: SessionSnapshot,
    pub control_plane: ControlPlaneSnapshot,
}

#[derive(Debug, Serialize)]
pub struct HealthConfigSnapshot {
    pub channel: String,
    pub health_addr: String,
    pub network_loaded: bool,
    pub config_revision: u64,
    pub disable_quic: Option<bool>,
    pub area: Option<String>,
    pub bandwidth_quality: Option<BandwidthQuality>,
    pub tag: Option<String>,
    pub restart_required: bool,
}

#[derive(Debug, Serialize)]
pub struct ListenerSnapshot {
    pub udp_listening: bool,
    pub tcp_listening: bool,
    pub listen_addr: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TrafficSnapshot {
    pub udp_rx_packets: u64,
    pub udp_rx_bytes: u64,
    pub udp_tx_packets: u64,
    pub udp_tx_bytes: u64,
    pub tcp_accepted: u64,
    pub tcp_rx_bytes: u64,
    pub tcp_tx_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct SessionSnapshot {
    pub active_tcp_connections: u64,
    pub active_udp_sessions: u64,
    pub probe_sessions_total: u64,
    pub probe_rejected: u64,
    pub auth_missing: u64,
    pub auth_ok: u64,
    pub auth_failed: u64,
    pub udp_session_rx_packets: u64,
    pub udp_session_rx_bytes: u64,
    pub udp_session_tx_packets: u64,
    pub udp_session_tx_bytes: u64,
    pub udp_session_miss: u64,
    pub udp_session_expired: u64,
    pub udp_relay_tx_packets: u64,
    pub udp_relay_tx_bytes: u64,
    pub udp_relay_rx_packets: u64,
    pub udp_relay_rx_bytes: u64,
    pub udp_relay_timeout: u64,
    pub udp_relay_error: u64,
    pub last_probe_session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ControlPlaneSnapshot {
    pub enabled: bool,
    pub handshake_last_success_at: Option<u64>,
    pub handshake_last_failure_at: Option<u64>,
    pub handshake_last_http_status: Option<u16>,
    pub handshake_last_error: Option<String>,
    pub handshake_ok: u64,
    pub handshake_failed: u64,
    pub last_success_at: Option<u64>,
    pub last_failure_at: Option<u64>,
    pub last_http_status: Option<u16>,
    pub last_error: Option<String>,
    pub report_ok: u64,
    pub report_failed: u64,
    pub config_last_success_at: Option<u64>,
    pub config_last_failure_at: Option<u64>,
    pub config_last_http_status: Option<u16>,
    pub config_last_error: Option<String>,
    pub config_ok: u64,
    pub config_failed: u64,
}

#[derive(Debug, Clone)]
pub struct ConfigApplyResult {
    pub applied: bool,
    pub restart_required: bool,
    pub previous_revision: u64,
    pub current_revision: u64,
}

#[derive(Debug, Clone)]
struct RuntimeConfigState {
    config_revision: u64,
    network: Option<NetworkConfig>,
    restart_required: bool,
}

#[derive(Default)]
pub struct RuntimeStats {
    udp_listening: AtomicBool,
    tcp_listening: AtomicBool,
    udp_rx_packets: AtomicU64,
    udp_rx_bytes: AtomicU64,
    udp_tx_packets: AtomicU64,
    udp_tx_bytes: AtomicU64,
    tcp_accepted: AtomicU64,
    tcp_active: AtomicU64,
    tcp_rx_bytes: AtomicU64,
    tcp_tx_bytes: AtomicU64,
    probe_sequence: AtomicU64,
    probe_sessions_total: AtomicU64,
    probe_rejected: AtomicU64,
    auth_missing: AtomicU64,
    auth_ok: AtomicU64,
    auth_failed: AtomicU64,
    udp_session_rx_packets: AtomicU64,
    udp_session_rx_bytes: AtomicU64,
    udp_session_tx_packets: AtomicU64,
    udp_session_tx_bytes: AtomicU64,
    udp_session_miss: AtomicU64,
    udp_session_expired: AtomicU64,
    udp_relay_tx_packets: AtomicU64,
    udp_relay_tx_bytes: AtomicU64,
    udp_relay_rx_packets: AtomicU64,
    udp_relay_rx_bytes: AtomicU64,
    udp_relay_timeout: AtomicU64,
    udp_relay_error: AtomicU64,
    last_probe_session_id: Mutex<Option<String>>,
    control_last_success_at: AtomicU64,
    control_last_failure_at: AtomicU64,
    control_last_http_status: AtomicU64,
    control_report_ok: AtomicU64,
    control_report_failed: AtomicU64,
    control_last_error: Mutex<Option<String>>,
    handshake_last_success_at: AtomicU64,
    handshake_last_failure_at: AtomicU64,
    handshake_last_http_status: AtomicU64,
    handshake_ok: AtomicU64,
    handshake_failed: AtomicU64,
    handshake_last_error: Mutex<Option<String>>,
    config_last_success_at: AtomicU64,
    config_last_failure_at: AtomicU64,
    config_last_http_status: AtomicU64,
    config_ok: AtomicU64,
    config_failed: AtomicU64,
    config_last_error: Mutex<Option<String>>,
}

impl RuntimeState {
    pub fn new(config: NodeConfig, identity: IdentityState) -> Self {
        let status = if identity.is_bootstrap_placeholder() {
            NodeStatus::Registered
        } else {
            NodeStatus::Ready
        };

        let config_revision = config
            .control
            .as_ref()
            .map(|control| control.config_revision)
            .unwrap_or(1)
            .max(1);
        let network = config.network.clone();

        Self {
            inner: Arc::new(RuntimeStateInner {
                config,
                identity,
                started_at: now_unix(),
                status,
                stats: Arc::new(RuntimeStats::default()),
                sessions: Arc::new(SessionStore::default()),
                runtime_config: Mutex::new(RuntimeConfigState {
                    config_revision,
                    network,
                    restart_required: false,
                }),
            }),
        }
    }

    pub fn config(&self) -> &NodeConfig {
        &self.inner.config
    }

    pub fn identity(&self) -> &IdentityState {
        &self.inner.identity
    }

    pub fn stats(&self) -> &RuntimeStats {
        &self.inner.stats
    }

    pub fn sessions(&self) -> &SessionStore {
        &self.inner.sessions
    }

    pub fn config_revision(&self) -> u64 {
        self.inner
            .runtime_config
            .lock()
            .map(|config| config.config_revision)
            .unwrap_or(1)
    }

    pub fn effective_network(&self) -> Option<NetworkConfig> {
        self.inner
            .runtime_config
            .lock()
            .ok()
            .and_then(|config| config.network.clone())
            .or_else(|| self.inner.config.network.clone())
    }

    pub fn apply_remote_network_config(
        &self,
        config_revision: u64,
        remote_network: NetworkConfig,
    ) -> ConfigApplyResult {
        let Ok(mut config) = self.inner.runtime_config.lock() else {
            return ConfigApplyResult {
                applied: false,
                restart_required: false,
                previous_revision: self.config_revision(),
                current_revision: self.config_revision(),
            };
        };
        let previous_revision = config.config_revision;
        if config_revision <= previous_revision {
            return ConfigApplyResult {
                applied: false,
                restart_required: config.restart_required,
                previous_revision,
                current_revision: previous_revision,
            };
        }

        let mut next_network = config
            .network
            .clone()
            .unwrap_or_else(|| remote_network.clone());
        let restart_required = network_listener_changed(&next_network, &remote_network);
        if !restart_required {
            next_network.server_ip = remote_network.server_ip.clone();
            next_network.listen_ip = remote_network.listen_ip.clone();
            next_network.server_port = remote_network.server_port;
        }
        next_network.relay_server_ip = remote_network.relay_server_ip.clone();
        next_network.relay_server_port = remote_network.relay_server_port;
        next_network.is_support_ipv6 = remote_network.is_support_ipv6;
        next_network.disable_quic = remote_network.disable_quic;
        next_network.area = remote_network.area.clone();
        next_network.bandwidth_quality = remote_network.bandwidth_quality.clone();
        next_network.tag = remote_network.tag.clone();
        next_network.operator_ips = remote_network.operator_ips.clone();

        config.config_revision = config_revision;
        config.network = Some(next_network);
        config.restart_required = config.restart_required || restart_required;

        ConfigApplyResult {
            applied: true,
            restart_required: config.restart_required,
            previous_revision,
            current_revision: config_revision,
        }
    }

    pub fn status(&self) -> &'static str {
        match &self.inner.status {
            NodeStatus::Ready => "ready",
            NodeStatus::Registered => "registered",
        }
    }

    pub fn health_snapshot(&self) -> HealthSnapshot {
        let runtime_config = self
            .inner
            .runtime_config
            .lock()
            .ok()
            .map(|config| config.clone())
            .unwrap_or(RuntimeConfigState {
                config_revision: 1,
                network: self.inner.config.network.clone(),
                restart_required: false,
            });
        let network = runtime_config.network.as_ref();

        let listen_addr = network.map(|network| network.listen_endpoint());

        HealthSnapshot {
            status: self.inner.status.clone(),
            version: env!("CARGO_PKG_VERSION"),
            node_id: self.inner.identity.node_id,
            panel_url: self.inner.identity.panel_url.clone(),
            uptime_sec: now_unix().saturating_sub(self.inner.started_at),
            config: HealthConfigSnapshot {
                channel: self.inner.config.runtime.channel.clone(),
                health_addr: self.inner.config.runtime.health_addr.to_string(),
                network_loaded: network.is_some(),
                config_revision: runtime_config.config_revision,
                disable_quic: network.map(|network| network.disable_quic),
                area: network.map(|network| network.area.clone()),
                bandwidth_quality: network.map(|network| network.bandwidth_quality.clone()),
                tag: network.and_then(|network| network.tag.clone()),
                restart_required: runtime_config.restart_required,
            },
            listeners: ListenerSnapshot {
                udp_listening: self.inner.stats.udp_listening(),
                tcp_listening: self.inner.stats.tcp_listening(),
                listen_addr,
            },
            traffic: self.inner.stats.traffic_snapshot(),
            sessions: self
                .inner
                .stats
                .session_snapshot(self.inner.sessions.active_udp_session_count()),
            control_plane: self.inner.stats.control_plane_snapshot(
                self.inner
                    .config
                    .control
                    .as_ref()
                    .map(|control| control.enabled)
                    .unwrap_or(false),
            ),
        }
    }
}

fn network_listener_changed(current: &NetworkConfig, remote: &NetworkConfig) -> bool {
    current.server_ip != remote.server_ip
        || current.listen_ip != remote.listen_ip
        || current.server_port != remote.server_port
}

impl RuntimeStats {
    pub fn set_udp_listening(&self, value: bool) {
        self.udp_listening.store(value, Ordering::Relaxed);
    }

    pub fn set_tcp_listening(&self, value: bool) {
        self.tcp_listening.store(value, Ordering::Relaxed);
    }

    pub fn udp_listening(&self) -> bool {
        self.udp_listening.load(Ordering::Relaxed)
    }

    pub fn tcp_listening(&self) -> bool {
        self.tcp_listening.load(Ordering::Relaxed)
    }

    pub fn record_udp_rx(&self, bytes: u64) {
        self.udp_rx_packets.fetch_add(1, Ordering::Relaxed);
        self.udp_rx_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_udp_tx(&self, bytes: u64) {
        self.udp_tx_packets.fetch_add(1, Ordering::Relaxed);
        self.udp_tx_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_tcp_accept(&self) {
        self.tcp_accepted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_tcp_open(&self) {
        self.tcp_active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_tcp_close(&self) {
        self.tcp_active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn record_tcp_rx(&self, bytes: u64) {
        self.tcp_rx_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_tcp_tx(&self, bytes: u64) {
        self.tcp_tx_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn next_probe_sequence(&self) -> u64 {
        self.probe_sequence.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn record_probe_session(&self, session_id: String) {
        self.probe_sessions_total.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut last_probe_session_id) = self.last_probe_session_id.lock() {
            *last_probe_session_id = Some(session_id);
        }
    }

    pub fn record_probe_rejected(&self) {
        self.probe_rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_auth_missing(&self) {
        self.auth_missing.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_auth_ok(&self) {
        self.auth_ok.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_auth_failed(&self) {
        self.auth_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_udp_session_rx(&self, bytes: u64) {
        self.udp_session_rx_packets.fetch_add(1, Ordering::Relaxed);
        self.udp_session_rx_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_udp_session_tx(&self, bytes: u64) {
        self.udp_session_tx_packets.fetch_add(1, Ordering::Relaxed);
        self.udp_session_tx_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_udp_session_miss(&self) {
        self.udp_session_miss.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_udp_session_expired(&self) {
        self.udp_session_expired.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_udp_relay_tx(&self, bytes: u64) {
        self.udp_relay_tx_packets.fetch_add(1, Ordering::Relaxed);
        self.udp_relay_tx_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_udp_relay_rx(&self, bytes: u64) {
        self.udp_relay_rx_packets.fetch_add(1, Ordering::Relaxed);
        self.udp_relay_rx_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_udp_relay_timeout(&self) {
        self.udp_relay_timeout.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_udp_relay_error(&self) {
        self.udp_relay_error.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_control_success(&self, http_status: u16) {
        self.control_report_ok.fetch_add(1, Ordering::Relaxed);
        self.control_last_success_at
            .store(now_unix(), Ordering::Relaxed);
        self.control_last_http_status
            .store(u64::from(http_status), Ordering::Relaxed);
        if let Ok(mut last_error) = self.control_last_error.lock() {
            *last_error = None;
        }
    }

    pub fn record_control_failure(&self, http_status: Option<u16>, error: impl Into<String>) {
        self.control_report_failed.fetch_add(1, Ordering::Relaxed);
        self.control_last_failure_at
            .store(now_unix(), Ordering::Relaxed);
        if let Some(status) = http_status {
            self.control_last_http_status
                .store(u64::from(status), Ordering::Relaxed);
        }
        if let Ok(mut last_error) = self.control_last_error.lock() {
            *last_error = Some(error.into());
        }
    }

    pub fn record_handshake_success(&self, http_status: u16) {
        self.handshake_ok.fetch_add(1, Ordering::Relaxed);
        self.handshake_last_success_at
            .store(now_unix(), Ordering::Relaxed);
        self.handshake_last_http_status
            .store(u64::from(http_status), Ordering::Relaxed);
        if let Ok(mut last_error) = self.handshake_last_error.lock() {
            *last_error = None;
        }
    }

    pub fn record_handshake_failure(&self, http_status: Option<u16>, error: impl Into<String>) {
        self.handshake_failed.fetch_add(1, Ordering::Relaxed);
        self.handshake_last_failure_at
            .store(now_unix(), Ordering::Relaxed);
        if let Some(status) = http_status {
            self.handshake_last_http_status
                .store(u64::from(status), Ordering::Relaxed);
        }
        if let Ok(mut last_error) = self.handshake_last_error.lock() {
            *last_error = Some(error.into());
        }
    }

    pub fn record_config_success(&self, http_status: u16) {
        self.config_ok.fetch_add(1, Ordering::Relaxed);
        self.config_last_success_at
            .store(now_unix(), Ordering::Relaxed);
        self.config_last_http_status
            .store(u64::from(http_status), Ordering::Relaxed);
        if let Ok(mut last_error) = self.config_last_error.lock() {
            *last_error = None;
        }
    }

    pub fn record_config_failure(&self, http_status: Option<u16>, error: impl Into<String>) {
        self.config_failed.fetch_add(1, Ordering::Relaxed);
        self.config_last_failure_at
            .store(now_unix(), Ordering::Relaxed);
        if let Some(status) = http_status {
            self.config_last_http_status
                .store(u64::from(status), Ordering::Relaxed);
        }
        if let Ok(mut last_error) = self.config_last_error.lock() {
            *last_error = Some(error.into());
        }
    }

    pub fn traffic_snapshot(&self) -> TrafficSnapshot {
        TrafficSnapshot {
            udp_rx_packets: self.udp_rx_packets.load(Ordering::Relaxed),
            udp_rx_bytes: self.udp_rx_bytes.load(Ordering::Relaxed),
            udp_tx_packets: self.udp_tx_packets.load(Ordering::Relaxed),
            udp_tx_bytes: self.udp_tx_bytes.load(Ordering::Relaxed),
            tcp_accepted: self.tcp_accepted.load(Ordering::Relaxed),
            tcp_rx_bytes: self.tcp_rx_bytes.load(Ordering::Relaxed),
            tcp_tx_bytes: self.tcp_tx_bytes.load(Ordering::Relaxed),
        }
    }

    pub fn session_snapshot(&self, active_udp_sessions: u64) -> SessionSnapshot {
        let last_probe_session_id = self
            .last_probe_session_id
            .lock()
            .ok()
            .and_then(|last_probe_session_id| last_probe_session_id.clone());

        SessionSnapshot {
            active_tcp_connections: self.tcp_active.load(Ordering::Relaxed),
            active_udp_sessions,
            probe_sessions_total: self.probe_sessions_total.load(Ordering::Relaxed),
            probe_rejected: self.probe_rejected.load(Ordering::Relaxed),
            auth_missing: self.auth_missing.load(Ordering::Relaxed),
            auth_ok: self.auth_ok.load(Ordering::Relaxed),
            auth_failed: self.auth_failed.load(Ordering::Relaxed),
            udp_session_rx_packets: self.udp_session_rx_packets.load(Ordering::Relaxed),
            udp_session_rx_bytes: self.udp_session_rx_bytes.load(Ordering::Relaxed),
            udp_session_tx_packets: self.udp_session_tx_packets.load(Ordering::Relaxed),
            udp_session_tx_bytes: self.udp_session_tx_bytes.load(Ordering::Relaxed),
            udp_session_miss: self.udp_session_miss.load(Ordering::Relaxed),
            udp_session_expired: self.udp_session_expired.load(Ordering::Relaxed),
            udp_relay_tx_packets: self.udp_relay_tx_packets.load(Ordering::Relaxed),
            udp_relay_tx_bytes: self.udp_relay_tx_bytes.load(Ordering::Relaxed),
            udp_relay_rx_packets: self.udp_relay_rx_packets.load(Ordering::Relaxed),
            udp_relay_rx_bytes: self.udp_relay_rx_bytes.load(Ordering::Relaxed),
            udp_relay_timeout: self.udp_relay_timeout.load(Ordering::Relaxed),
            udp_relay_error: self.udp_relay_error.load(Ordering::Relaxed),
            last_probe_session_id,
        }
    }

    pub fn control_plane_snapshot(&self, enabled: bool) -> ControlPlaneSnapshot {
        let last_success_at = unix_option(self.control_last_success_at.load(Ordering::Relaxed));
        let last_failure_at = unix_option(self.control_last_failure_at.load(Ordering::Relaxed));
        let last_http_status = self.control_last_http_status.load(Ordering::Relaxed);
        let handshake_last_success_at =
            unix_option(self.handshake_last_success_at.load(Ordering::Relaxed));
        let handshake_last_failure_at =
            unix_option(self.handshake_last_failure_at.load(Ordering::Relaxed));
        let handshake_last_http_status = self.handshake_last_http_status.load(Ordering::Relaxed);
        let config_last_success_at =
            unix_option(self.config_last_success_at.load(Ordering::Relaxed));
        let config_last_failure_at =
            unix_option(self.config_last_failure_at.load(Ordering::Relaxed));
        let config_last_http_status = self.config_last_http_status.load(Ordering::Relaxed);
        let last_error = self
            .control_last_error
            .lock()
            .ok()
            .and_then(|last_error| last_error.clone());
        let handshake_last_error = self
            .handshake_last_error
            .lock()
            .ok()
            .and_then(|last_error| last_error.clone());
        let config_last_error = self
            .config_last_error
            .lock()
            .ok()
            .and_then(|last_error| last_error.clone());

        ControlPlaneSnapshot {
            enabled,
            handshake_last_success_at,
            handshake_last_failure_at,
            handshake_last_http_status: if handshake_last_http_status == 0 {
                None
            } else {
                Some(handshake_last_http_status as u16)
            },
            handshake_last_error,
            handshake_ok: self.handshake_ok.load(Ordering::Relaxed),
            handshake_failed: self.handshake_failed.load(Ordering::Relaxed),
            last_success_at,
            last_failure_at,
            last_http_status: if last_http_status == 0 {
                None
            } else {
                Some(last_http_status as u16)
            },
            last_error,
            report_ok: self.control_report_ok.load(Ordering::Relaxed),
            report_failed: self.control_report_failed.load(Ordering::Relaxed),
            config_last_success_at,
            config_last_failure_at,
            config_last_http_status: if config_last_http_status == 0 {
                None
            } else {
                Some(config_last_http_status as u16)
            },
            config_last_error,
            config_ok: self.config_ok.load(Ordering::Relaxed),
            config_failed: self.config_failed.load(Ordering::Relaxed),
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn unix_option(value: u64) -> Option<u64> {
    if value == 0 {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BootstrapConfig, ControlPlaneConfig, IdentityConfig, RuntimeConfig};
    use std::net::SocketAddr;

    fn base_network() -> NetworkConfig {
        NetworkConfig {
            server_ip: "103.201.131.99".to_string(),
            listen_ip: Some("0.0.0.0".to_string()),
            server_port: 666,
            relay_server_ip: None,
            relay_server_port: None,
            is_support_ipv6: false,
            disable_quic: false,
            area: "UNKNOWN".to_string(),
            bandwidth_quality: BandwidthQuality::Normal,
            tag: Some("standalone".to_string()),
            operator_ips: None,
        }
    }

    fn state_with_network(network: NetworkConfig) -> RuntimeState {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config = NodeConfig {
            identity: IdentityConfig {
                node_id: Some(1),
                panel_url: Some("http://127.0.0.1:18080".to_string()),
                identity_file: temp_dir.path().join("identity.json").display().to_string(),
            },
            runtime: RuntimeConfig {
                data_dir: temp_dir.path().display().to_string(),
                log_dir: temp_dir.path().join("log").display().to_string(),
                health_addr: "127.0.0.1:9876".parse::<SocketAddr>().unwrap(),
                channel: "stable".to_string(),
            },
            bootstrap: Some(BootstrapConfig {
                response_file: temp_dir
                    .path()
                    .join("bootstrap-response.json")
                    .display()
                    .to_string(),
            }),
            control: Some(ControlPlaneConfig {
                enabled: true,
                config_revision: 1,
                request_timeout_sec: 5,
                config_poll_interval_sec: 30,
            }),
            network: Some(network),
            report: None,
            limits: None,
        };
        let identity = IdentityState::from_config(&config).expect("identity loads");
        RuntimeState::new(config, identity)
    }

    #[test]
    fn applies_remote_config_without_listener_restart() {
        let state = state_with_network(base_network());
        let mut remote = base_network();
        remote.area = "HK".to_string();
        remote.bandwidth_quality = BandwidthQuality::Fast;
        remote.disable_quic = true;
        remote.tag = Some("premium".to_string());

        let result = state.apply_remote_network_config(2, remote);
        let network = state.effective_network().expect("network");
        let health = state.health_snapshot();

        assert!(result.applied);
        assert!(!result.restart_required);
        assert_eq!(result.previous_revision, 1);
        assert_eq!(result.current_revision, 2);
        assert_eq!(network.server_ip, "103.201.131.99");
        assert_eq!(network.server_port, 666);
        assert_eq!(network.area, "HK");
        assert!(matches!(network.bandwidth_quality, BandwidthQuality::Fast));
        assert_eq!(network.tag.as_deref(), Some("premium"));
        assert!(network.disable_quic);
        assert_eq!(health.config.config_revision, 2);
        assert!(!health.config.restart_required);
    }

    #[test]
    fn listener_change_requires_restart_and_preserves_bound_endpoint() {
        let state = state_with_network(base_network());
        let mut remote = base_network();
        remote.server_ip = "47.83.160.126".to_string();
        remote.server_port = 667;
        remote.area = "SG".to_string();

        let result = state.apply_remote_network_config(2, remote);
        let network = state.effective_network().expect("network");
        let health = state.health_snapshot();

        assert!(result.applied);
        assert!(result.restart_required);
        assert_eq!(network.server_ip, "103.201.131.99");
        assert_eq!(network.server_port, 666);
        assert_eq!(network.area, "SG");
        assert_eq!(health.config.config_revision, 2);
        assert!(health.config.restart_required);
    }
}
