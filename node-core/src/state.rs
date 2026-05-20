use crate::{config::NodeConfig, identity::IdentityState};
use serde::Serialize;
use std::{
    sync::Arc,
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
}

#[derive(Debug, Serialize)]
pub struct HealthConfigSnapshot {
    pub channel: String,
    pub health_addr: String,
    pub network_loaded: bool,
    pub disable_quic: Option<bool>,
    pub area: Option<String>,
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
            }),
        }
    }

    pub fn config(&self) -> &NodeConfig {
        &self.inner.config
    }

    pub fn identity(&self) -> &IdentityState {
        &self.inner.identity
    }

    pub fn status(&self) -> &'static str {
        match &self.inner.status {
            NodeStatus::Ready => "ready",
            NodeStatus::Registered => "registered",
        }
    }

    pub fn health_snapshot(&self) -> HealthSnapshot {
        let network = self.inner.config.network.as_ref();

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
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
