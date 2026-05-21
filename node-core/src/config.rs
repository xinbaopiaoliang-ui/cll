use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::{fs, net::SocketAddr, path::Path};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NodeConfig {
    pub identity: IdentityConfig,
    pub runtime: RuntimeConfig,
    pub bootstrap: Option<BootstrapConfig>,
    pub control: Option<ControlPlaneConfig>,
    pub network: Option<NetworkConfig>,
    pub report: Option<ReportConfig>,
    pub limits: Option<LimitConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IdentityConfig {
    pub node_id: Option<u64>,
    pub panel_url: Option<String>,
    pub identity_file: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuntimeConfig {
    pub data_dir: String,
    pub log_dir: String,
    pub health_addr: SocketAddr,
    pub channel: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BootstrapConfig {
    pub response_file: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ControlPlaneConfig {
    pub enabled: bool,
    pub config_revision: u64,
    pub request_timeout_sec: u64,
}

impl Default for ControlPlaneConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            config_revision: 1,
            request_timeout_sec: 5,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkConfig {
    pub server_ip: String,
    pub server_port: u16,
    pub relay_server_ip: Option<String>,
    pub relay_server_port: Option<u16>,
    pub is_support_ipv6: bool,
    pub disable_quic: bool,
    pub area: String,
    pub bandwidth_quality: BandwidthQuality,
    pub tag: Option<String>,
    pub operator_ips: Option<OperatorIps>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BandwidthQuality {
    Fast,
    Normal,
    Slow,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OperatorIps {
    pub telecom_ip: Option<String>,
    pub mobile_ip: Option<String>,
    pub unicom_ip: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReportConfig {
    pub interval_sec: u64,
    pub traffic_batch_sec: u64,
    pub metrics_interval_sec: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LimitConfig {
    pub max_sessions: u64,
    pub max_sessions_per_user: u64,
    pub max_udp_mappings: u64,
    pub default_user_speed_mbps: u64,
}

impl NodeConfig {
    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse TOML {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.identity.identity_file.trim().is_empty() {
            bail!("identity.identity_file is required");
        }

        if self.runtime.data_dir.trim().is_empty() {
            bail!("runtime.data_dir is required");
        }

        if self.runtime.log_dir.trim().is_empty() {
            bail!("runtime.log_dir is required");
        }

        if self.runtime.channel.trim().is_empty() {
            bail!("runtime.channel is required");
        }

        if let Some(network) = &self.network {
            if network.server_ip.trim().is_empty() {
                bail!("network.server_ip is required");
            }

            if network.server_port == 0 {
                bail!("network.server_port must be greater than 0");
            }
        }

        if let Some(control) = &self.control {
            if control.request_timeout_sec == 0 {
                bail!("control.request_timeout_sec must be greater than 0");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_installer_config() {
        let config: NodeConfig = toml::from_str(
            r#"
            [identity]
            identity_file = "/var/lib/xaccel-node/identity.json"

            [runtime]
            data_dir = "/var/lib/xaccel-node"
            log_dir = "/var/log/xaccel-node"
            health_addr = "127.0.0.1:9876"
            channel = "stable"

            [bootstrap]
            response_file = "/var/lib/xaccel-node/bootstrap-response.json"

            [control]
            enabled = false
            "#,
        )
        .expect("config parses");

        config.validate().expect("config validates");
    }
}
