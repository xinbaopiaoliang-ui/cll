use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::{IpAddr, SocketAddr},
    path::Path,
};

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
    pub listen_ip: Option<String>,
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

impl NetworkConfig {
    pub fn listen_host(&self) -> &str {
        self.listen_ip
            .as_deref()
            .map(str::trim)
            .filter(|listen_ip| !listen_ip.is_empty())
            .unwrap_or_else(|| self.server_ip.trim())
    }

    pub fn listen_endpoint(&self) -> String {
        socket_endpoint(self.listen_host(), self.server_port)
    }
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

            if let Some(listen_ip) = &network.listen_ip {
                let listen_ip = listen_ip.trim();
                if listen_ip.is_empty() {
                    bail!("network.listen_ip must not be empty when provided");
                }

                listen_ip.parse::<IpAddr>().with_context(|| {
                    format!("network.listen_ip must be an IP address: {listen_ip}")
                })?;
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

fn socket_endpoint(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
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

    #[test]
    fn network_listen_endpoint_prefers_listen_ip() {
        let network = NetworkConfig {
            server_ip: "47.83.160.126".to_string(),
            listen_ip: Some("0.0.0.0".to_string()),
            server_port: 666,
            relay_server_ip: None,
            relay_server_port: None,
            is_support_ipv6: false,
            disable_quic: false,
            area: "UNKNOWN".to_string(),
            bandwidth_quality: BandwidthQuality::Normal,
            tag: None,
            operator_ips: None,
        };

        assert_eq!(network.listen_endpoint(), "0.0.0.0:666");
    }

    #[test]
    fn network_listen_endpoint_falls_back_to_server_ip() {
        let network = NetworkConfig {
            server_ip: "103.201.131.99".to_string(),
            listen_ip: None,
            server_port: 666,
            relay_server_ip: None,
            relay_server_port: None,
            is_support_ipv6: false,
            disable_quic: false,
            area: "UNKNOWN".to_string(),
            bandwidth_quality: BandwidthQuality::Normal,
            tag: None,
            operator_ips: None,
        };

        assert_eq!(network.listen_endpoint(), "103.201.131.99:666");
    }
}
