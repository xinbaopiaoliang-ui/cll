use crate::{config::NodeConfig, identity::IdentityState};
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
    pub disable_quic: Option<bool>,
    pub area: Option<String>,
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
    pub probe_sessions_total: u64,
    pub probe_rejected: u64,
    pub auth_missing: u64,
    pub auth_ok: u64,
    pub auth_failed: u64,
    pub last_probe_session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ControlPlaneSnapshot {
    pub enabled: bool,
    pub last_success_at: Option<u64>,
    pub last_failure_at: Option<u64>,
    pub last_http_status: Option<u16>,
    pub last_error: Option<String>,
    pub report_ok: u64,
    pub report_failed: u64,
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
    last_probe_session_id: Mutex<Option<String>>,
    control_last_success_at: AtomicU64,
    control_last_failure_at: AtomicU64,
    control_last_http_status: AtomicU64,
    control_report_ok: AtomicU64,
    control_report_failed: AtomicU64,
    control_last_error: Mutex<Option<String>>,
}

impl RuntimeState {
    pub fn new(config: NodeConfig, identity: IdentityState) -> Self {
        let status = if identity.is_bootstrap_placeholder() {
            NodeStatus::Registered
        } else {
            NodeStatus::Ready
        };

        Self {
            inner: Arc::new(RuntimeStateInner {
                config,
                identity,
                started_at: now_unix(),
                status,
                stats: Arc::new(RuntimeStats::default()),
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

    pub fn status(&self) -> &'static str {
        match &self.inner.status {
            NodeStatus::Ready => "ready",
            NodeStatus::Registered => "registered",
        }
    }

    pub fn health_snapshot(&self) -> HealthSnapshot {
        let network = self.inner.config.network.as_ref();

        let listen_addr = network.map(|network| {
            if network.server_ip.contains(':') {
                format!("[{}]:{}", network.server_ip, network.server_port)
            } else {
                format!("{}:{}", network.server_ip, network.server_port)
            }
        });

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
                disable_quic: network.map(|network| network.disable_quic),
                area: network.map(|network| network.area.clone()),
            },
            listeners: ListenerSnapshot {
                udp_listening: self.inner.stats.udp_listening(),
                tcp_listening: self.inner.stats.tcp_listening(),
                listen_addr,
            },
            traffic: self.inner.stats.traffic_snapshot(),
            sessions: self.inner.stats.session_snapshot(),
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

    pub fn session_snapshot(&self) -> SessionSnapshot {
        let last_probe_session_id = self
            .last_probe_session_id
            .lock()
            .ok()
            .and_then(|last_probe_session_id| last_probe_session_id.clone());

        SessionSnapshot {
            active_tcp_connections: self.tcp_active.load(Ordering::Relaxed),
            probe_sessions_total: self.probe_sessions_total.load(Ordering::Relaxed),
            probe_rejected: self.probe_rejected.load(Ordering::Relaxed),
            auth_missing: self.auth_missing.load(Ordering::Relaxed),
            auth_ok: self.auth_ok.load(Ordering::Relaxed),
            auth_failed: self.auth_failed.load(Ordering::Relaxed),
            last_probe_session_id,
        }
    }

    pub fn control_plane_snapshot(&self, enabled: bool) -> ControlPlaneSnapshot {
        let last_success_at = unix_option(self.control_last_success_at.load(Ordering::Relaxed));
        let last_failure_at = unix_option(self.control_last_failure_at.load(Ordering::Relaxed));
        let last_http_status = self.control_last_http_status.load(Ordering::Relaxed);
        let last_error = self
            .control_last_error
            .lock()
            .ok()
            .and_then(|last_error| last_error.clone());

        ControlPlaneSnapshot {
            enabled,
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
